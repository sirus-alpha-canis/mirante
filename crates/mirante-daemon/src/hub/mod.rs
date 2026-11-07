//! The hub: all daemon state behind one mutex. Every public method locks, mutates through
//! the `Store` (which persists each transition with its audit event), then publishes the
//! new events to subscribers. Submodules add the session lifecycle (`session`) and
//! messaging / permissions (`messages`).
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use mirante_core::model::{AgentKind, Event, Lifecycle, NewTask, Status, Task};
use mirante_core::status::StatusEngine;
use mirante_core::store::Store;
use mirante_core::worktree::WorktreeManager;
use mirante_core::CoreError;
use mirante_ipc::rpc::{code, RpcError};
use mirante_ipc::types::{BootState, CreateTask, DaemonStatus, Snapshot, TaskView};
use mirante_ipc::Paths;
use serde_json::{json, Value};

use crate::driver::{Driver, DriverError, DriverEvent, DriverFactory};

pub(crate) mod evidence;
pub mod memory;
pub mod messages;
pub mod session;

pub type Result<T> = std::result::Result<T, HubError>;
pub type EventRx = mpsc::Receiver<(String, DriverEvent)>;

#[derive(Debug)]
pub enum HubError {
    Core(CoreError),
    Driver(DriverError),
    NotFound(String),
    Conflict(String),
    /// Refused because a dialog waits for the user.
    NeedsYou(String),
    Invalid(String),
    /// Mirante did not type this Claude answer (inline answers are off, or the evidence
    /// that the dialog on screen is this card's is missing): the user answers it in the
    /// agent's terminal. `task` is how to name the task (`mirante attach <task>`).
    AnswerInTerminal {
        task: String,
        reason: String,
    },
}

impl fmt::Display for HubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HubError::Core(e) => write!(f, "{e}"),
            HubError::Driver(e) => write!(f, "{e}"),
            HubError::NotFound(m) | HubError::Conflict(m) | HubError::NeedsYou(m) | HubError::Invalid(m) => {
                write!(f, "{m}")
            }
            HubError::AnswerInTerminal { task, reason } => {
                write!(f, "answer this dialog in the terminal (mirante attach {task}): {reason}; nothing was typed")
            }
        }
    }
}

impl std::error::Error for HubError {}

impl From<CoreError> for HubError {
    fn from(e: CoreError) -> Self {
        HubError::Core(e)
    }
}

impl From<DriverError> for HubError {
    fn from(e: DriverError) -> Self {
        HubError::Driver(e)
    }
}

impl From<HubError> for RpcError {
    fn from(e: HubError) -> Self {
        let c = match &e {
            HubError::Core(CoreError::NotFound { .. }) | HubError::NotFound(_) => code::NOT_FOUND,
            HubError::Core(CoreError::InvalidBranch { .. }) | HubError::Invalid(_) => code::INVALID_PARAMS,
            HubError::Core(
                CoreError::TaskBusy(_)
                | CoreError::InvalidTransition { .. }
                | CoreError::StaleStatus { .. }
                | CoreError::SessionEnded(_)
                | CoreError::Git { .. }
                | CoreError::DirtyWorktree { .. }
                | CoreError::OutsideRoot { .. },
            )
            | HubError::Conflict(_)
            | HubError::Driver(DriverError::NoDialog(_) | DriverError::NotNow(_) | DriverError::NativeGone(_)) => {
                code::CONFLICT
            }
            HubError::Driver(DriverError::Unsupported(_)) => code::UNSUPPORTED,
            HubError::Driver(DriverError::Failed(_)) | HubError::Core(CoreError::ClaudeNotFound(_)) => code::DRIVER,
            HubError::NeedsYou(_) => code::NEEDS_YOU,
            HubError::AnswerInTerminal { .. } => code::ANSWER_IN_TERMINAL,
            HubError::Core(_) => code::INTERNAL,
        };
        RpcError::new(c, e.to_string())
    }
}

