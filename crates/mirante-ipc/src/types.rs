//! Method names and their parameter / result types. A task may be referenced by its full
//! id, its branch name, or a unique suffix of its id (at least 4 characters).
use mirante_core::model::{AgentKind, PermissionRequest, Task};
use serde::{Deserialize, Serialize};

pub mod method {
    pub const TASK_CREATE: &str = "task.create";
    pub const TASK_LIST: &str = "task.list";
    pub const TASK_GET: &str = "task.get";
    pub const TASK_ARCHIVE: &str = "task.archive";
    pub const SESSION_START: &str = "session.start";
    pub const SESSION_RESUME: &str = "session.resume";
    pub const SESSION_TRUST: &str = "session.trust";
    pub const SESSION_STOP: &str = "session.stop";
    pub const SESSION_INTERRUPT: &str = "session.interrupt";
    /// The attached terminal changed size ([`super::ResizeParams`]).
    pub const SESSION_RESIZE: &str = "session.resize";
    pub const MESSAGE_SEND: &str = "message.send";
    pub const PERMISSION_LIST: &str = "permission.list";
    pub const PERMISSION_ANSWER: &str = "permission.answer";
    pub const EVENTS_SUBSCRIBE: &str = "events.subscribe";
    pub const ATTACH: &str = "attach";
    pub const HOOK_EVENT: &str = "hook.event";
    pub const DAEMON_STATUS: &str = "daemon.status";
    pub const DAEMON_SHUTDOWN: &str = "daemon.shutdown";
    /// Ends an idle session to free memory, keeping it resumable ([`super::TaskRef`]).
    pub const SESSION_HIBERNATE: &str = "session.hibernate";
    /// Resumes a hibernated session ([`super::TaskRef`]; result: [`super::StartResult`]).
    pub const SESSION_WAKE: &str = "session.wake";
    /// Orphan processes / checkouts and memory ([`super::DoctorReport`]).
    pub const DAEMON_DOCTOR: &str = "daemon.doctor";
    /// Kills one orphan the user picked from `daemon.doctor` ([`super::KillOrphan`]).
    pub const DAEMON_KILL_ORPHAN: &str = "daemon.kill_orphan";
    /// Notification carrying one `mirante_core::model::Event`.
    pub const EVENT: &str = "event";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendMode {
    /// Deliver now: typed into Claude's TUI (absorbed at the next tool boundary) or
    /// `turn/steer` for Codex. Starts a turn when none is running.
    Steer,
    /// Hold in the daemon and deliver when the running turn ends.
    Queue,
}

/// Where a live session's agent process is in its startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootState {
    Booting,
    /// Claude's workspace-trust dialog is on screen and waits for the user's decision.
    TrustPrompt,
    Ready,
    Exited,
}

/// A task as clients see it: the stored task plus live-session facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskView {
    #[serde(flatten)]
    pub task: Task,
    pub live: bool,
    pub boot: Option<BootState>,
    /// Pending permission requests, plus one while a trust prompt waits.
    pub needs_you: usize,
    /// Messages held for delivery at the end of the running turn.
    pub queued: usize,
    /// Extra status detail, e.g. "turn done, 1 command still finishing".
    pub detail: Option<String>,
    /// Codex quota / token usage while live, e.g. "5h 42% · week 9% · 31k tokens".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<String>,
    /// Codex: the effective sandbox while live, e.g. "net:off" or "net:on, +1 root".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    /// Memory of the live session's process tree, once sampled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemView>,
    /// The session was hibernated to free memory (`mirante wake` resumes it).
    #[serde(default)]
    pub hibernated: bool,
}

/// One live session's memory: the summed footprint of its process tree (macOS:
/// `phys_footprint`, what Activity Monitor shows), its peak, its budget and where it stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemView {
    pub bytes: u64,
    pub peak: u64,
    pub budget: u64,
    /// "ok", "soft" (past the soft share of the budget) or "hard" (past the budget).
    pub state: String,
}