/// One live session: the agent process (through its driver) plus the state the hub keeps
/// about it. Removed when the session ends.
pub(crate) struct Live {
    pub(crate) session_id: String,
    pub(crate) agent: AgentKind,
    /// `None` only while the driver is being spawned.
    pub(crate) driver: Option<Arc<dyn Driver>>,
    /// Claude's fused status (hooks + transcript). Unused for Codex, whose driver runs its
    /// own engine and reports `DriverEvent::Status`.
    pub(crate) engine: StatusEngine,
    pub(crate) turn_id: Option<String>,
    pub(crate) native_id: Option<String>,
    /// Launched on an existing native conversation (`--resume` / `thread/resume`). Not the
    /// same as `native_id` being known: a fresh Claude learns its id at launch.
    pub(crate) resumed: bool,
    pub(crate) transcript_path: Option<String>,
    pub(crate) boot: BootState,
    pub(crate) detail: Option<String>,
    /// Messages held until the running turn ends (front = next).
    pub(crate) queue: VecDeque<String>,
    /// Where the next handed-back message goes: after the ones handed back before it (so a
    /// run of them keeps its original order), ahead of everything queued since. Reset when
    /// the queue is next delivered from.
    pub(crate) requeue_at: usize,
    /// Permission id -> driver key (Codex request id).
    pub(crate) perm_keys: HashMap<String, String>,
    /// Permission id -> the Claude dialog it shows (to settle exactly that card when its
    /// tool runs).
    pub(crate) dialogs: HashMap<String, DialogKey>,
    /// Permission ids whose answer is being typed right now (the hub lock is released
    /// meanwhile): a second answer to the same card waits for it, then looks again.
    pub(crate) answering: HashSet<String>,
    /// A message was handed to the idle agent and the status has not left `Idle` since:
    /// nothing else is delivered meanwhile (it would land in the turn it starts). Expires
    /// after [`DELIVERY_GRACE`] in case the message started no turn (a local command).
    pub(crate) delivery_in_flight: Option<Instant>,
    /// Claude messages typed mid-turn, waiting for the transcript to say steered or queued.
    pub(crate) steers: Vec<String>,
    /// Recent compacted main-chain transcript lines (for steer/queue classification).
    pub(crate) transcript: Vec<Value>,
    /// The driver's hold flag ([`crate::driver::Sink::held`]): set while this session has a
    /// pending permission request, so the driver types nothing.
    pub(crate) hold: Arc<AtomicBool>,
    /// Claude: the transcript's recent tool calls (the evidence a card's dialog is the one
    /// waiting; see [`evidence`]).
    pub(crate) ledger: evidence::ToolLedger,
    /// Claude: bumped under the hub lock by everything that may change which dialog is on
    /// screen (any applied hook, a card opened or settled, a tool call or result in the
    /// transcript). An answer captures it and the typist compares it right before the key.
    pub(crate) card_gen: Arc<AtomicU64>,
    /// Codex quota / token usage, as the driver last reported it.
    pub(crate) usage: Option<String>,
    /// Codex: the effective sandbox, as the driver last reported it (`net:off`).
    pub(crate) sandbox: Option<String>,
    /// Since when the status is `Idle` (the memory guard's idle hibernation and leak check).
    pub(crate) idle_since: Option<Instant>,
    /// This session's `SessionStart` hook was seen (told to the driver once it is attached,
    /// when the hook beat the spawn).
    pub(crate) saw_session_start: bool,
    /// Why Mirante is ending this session (`mirante:stopped`, `mirante:hibernated`,
    /// `mirante:daemon_shutdown`), set before the driver is told to stop: the process's
    /// exit, which the pump thread may apply before the caller ends the session itself,
    /// then records this cause instead of `mirante:process_exited`.
    pub(crate) ending: Option<&'static str>,
}

/// Events queued per subscriber. One that falls this far behind is dropped (its stream
/// ends after the queued events) and resumes with `events.subscribe { since_seq }`.
pub const SUB_QUEUE: usize = 1024;
/// Largest gap `events.subscribe { since_seq }` replays; beyond it a full snapshot is sent.
pub const REPLAY_MAX: u64 = 10_000;

/// How a subscription starts: the snapshot (or, resumed, its stand-in), the missed events
/// to send before the live ones, and the live channel.
pub struct Subscription {
    pub snapshot: Snapshot,
    pub replay: Vec<Event>,
    pub events: mpsc::Receiver<Event>,
}

/// How long a delivery to an idle agent blocks the next one if no turn shows up.
pub(crate) const DELIVERY_GRACE: Duration = Duration::from_secs(30);

/// Who raised a Claude permission dialog and about which tool call (`tool_use_id`, from the
/// payload or borrowed from the matching `PreToolUse`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DialogKey {
    pub(crate) agent: Option<String>,
    pub(crate) tool_use_id: Option<String>,
    pub(crate) tool_name: Option<String>,
    /// The hook's `tool_input.description` (Bash shows it in the dialog box below the
    /// command); the driver matches it too.
    pub(crate) description: Option<String>,
    /// The hook-loss marker when the card was created ([`mirante_ipc::hookloss`]). Not part
    /// of the dialog's identity (see [`DialogKey::same_dialog`]).
    pub(crate) hook_lost: mirante_ipc::hookloss::Fingerprint,
}

impl DialogKey {
    /// The same dialog (agent, tool call, tool, description), whatever the marker said.
    pub(crate) fn same_dialog(&self, o: &DialogKey) -> bool {
        (&self.agent, &self.tool_use_id, &self.tool_name, &self.description)
            == (&o.agent, &o.tool_use_id, &o.tool_name, &o.description)
    }

    /// Is `ev` (a `PostToolUse`) this dialog's tool finishing? By `tool_use_id` when both
    /// sides have one, else by agent identity (as the status engine pairs them).
    pub(crate) fn answered_by(&self, ev: &mirante_core::hook::HookEvent) -> bool {
        match (&self.tool_use_id, &ev.tool_use_id) {
            (Some(a), Some(b)) => a == b,
            _ => self.agent == ev.agent_id,
        }
    }
}

impl Live {
    /// A delivery to the idle agent is still expected to start a turn.
    pub(crate) fn delivering(&self) -> bool {
        self.delivery_in_flight.is_some_and(|t| t.elapsed() < DELIVERY_GRACE)
    }

    pub(crate) fn new(session_id: &str, agent: AgentKind, native_id: Option<String>) -> Live {
        Live {
            session_id: session_id.to_string(),
            agent,
            driver: None,
            engine: StatusEngine::new(),
            turn_id: None,
            resumed: native_id.is_some(),
            native_id,
            transcript_path: None,
            boot: BootState::Booting,
            detail: None,
            queue: VecDeque::new(),
            requeue_at: 0,
            perm_keys: HashMap::new(),
            dialogs: HashMap::new(),
            answering: HashSet::new(),
            delivery_in_flight: None,
            steers: Vec::new(),
            transcript: Vec::new(),
            hold: Arc::default(),
            ledger: evidence::ToolLedger::default(),
            card_gen: Arc::default(),
            usage: None,
            sandbox: None,
            idle_since: Some(Instant::now()),
            saw_session_start: false,
            ending: None,
        }
    }
}

pub(crate) struct State {
    pub(crate) store: Store,
    pub(crate) live: HashMap<String, Live>,
    pub(crate) last_seq: u64,
    /// Bounded ([`SUB_QUEUE`]): a subscriber that falls behind is dropped, not buffered.
    pub(crate) subscribers: Vec<mpsc::SyncSender<Event>>,
    /// Recently applied hook `event_id`s (a hook may deliver and also spool one event).
    pub(crate) seen_hooks: session::SeenHooks,
    /// The memory guard's last view, pause flag and hibernated tasks.
    pub(crate) mem: memory::MemShared,
    /// The daemon's home (the hook-loss marker lives there).
    pub(crate) paths: Paths,
}

pub struct Hub {
    pub(crate) state: Mutex<State>,
    pub(crate) paths: Paths,
    pub(crate) worktrees: WorktreeManager,
    pub(crate) factory: Box<dyn DriverFactory>,
    pub(crate) events: mpsc::Sender<(String, DriverEvent)>,
    pub(crate) boot_timeout: Duration,
    /// Mirante types Claude permission answers (off by default; see
    /// [`Hub::set_claude_inline_answers`]).
    pub(crate) claude_inline: AtomicBool,
}

/// Highest event seq in the store (0 when empty).
fn last_seq(store: &Store) -> Result<u64> {
    let mut seq = 0;
    loop {
        let page = store.events_since(seq, 10_000)?;
        match page.last() {
            Some(e) => seq = e.seq,
            None => return Ok(seq),
        }
    }
}

/// First line of the goal, at most 60 characters.
pub fn title_from_goal(goal: &str) -> String {
    let line = goal.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("untitled");
    if line.chars().count() <= 60 {
        return line.to_string();
    }
    let cut: String = line.chars().take(59).collect();
    format!("{}…", cut.trim_end())
}