/// The machine's memory as the daemon's guard sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemView {
    /// "normal", "elevated" or "critical".
    pub level: String,
    pub total: u64,
    pub free: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    /// New sessions and queued deliveries wait while memory is tight.
    pub paused: bool,
}

/// A process left behind by an agent: parent gone (ppid 1), working directory or command
/// line inside Mirante's worktrees. Shown by `mirante doctor`; killed only on request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanView {
    pub pid: i32,
    /// Start time; with the pid it names exactly this process.
    pub start: u64,
    pub cwd: Option<String>,
    pub argv: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    pub orphan_processes: Vec<OrphanView>,
    /// Checkouts under the worktree root that no task owns (never deleted by Mirante).
    pub orphan_worktrees: Vec<String>,
    pub memory: Option<SystemView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillOrphan {
    pub pid: i32,
    pub start: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRef {
    pub task: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListParams {
    #[serde(default)]
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateTask {
    pub repo: String,
    pub agent: AgentKind,
    pub branch: String,
    #[serde(default)]
    pub base: Option<String>,
    pub goal: String,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartSession {
    pub task: String,
    /// First message, delivered as soon as the agent is ready.
    #[serde(default)]
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartResult {
    pub task: String,
    pub boot: BootState,
    /// True when the agent's native session / thread was resumed (context restored).
    pub resumed: bool,
    /// Last lines of the agent's screen (Claude) for the trust prompt / exit reason.
    pub screen: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustAnswer {
    pub task: String,
    pub accept: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendMessage {
    pub task: String,
    pub text: String,
    pub mode: SendMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendResult {
    /// What actually happened: `Queue` when held, `Steer` when handed to the agent now.
    pub delivered: SendMode,
    /// Queue length after this call.
    pub queued: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionView {
    #[serde(flatten)]
    pub request: PermissionRequest,
    pub task_title: String,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerPermission {
    /// Full id or a unique suffix (at least 4 characters).
    pub id: String,
    pub allow: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachParams {
    pub task: String,
    pub rows: u16,
    pub cols: u16,
}

/// After this response the connection carries raw bytes in both directions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachResult {
    /// False for a read-only stream (Codex): typed bytes are ignored.
    pub input: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResizeParams {
    pub task: String,
    pub rows: u16,
    pub cols: u16,
}

/// `events.subscribe` parameters. With `since_seq` and the `boot_id` of the snapshot or
/// event it came from, a reconnecting client resumes: the daemon replays the events after
/// `since_seq` (as `event` notifications) instead of sending a full snapshot, when it
/// still can (same boot, gap not too large). Otherwise a full snapshot is sent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribeParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookParams {
    pub task: String,
    /// The hook's stdin, verbatim (parsed by the daemon so errors are reported, not lost).
    pub payload: String,
    pub ts_ms: i64,
}

/// What `mirante-hook` actually puts on the wire and in the spool: [`HookParams`] plus
/// optional fields (flattened, so a plain `HookParams` reader ignores them and old spool
/// lines without them still parse).
///
/// The daemon's `hook.event` handling and spool ingest MUST dedupe by `event_id`: a hook
/// that timed out waiting for the ack also spools the event, so the daemon may see it
/// twice. Spool replays can also be older than live events already applied (the spool is
/// ingested at daemon start); order by `received_ts_ms`/`ts_ms`, not by arrival.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookEnvelope {
    #[serde(flatten)]
    pub params: HookParams,
    /// `<ts_ms>-<pid>-<counter>`, unique per hook invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    /// When the hook process received the payload (same clock as `ts_ms`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_ts_ms: Option<i64>,
    /// The hook's stdin was cut at the size cap; `payload` is a stub, not Claude's JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    /// The Mirante session id the hook ran under (`MIRANTE_SESSION_ID` in Claude's env),
    /// so the daemon can fence events from an older session of the same task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// First response of `events.subscribe`; `event` notifications with `seq > seq` follow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub boot_id: String,
    pub seq: u64,
    /// Empty when `resumed`.
    pub tasks: Vec<TaskView>,
    /// The subscription resumed from the requested `since_seq`: `seq` is that value, `tasks`
    /// is empty, and the missed events follow as notifications. The client keeps its state.
    #[serde(default)]
    pub resumed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub boot_id: String,
    pub version: String,
    pub live_sessions: usize,
    /// The memory guard's view (absent from older daemons and before the first sample).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<SystemView>,
    /// Mirante types Claude permission answers itself (`MIRANTE_CLAUDE_INLINE_ANSWERS=1`).
    /// Off (the default): Claude cards are answered in the terminal (`mirante attach`).
    #[serde(default)]
    pub claude_inline_answers: bool,
}
#[cfg(test)]
mod tests {
    use super::*;
    use mirante_core::model::{Lifecycle, Status};
    use serde_json::json;

    #[test]
    fn task_view_flattens_the_task() {
        let task = Task {
            id: "t1".into(),
            title: "T".into(),
            repo_path: "/r".into(),
            worktree_path: "/w".into(),
            branch: "feat".into(),
            agent: AgentKind::Codex,
            goal: "g".into(),
            notes: String::new(),
            lifecycle: Lifecycle::Active,
            status: Status::NeedsYou,
            created_at: 1,
            updated_at: 2,
        };
        let v = TaskView {
            task,
            live: true,
            boot: Some(BootState::Ready),
            needs_you: 1,
            queued: 0,
            detail: None,
            usage: None,
            sandbox: Some("net:off".into()),
            memory: Some(MemView { bytes: 1, peak: 2, budget: 3, state: "ok".into() }),
            hibernated: false,
        };
        let j = serde_json::to_value(&v).unwrap();
        assert_eq!(j["id"], "t1");
        assert_eq!(j["status"], "needs_you");
        assert_eq!(j["boot"], "ready");
        assert_eq!(serde_json::from_value::<TaskView>(j).unwrap(), v);
    }

    #[test]
    fn additive_fields_are_optional_on_the_wire() {
        let s: SubscribeParams = serde_json::from_value(json!({})).unwrap();
        assert_eq!(s, SubscribeParams::default());
        assert_eq!(serde_json::to_value(&s).unwrap(), json!({}));
        let snap: Snapshot = serde_json::from_value(json!({"boot_id": "b", "seq": 3, "tasks": []})).unwrap();
        assert!(!snap.resumed);
        let e: HookEnvelope = serde_json::from_value(json!({"task": "t", "payload": "{}", "ts_ms": 1})).unwrap();
        assert_eq!(e.session, None);
        assert!(serde_json::to_value(&e).unwrap().get("session").is_none());
        let r: ResizeParams = serde_json::from_value(json!({"task": "t", "rows": 30, "cols": 100})).unwrap();
        assert_eq!((r.rows, r.cols), (30, 100));
        let d: DaemonStatus =
            serde_json::from_value(json!({"pid": 1, "boot_id": "b", "version": "0.1.0", "live_sessions": 0})).unwrap();
        assert_eq!(d.memory, None);
    }

    #[test]
    fn optional_fields_default() {
        let c: CreateTask =
            serde_json::from_value(json!({"repo": "/r", "agent": "claude", "branch": "b", "goal": "g"})).unwrap();
        assert_eq!((c.base, c.title), (None, None));
        let s: StartSession = serde_json::from_value(json!({"task": "t"})).unwrap();
        assert_eq!(s.prompt, None);
        let l: ListParams = serde_json::from_value(json!({})).unwrap();
        assert!(!l.archived);
        let m: SendMessage = serde_json::from_value(json!({"task": "t", "text": "x", "mode": "queue"})).unwrap();
        assert_eq!(m.mode, SendMode::Queue);
    }
}