impl State {
    /// A task by full id, by branch (unique among non-archived tasks), or by a unique id
    /// suffix of at least 4 characters.
    pub(crate) fn resolve(&self, reference: &str) -> Result<Task> {
        let r = reference.trim();
        if r.is_empty() {
            return Err(HubError::Invalid("empty task reference".into()));
        }
        if let Ok(t) = self.store.get_task(r) {
            return Ok(t);
        }
        let tasks = self.store.list_tasks(true)?;
        let by_branch: Vec<&Task> =
            tasks.iter().filter(|t| t.lifecycle != Lifecycle::Archived && t.branch == r).collect();
        if by_branch.len() == 1 {
            return Ok(by_branch[0].clone());
        }
        if by_branch.len() > 1 {
            return Err(HubError::Conflict(format!("branch {r} names several tasks; use the task id")));
        }
        if r.len() >= 4 {
            let by_suffix: Vec<&Task> = tasks.iter().filter(|t| t.id.ends_with(r)).collect();
            match by_suffix.len() {
                1 => return Ok(by_suffix[0].clone()),
                0 => {}
                _ => return Err(HubError::Conflict(format!("id suffix {r} is ambiguous"))),
            }
        }
        Err(HubError::NotFound(format!("no task {r}")))
    }

    pub(crate) fn view(&self, task: Task) -> Result<TaskView> {
        let pending = self.store.pending_permissions(&task.id)?.len();
        let live = self.live.get(&task.id);
        let trust = usize::from(live.is_some_and(|l| l.boot == BootState::TrustPrompt));
        Ok(TaskView {
            live: live.is_some(),
            boot: live.map(|l| l.boot),
            needs_you: pending + trust,
            queued: live.map_or(0, |l| l.queue.len()),
            detail: live.and_then(|l| l.detail.clone()),
            usage: live.and_then(|l| l.usage.clone()),
            sandbox: live.and_then(|l| l.sandbox.clone()),
            memory: live.and(self.mem.sessions.get(&task.id)).map(memory::mem_view),
            hibernated: self.mem.hibernated.contains(&task.id),
            task,
        })
    }

    pub(crate) fn views(&self, archived: bool) -> Result<Vec<TaskView>> {
        self.store.list_tasks(archived)?.into_iter().map(|t| self.view(t)).collect()
    }

    /// Persists `to` as the task status unless it already is, or the move is not allowed
    /// (e.g. `Ended -> Working` from a late event).
    pub(crate) fn set_status(&mut self, task_id: &str, to: Status, cause: &str) -> Result<()> {
        let cur = self.store.get_task(task_id)?.status;
        if cur == to || !cur.can_transition_to(to) {
            return Ok(());
        }
        self.store.set_status(task_id, cur, to, cause)?;
        if let Some(l) = self.live.get_mut(task_id) {
            if cur == Status::Idle {
                l.delivery_in_flight = None;
            }
            l.idle_since = (to == Status::Idle).then(Instant::now);
        }
        Ok(())
    }

    /// Logs an error that must not fail the operation it happened in (the user-visible
    /// action already took effect): stderr plus a `daemon_error` event, best effort.
    pub(crate) fn log_error(&mut self, task_id: Option<&str>, context: &str, e: &dyn fmt::Display) {
        eprintln!("mirante-daemon: {context}: {e}");
        let _ = self.event(task_id, "daemon_error", json!({ "context": context, "error": e.to_string() }));
    }

    pub(crate) fn event(&mut self, task_id: Option<&str>, kind: &str, payload: Value) -> Result<()> {
        self.store.append_event(task_id, kind, &payload)?;
        Ok(())
    }

    /// Sets each live session's hold flag: held while it has a pending permission request
    /// (typed text could answer that dialog).
    pub(crate) fn sync_holds(&mut self) {
        let State { store, live, .. } = self;
        for (task_id, l) in live.iter() {
            let held = store
                .pending_permissions(task_id)
                .map(|p| p.iter().any(|p| p.session_id == l.session_id))
                .unwrap_or(true);
            l.hold.store(held, Ordering::SeqCst);
        }
    }

    /// Sends every event newer than `last_seq` to the subscribers, dropping closed ones.
    /// Every hub mutation ends here, so the hold flags are brought up to date first.
    pub(crate) fn publish(&mut self) {
        self.sync_holds();
        loop {
            let Ok(events) = self.store.events_since(self.last_seq, 1000) else {
                return;
            };
            let Some(last) = events.last() else {
                return;
            };
            self.last_seq = last.seq;
            // Never blocks under the hub lock: a full queue drops that subscriber.
            self.subscribers.retain(|s| events.iter().all(|e| s.try_send(e.clone()).is_ok()));
        }
    }

    pub(crate) fn task_of_session(&self, session_id: &str) -> Option<String> {
        self.live.iter().find(|(_, l)| l.session_id == session_id).map(|(t, _)| t.clone())
    }
}

impl Hub {
    /// The hub and the receiving end of the driver-event channel (give it to
    /// [`Hub::spawn_pump`], or drain it with [`Hub::drain`] in tests).
    pub fn new(store: Store, paths: Paths, factory: Box<dyn DriverFactory>) -> Result<(Hub, EventRx)> {
        let (tx, rx) = mpsc::channel();
        let last_seq = last_seq(&store)?;
        let hub = Hub {
            state: Mutex::new(State {
                store,
                live: HashMap::new(),
                last_seq,
                subscribers: Vec::new(),
                seen_hooks: session::SeenHooks::default(),
                mem: memory::MemShared::default(),
                paths: paths.clone(),
            }),
            worktrees: WorktreeManager::new(&paths.worktrees()),
            paths,
            factory,
            events: tx,
            boot_timeout: Duration::from_secs(45),
            claude_inline: AtomicBool::new(false),
        };
        Ok((hub, rx))
    }

    /// How long `session.start` / `session.trust` wait for the agent to leave `Booting`.
    pub fn set_boot_timeout(&mut self, timeout: Duration) {
        self.boot_timeout = timeout;
    }

    /// Claude inline answers: when on, `permission.answer` on a Claude card types the
    /// dialog's option key, but only with all the evidence `Hub::answer_permission` lists.
    /// Off (the default; `MIRANTE_CLAUDE_INLINE_ANSWERS=1` turns it on): Claude cards are
    /// answered in the terminal and `permission.answer` says so (`ANSWER_IN_TERMINAL`).
    pub fn set_claude_inline_answers(&self, on: bool) {
        self.claude_inline.store(on, Ordering::SeqCst);
    }

    pub fn claude_inline_answers(&self) -> bool {
        self.claude_inline.load(Ordering::SeqCst)
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn create_task(&self, p: CreateTask) -> Result<TaskView> {
        if p.goal.trim().is_empty() {
            return Err(HubError::Invalid("the goal is empty".into()));
        }
        let repo = std::fs::canonicalize(&p.repo).map_err(|e| HubError::Invalid(format!("repo {}: {e}", p.repo)))?;
        // Worktree first, then the task row (rolled back below if the row fails). A crash in
        // between leaves a checkout no task owns: recovery reports it as an orphan and never
        // deletes it (it may hold the user's branch). The other order would need a row
        // state for "worktree not made yet" and deleting rows the audit trail names.
        // git is slow: no hub lock held while the worktree is created.
        let wt = self.worktrees.create(&repo, &p.branch, p.base.as_deref())?;
        let title = p.title.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| title_from_goal(&p.goal));
        let new = NewTask {
            title,
            repo_path: repo.to_string_lossy().into_owned(),
            worktree_path: wt.path.to_string_lossy().into_owned(),
            branch: p.branch,
            agent: p.agent,
            goal: p.goal,
        };
        let mut st = self.lock();
        match st.store.create_task(new) {
            Ok(task) => {
                let v = st.view(task)?;
                st.publish();
                Ok(v)
            }
            Err(e) => {
                drop(st);
                // Removes the worktree, and its branch only if we made it and it is unchanged.
                if let Err(undo) = self.worktrees.discard(&repo, &wt) {
                    self.lock().log_error(None, "task.create rollback", &undo);
                }
                Err(e.into())
            }
        }
    }

    pub fn list_tasks(&self, archived: bool) -> Result<Vec<TaskView>> {
        self.lock().views(archived)
    }

    pub fn get_task(&self, reference: &str) -> Result<TaskView> {
        let st = self.lock();
        let t = st.resolve(reference)?;
        st.view(t)
    }

    /// Removes the worktree (git refuses a dirty one) and archives the task. The branch is
    /// kept. Refused while a session is live.
    ///
    /// Crash-safe: `Archiving` is persisted before the worktree is touched (no session may
    /// start meanwhile), then `Archived` after. A removal that fails puts the task back to
    /// `Active` (`archive_failed` says why); a crash in between is finished or undone by
    /// [`Hub::recover`].
    pub fn archive(&self, reference: &str) -> Result<TaskView> {
        let task = {
            let mut st = self.lock();
            let t = st.resolve(reference)?;
            if st.live.contains_key(&t.id) {
                return Err(HubError::Conflict(format!("task {} has a live session: stop it first", t.branch)));
            }
            match t.lifecycle {
                Lifecycle::Archiving => {
                    return Err(HubError::Conflict(format!("task {} is already being archived", t.branch)))
                }
                Lifecycle::Archived => {
                    return Err(HubError::Conflict(format!("task {} is already archived", t.branch)))
                }
                Lifecycle::Created | Lifecycle::Active => {}
            }
            let t = st.store.transition_lifecycle(&t.id, Lifecycle::Archiving)?;
            st.publish();
            t
        };
        let removed = self.worktrees.remove(Path::new(&task.repo_path), Path::new(&task.worktree_path));
        let mut st = self.lock();
        if let Err(e) = removed {
            let undo = st.store.transition_lifecycle(&task.id, Lifecycle::Active);
            let _ = st.event(Some(&task.id), "archive_failed", json!({ "error": e.to_string() }));
            if let Err(u) = undo {
                st.log_error(Some(&task.id), "archive rollback", &u);
            }
            st.publish();
            return Err(e.into());
        }
        let t = st.store.transition_lifecycle(&task.id, Lifecycle::Archived)?;
        let v = st.view(t)?;
        st.publish();
        Ok(v)
    }

    /// The current snapshot plus a channel of every event after it.
    pub fn subscribe(&self) -> Result<(Snapshot, mpsc::Receiver<Event>)> {
        let s = self.subscribe_from(None, None)?;
        Ok((s.snapshot, s.events))
    }

    /// Like [`Hub::subscribe`], but resumes after `since_seq` when `boot_id` is this boot's
    /// and the gap is at most [`REPLAY_MAX`]: the snapshot is then `resumed` (no tasks,
    /// `seq = since_seq`) and `replay` holds the missed events. Otherwise a full snapshot.
    pub fn subscribe_from(&self, since_seq: Option<u64>, boot_id: Option<&str>) -> Result<Subscription> {
        let mut st = self.lock();
        st.publish();
        let (tx, rx) = mpsc::sync_channel(SUB_QUEUE);
        let boot = st.store.boot_id().to_string();
        let resume = since_seq
            .filter(|s| boot_id == Some(boot.as_str()) && *s <= st.last_seq)
            .filter(|s| st.last_seq - s <= REPLAY_MAX);
        let (snapshot, replay) = match resume {
            Some(since) => {
                let mut replay = Vec::new();
                let mut after = since;
                while after < st.last_seq {
                    let page = st.store.events_since(after, 1000)?;
                    let Some(last) = page.last() else {
                        break;
                    };
                    after = last.seq;
                    replay.extend(page.into_iter().filter(|e| e.seq <= st.last_seq));
                }
                (Snapshot { boot_id: boot, seq: since, tasks: Vec::new(), resumed: true }, replay)
            }
            None => (Snapshot { boot_id: boot, seq: st.last_seq, tasks: st.views(false)?, resumed: false }, Vec::new()),
        };
        st.subscribers.push(tx);
        Ok(Subscription { snapshot, replay, events: rx })
    }

    pub fn status(&self) -> DaemonStatus {
        let st = self.lock();
        DaemonStatus {
            pid: std::process::id(),
            boot_id: st.store.boot_id().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            live_sessions: st.live.len(),
            memory: st.mem.system.clone(),
            claude_inline_answers: self.claude_inline_answers(),
        }
    }
}

#[cfg(test)]
pub(crate) mod testkit {
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Arc;

    use mirante_core::model::AgentKind;
    use mirante_core::store::Store;
    use mirante_ipc::test_dir::TestDir;
    use mirante_ipc::types::{CreateTask, TaskView};
    use mirante_ipc::Paths;

    use super::{EventRx, Hub};
    use crate::driver::fake::FakeFactory;

    pub struct Kit {
        pub hub: Arc<Hub>,
        pub rx: EventRx,
        pub factory: Arc<FakeFactory>,
        pub repo: PathBuf,
        pub dir: TestDir,
    }

    /// A git repo whose only branch is `trunk`, with one commit.
    pub fn git_repo(root: &std::path::Path) -> PathBuf {
        let repo = root.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| {
            let ok = Command::new("git").arg("-C").arg(&repo).args(args).output().unwrap().status.success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q", "-b", "trunk"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "T"]);
        std::fs::write(repo.join("README"), "hi\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
        repo
    }

    impl Kit {
        pub fn new() -> Kit {
            Kit::with_store(|_| Store::open_in_memory().unwrap())
        }

        pub fn with_store(open: impl FnOnce(&Paths) -> Store) -> Kit {
            let dir = TestDir::new();
            let paths = Paths::new(&dir.path().join("home"));
            paths.ensure_home().unwrap();
            let repo = git_repo(dir.path());
            let factory = Arc::new(FakeFactory::default());
            let (mut hub, rx) = Hub::new(open(&paths), paths, Box::new(factory.clone())).unwrap();
            hub.set_boot_timeout(std::time::Duration::ZERO);
            Kit { hub: Arc::new(hub), rx, factory, repo, dir }
        }

        pub fn create(&self, agent: AgentKind, branch: &str) -> TaskView {
            self.hub
                .create_task(CreateTask {
                    repo: self.repo.to_string_lossy().into_owned(),
                    agent,
                    branch: branch.into(),
                    base: None,
                    goal: format!("goal of {branch}"),
                    title: None,
                })
                .unwrap()
        }

        pub fn kinds(&self, task_id: &str) -> Vec<String> {
            self.hub.lock().store.task_events(task_id).unwrap().into_iter().map(|e| e.kind).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::Kit;
    use super::*;
    use mirante_ipc::types::CreateTask;

    #[test]
    fn create_makes_a_worktree_on_the_users_branch_and_a_task() {
        let k = Kit::new();
        let v = k.create(AgentKind::Claude, "feat/login");
        assert_eq!(v.task.branch, "feat/login");
        assert_eq!(v.task.title, "goal of feat/login");
        assert!(Path::new(&v.task.worktree_path).join("README").exists());
        assert!(v.task.worktree_path.ends_with("worktrees/app/feat/login"), "{}", v.task.worktree_path);
        assert!(!v.live);
        assert_eq!(v.task.status, Status::Idle);
    }

    #[test]
    fn create_rejects_an_empty_goal_and_rolls_back_the_worktree_on_failure() {
        let k = Kit::new();
        let bad = CreateTask {
            repo: k.repo.to_string_lossy().into_owned(),
            agent: AgentKind::Codex,
            branch: "b1".into(),
            base: None,
            goal: "  ".into(),
            title: None,
        };
        assert!(matches!(k.hub.create_task(bad), Err(HubError::Invalid(_))));
        let missing = CreateTask {
            repo: "/definitely/not/here".into(),
            agent: AgentKind::Codex,
            branch: "b1".into(),
            base: None,
            goal: "g".into(),
            title: None,
        };
        assert!(matches!(k.hub.create_task(missing), Err(HubError::Invalid(_))));
    }

    #[test]
    fn tasks_resolve_by_id_branch_or_unique_suffix() {
        let k = Kit::new();
        let a = k.create(AgentKind::Claude, "alpha").task;
        let b = k.create(AgentKind::Codex, "beta").task;
        assert_eq!(k.hub.get_task(&a.id).unwrap().task.id, a.id);
        assert_eq!(k.hub.get_task("beta").unwrap().task.id, b.id);
        assert_eq!(k.hub.get_task(&b.id[b.id.len() - 6..]).unwrap().task.id, b.id);
        assert!(matches!(k.hub.get_task("nope"), Err(HubError::NotFound(_))));
        assert!(matches!(k.hub.get_task(""), Err(HubError::Invalid(_))));
        assert_eq!(k.hub.list_tasks(false).unwrap().len(), 2);
    }

    #[test]
    fn archive_removes_the_worktree_keeps_the_branch_and_hides_the_task() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "gone").task;
        let v = k.hub.archive("gone").unwrap();
        assert_eq!(v.task.lifecycle, Lifecycle::Archived);
        assert!(!Path::new(&t.worktree_path).exists());
        let branches = std::process::Command::new("git").arg("-C").arg(&k.repo).args(["branch"]).output().unwrap();
        assert!(String::from_utf8_lossy(&branches.stdout).contains("gone"));
        assert!(k.hub.list_tasks(false).unwrap().is_empty());
        assert_eq!(k.hub.list_tasks(true).unwrap().len(), 1);
        assert!(matches!(k.hub.archive(&t.id), Err(HubError::Conflict(_))));
    }

    fn lifecycle_moves(k: &Kit, task: &str) -> Vec<String> {
        k.hub
            .lock()
            .store
            .task_events(task)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "lifecycle")
            .map(|e| e.payload["to"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn archive_persists_archiving_before_removing_the_worktree() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "gone").task;
        k.hub.archive("gone").unwrap();
        assert_eq!(lifecycle_moves(&k, &t.id), vec!["archiving", "archived"]);
    }

    #[test]
    fn a_failed_archive_puts_the_task_back_and_says_why() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "dirty").task;
        std::fs::write(Path::new(&t.worktree_path).join("wip.txt"), "unsaved\n").unwrap();
        let err = k.hub.archive("dirty").unwrap_err();
        let rpc: RpcError = err.into();
        assert_eq!(rpc.code, code::CONFLICT);
        assert!(rpc.message.contains("uncommitted changes") && rpc.message.contains("?? wip.txt"), "{}", rpc.message);
        let v = k.hub.get_task(&t.id).unwrap();
        assert_eq!(v.task.lifecycle, Lifecycle::Active);
        assert!(Path::new(&t.worktree_path).join("wip.txt").exists());
        assert_eq!(lifecycle_moves(&k, &t.id), vec!["archiving", "active"]);
        assert!(k.kinds(&t.id).contains(&"archive_failed".to_string()));
        // A session may start again.
        k.hub.start_session(mirante_ipc::types::StartSession { task: t.id.clone(), prompt: None }, false).unwrap();
    }

    #[test]
    fn subscribers_get_every_event_after_the_snapshot() {
        let k = Kit::new();
        k.create(AgentKind::Claude, "one");
        let (snap, rx) = k.hub.subscribe().unwrap();
        assert_eq!(snap.tasks.len(), 1);
        k.create(AgentKind::Claude, "two");
        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(ev.kind, "task_created");
        assert_eq!(ev.seq, snap.seq + 1);
        assert_eq!(ev.boot_id, snap.boot_id);
    }

    #[test]
    fn a_subscriber_that_never_reads_is_bounded_then_dropped() {
        let k = Kit::new();
        let (_snap, rx) = k.hub.subscribe().unwrap();
        {
            let mut st = k.hub.lock();
            for i in 0..SUB_QUEUE + 50 {
                st.event(None, "noise", json!({ "i": i })).unwrap();
            }
            st.publish();
            assert!(st.subscribers.is_empty(), "the stalled subscriber must be dropped");
        }
        // Exactly the queue bound was buffered; then the stream ends (the client resumes).
        assert_eq!(rx.iter().count(), SUB_QUEUE);
    }

    #[test]
    fn subscribe_resumes_from_since_seq_on_the_same_boot_only() {
        let k = Kit::new();
        let (snap, _rx) = k.hub.subscribe().unwrap();
        k.create(AgentKind::Claude, "one");
        k.create(AgentKind::Claude, "two");
        let s = k.hub.subscribe_from(Some(snap.seq), Some(&snap.boot_id)).unwrap();
        assert!(s.snapshot.resumed && s.snapshot.tasks.is_empty());
        assert_eq!(s.snapshot.seq, snap.seq);
        let kinds: Vec<&str> = s.replay.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, ["task_created", "task_created"]);
        assert_eq!(s.replay[0].seq, snap.seq + 1);
        // Another boot, or a seq from the future: a full snapshot.
        let other = k.hub.subscribe_from(Some(snap.seq), Some("old-boot")).unwrap();
        assert!(!other.snapshot.resumed && other.replay.is_empty());
        assert_eq!(other.snapshot.tasks.len(), 2);
        let ahead = k.hub.subscribe_from(Some(snap.seq + 999), Some(&snap.boot_id)).unwrap();
        assert!(!ahead.snapshot.resumed);
    }

    #[test]
    fn title_is_the_first_goal_line_capped() {
        assert_eq!(title_from_goal("\n  fix the login bug \nmore"), "fix the login bug");
        let long = "x".repeat(80);
        let t = title_from_goal(&long);
        assert_eq!(t.chars().count(), 60);
        assert!(t.ends_with('…'));
        assert_eq!(title_from_goal(""), "untitled");
    }

    #[test]
    fn errors_map_to_typed_rpc_codes() {
        let e: RpcError = HubError::Core(CoreError::TaskBusy("t".into())).into();
        assert_eq!(e.code, code::CONFLICT);
        let e: RpcError = HubError::Driver(DriverError::Unsupported("trust")).into();
        assert_eq!(e.code, code::UNSUPPORTED);
        let e: RpcError = HubError::NeedsYou("dialog".into()).into();
        assert_eq!(e.code, code::NEEDS_YOU);
        let e: RpcError = HubError::Core(CoreError::NotFound { kind: "task", id: "x".into() }).into();
        assert_eq!(e.code, code::NOT_FOUND);
        let e: RpcError = HubError::Driver(DriverError::NotNow("no running turn".into())).into();
        assert_eq!((e.code, e.message.as_str()), (code::CONFLICT, "no running turn"));
        let e: RpcError = HubError::Driver(DriverError::NativeGone("no rollout".into())).into();
        assert_eq!(e.code, code::CONFLICT);
        assert!(e.message.contains("conversation is gone"), "{}", e.message);
    }
}
