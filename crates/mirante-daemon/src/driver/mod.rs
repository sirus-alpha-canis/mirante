//! The agent driver interface. Every agent sits behind [`Driver`]; an operation the agent
//! cannot do is a typed [`DriverError::Unsupported`], never a silent failure. Drivers are
//! non-blocking (typing and protocol calls run on the driver's own threads) except
//! [`Driver::stop`], so the hub may call them while holding its lock.
//!
//! Drivers report back through a [`Sink`] (a channel drained by the hub's pump thread),
//! never by calling the hub directly, so a driver thread can never deadlock on the hub.
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use mirante_core::model::{Status, Task};
use mirante_ipc::types::BootState;
use mirante_ipc::Paths;
use serde_json::Value;

pub mod claude;
pub mod codex;
pub mod codex_rpc;
pub mod codex_status;
#[cfg(test)]
pub mod fake;
pub mod paste;
pub mod proctree;
pub mod pty;
pub mod real;
pub mod screen;
pub mod skill_bundle;
pub mod termq;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverError {
    /// The agent cannot do this (e.g. a trust prompt on Codex).
    Unsupported(&'static str),
    /// The dialog the operation targets is not on screen / not pending.
    NoDialog(String),
    /// The operation does not apply now (e.g. an interrupt with no running turn).
    NotNow(String),
    /// The native conversation to resume is gone (Codex: no rollout for the thread). The
    /// user may start a fresh conversation; Mirante never does so on its own.
    NativeGone(String),
    Failed(String),
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DriverError::Unsupported(what) => write!(f, "not supported by this agent: {what}"),
            DriverError::NoDialog(m) => write!(f, "no matching dialog: {m}"),
            DriverError::NotNow(m) => write!(f, "{m}"),
            DriverError::NativeGone(m) => {
                write!(f, "the agent's conversation is gone ({m}); `mirante resume <task> --fresh` starts a new one")
            }
            DriverError::Failed(m) => write!(f, "agent driver failed: {m}"),
        }
    }
}

impl std::error::Error for DriverError {}

impl From<std::io::Error> for DriverError {
    fn from(e: std::io::Error) -> Self {
        DriverError::Failed(e.to_string())
    }
}

/// Most characters of one agent message kept in its feed step (the cockpit shows the last
/// one as the agent's answer).
pub const AGENT_MESSAGE_CAP: usize = 4000;
/// Characters kept of an agent text on the way (not the answer that ends the turn): the
/// feed shows a glimpse; only final answers are stored in full (up to the cap above).
pub const AGENT_MESSAGE_PARTIAL_CAP: usize = 500;

/// What a driver tells the hub.
#[derive(Debug, Clone, PartialEq)]
pub enum DriverEvent {
    /// Claude: one main-transcript line, for the hub's `StatusEngine`.
    Transcript(Value),
    /// Claude: where the transcript tailer is (persisted, not audited).
    TranscriptOffset {
        path: String,
        offset: u64,
    },
    /// Codex: a status computed by the driver's own engine. `turn_open` drives the turn
    /// records; `detail` is shown next to the status.
    Status {
        to: Status,
        cause: String,
        turn_open: bool,
        detail: Option<String>,
    },
    /// Codex: an approval request. `key` is what `answer_permission` gets back.
    PermissionRequested {
        key: String,
        tool: String,
        detail: String,
    },
    /// Codex: request `key` no longer waits for an answer (the server resolved it, its turn
    /// ended, or the process exited). The hub settles its card if it is still pending.
    PermissionResolved {
        key: String,
    },
    /// Codex: the thread id, used for `thread/resume`.
    NativeId(String),
    Boot(BootState),
    /// A message handed to `send` was not typed after all (a dialog appeared, a permission
    /// became pending): the hub puts it back at the front of the queue.
    DeliveryAborted {
        text: String,
        reason: String,
    },
    /// Claude: typing is no longer blocked (a dialog closed, or the hold was released): the
    /// hub delivers what it requeued.
    DialogClosed,
    /// Resuming the native conversation failed (Claude exited right away). The hub records
    /// `resume_failed`; a fresh start is the user's call (`mirante resume --fresh`).
    ResumeFailed {
        reason: String,
    },
    /// Codex: quota / token usage, one line for `mirante ls`.
    Usage(String),
    /// Codex: the effective sandbox in a few characters (`net:off`, `net:on, +1 root`), for
    /// `mirante ls`'s MODE column.
    Sandbox(String),
    /// A step for the feed and audit trail (command finished, agent message, ...).
    Step(Value),
    Exited {
        code: Option<i32>,
    },
}

/// Channel end a driver emits into. Every event is tagged with the driver's session id so
/// late events from an ended session are ignored. The sink also carries the hub's hold flag
/// for the session (see [`Sink::held`]).
#[derive(Clone)]
pub struct Sink {
    session_id: String,
    tx: mpsc::Sender<(String, DriverEvent)>,
    hold: Arc<AtomicBool>,
}

impl Sink {
    pub fn new(session_id: &str, tx: mpsc::Sender<(String, DriverEvent)>) -> Sink {
        Sink { session_id: session_id.to_string(), tx, hold: Arc::default() }
    }

    pub fn emit(&self, event: DriverEvent) {
        let _ = self.tx.send((self.session_id.clone(), event));
    }

    /// True while the hub has a pending permission request for this session: typed text
    /// could answer that dialog, so nothing may be typed.
    pub fn held(&self) -> bool {
        self.hold.load(Ordering::SeqCst)
    }

    /// The flag behind [`Sink::held`]; the hub keeps a clone and sets it.
    pub fn hold_flag(&self) -> Arc<AtomicBool> {
        self.hold.clone()
    }
}

/// A live view of the agent's terminal (Claude) or event stream (Codex).
pub struct Attachment {
    pub id: u64,
    /// Bytes that repaint the current screen (or recent stream) before live output.
    pub initial: Vec<u8>,
    pub output: mpsc::Receiver<Vec<u8>>,
    /// Whether typed bytes reach the agent (`Driver::input`).
    pub input: bool,
}

/// Output chunks (each at most one 8 KiB read) queued per attached client: at most
/// 256 KiB. A client that falls this far behind stops getting chunks until it is sent a
/// repaint (memory stays bounded whatever it does); the repaint carries the whole screen,
/// so nothing it missed is lost from view.
pub const ATTACH_QUEUE: usize = 32;
/// After a repaint that could not be queued (the client is still stalled), the next try
/// waits this long: a repaint renders the whole screen.
pub const REPAINT_RETRY: Duration = Duration::from_millis(50);

struct FanoutSub {
    tx: mpsc::SyncSender<Vec<u8>>,
    /// Its queue overflowed: chunks were dropped, so live bytes would corrupt its screen.
    /// Nothing more is sent until [`Fanout::repaint`] delivers a full screen.
    needs_repaint: bool,
}

/// Fan-out of output bytes to attached clients over bounded queues; dead receivers are
/// dropped on send. The owner of the screen calls [`Fanout::repaint`] whenever
/// [`Fanout::needs_repaint`] is true (e.g. right after `send`).
pub struct Fanout {
    next: u64,
    cap: usize,
    subs: HashMap<u64, FanoutSub>,
    /// When a repaint last failed to reach a stalled client (its queue still full).
    repaint_failed_at: Option<Instant>,
}

impl Default for Fanout {
    fn default() -> Self {
        Fanout::with_capacity(ATTACH_QUEUE)
    }
}

impl Fanout {
    /// `cap` chunks queued per client (at least 1).
    pub fn with_capacity(cap: usize) -> Fanout {
        Fanout { next: 0, cap: cap.max(1), subs: HashMap::new(), repaint_failed_at: None }
    }

    pub fn subscribe(&mut self) -> (u64, mpsc::Receiver<Vec<u8>>) {
        self.next += 1;
        let (tx, rx) = mpsc::sync_channel(self.cap);
        self.subs.insert(self.next, FanoutSub { tx, needs_repaint: false });
        (self.next, rx)
    }

    pub fn remove(&mut self, id: u64) {
        self.subs.remove(&id);
    }

    /// Never blocks: a full queue marks that client for a repaint and drops the chunk.
    pub fn send(&mut self, bytes: &[u8]) {
        self.subs.retain(|_, s| {
            if s.needs_repaint {
                return true;
            }
            match s.tx.try_send(bytes.to_vec()) {
                Ok(()) => true,
                Err(mpsc::TrySendError::Full(_)) => {
                    s.needs_repaint = true;
                    true
                }
                Err(mpsc::TrySendError::Disconnected(_)) => false,
            }
        });
    }

    /// Some client dropped chunks and waits for a full screen, and a repaint is due: never
    /// tried, or the last one could not be queued at least [`REPAINT_RETRY`] ago (a stalled
    /// client does not cost a screen render per chunk).
    pub fn needs_repaint(&self) -> bool {
        self.subs.values().any(|s| s.needs_repaint)
            && self.repaint_failed_at.is_none_or(|t| t.elapsed() >= REPAINT_RETRY)
    }

    /// Sends `screen` (bytes that redraw the whole screen) to every client marked by
    /// [`Fanout::send`] whose queue has room again, and resumes live bytes for them.
    pub fn repaint(&mut self, screen: &[u8]) {
        self.subs.retain(|_, s| {
            if !s.needs_repaint {
                return true;
            }
            match s.tx.try_send(screen.to_vec()) {
                Ok(()) => {
                    s.needs_repaint = false;
                    true
                }
                Err(mpsc::TrySendError::Full(_)) => true,
                Err(mpsc::TrySendError::Disconnected(_)) => false,
            }
        });
        let stalled = self.subs.values().any(|s| s.needs_repaint);
        self.repaint_failed_at = stalled.then(Instant::now);
    }

    pub fn is_empty(&self) -> bool {
        self.subs.is_empty()
    }
}

/// The hub's evidence for a Claude answer, re-checked by the typist immediately before it
/// types the answer key (the card generation it captured under its lock, the hook-loss
/// marker): `Err(why)` and nothing is typed.
pub type AnswerGuard = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

pub trait Driver: Send + Sync {
    /// Delivers a message now: types it into the TUI (Claude) or steers / starts a turn
    /// (Codex). Returns once the message is handed to the driver's worker.
    fn send(&self, text: &str) -> Result<(), DriverError>;
    /// Esc (Claude; refused with `NotNow` while a permission is pending or a dialog is open,
    /// where Esc would answer it: checked when asked AND by the typist right before the key,
    /// so Claude waits for the typist) or `turn/interrupt` (Codex).
    fn interrupt(&self) -> Result<(), DriverError>;
    /// Answers the permission dialog / approval request named by `key` (Codex: the request
    /// id; Claude: the request detail, checked with `tool` and the hook's `description`, when
    /// it had one, against the dialog on screen). Selects only a one-time "Yes" or "No",
    /// never an "always" or "auto" option. May wait briefly (Claude: until the typist typed
    /// the key or refused to); the hub does not hold its lock meanwhile. `guard` (Claude:
    /// required) is called right before the key is typed; when it fails nothing is typed.
    /// Codex ignores it (its answers go to a request id).
    fn answer_permission(
        &self,
        key: &str,
        tool: &str,
        description: Option<&str>,
        allow: bool,
        guard: Option<AnswerGuard>,
    ) -> Result<(), DriverError>;
    /// Answers Claude's workspace-trust dialog. `false` ends the session.
    fn trust(&self, accept: bool) -> Result<(), DriverError>;
    /// True while a dialog would capture typed text (a message must not be typed then).
    fn dialog_open(&self) -> bool;
    /// Claude: start (or switch) tailing the session transcript at `offset`.
    fn tail_transcript(&self, path: &Path, offset: u64);
    fn attach(&self, rows: u16, cols: u16) -> Result<Attachment, DriverError>;
    fn input(&self, attachment: u64, bytes: &[u8]) -> Result<(), DriverError>;
    fn detach(&self, attachment: u64);
    /// The attached terminal changed size (Claude: resize the PTY; the agent redraws).
    fn resize(&self, _rows: u16, _cols: u16) -> Result<(), DriverError> {
        Err(DriverError::Unsupported("resize"))
    }
    /// Last lines of the screen (Claude) or stream (Codex), for the CLI.
    fn screen_tail(&self) -> String;
    /// Ends the agent and every process below it (SIGTERM to the tree, SIGKILL after a
    /// grace period; see [`proctree::kill_tree`]). Blocks until they are gone; the driver
    /// still emits `Exited`.
    fn stop(&self);
    /// Claude: the hub saw this session's `SessionStart` hook (one of the readiness
    /// conditions). Must not block.
    fn session_started(&self) {}
    /// The agent's root process (memory sampling, orphan checks), when there is one.
    fn root(&self) -> Option<proctree::Proc> {
        None
    }
    /// The device of the agent's terminal ([`proctree::controlling_tty`]), when it runs on
    /// a PTY: a process whose controlling terminal it is was started from the agent's
    /// session.
    fn tty(&self) -> Option<u64> {
        None
    }
}

/// The skills a new session gets, verified: their bytes come from
/// `mirante_core::skills::library::read_verified` and are the only copy a driver may write
/// out (never the version folder itself, which could change after the check).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InjectedSkills {
    pub always: Vec<mirante_core::skills::library::VerifiedSkill>,
    pub on_demand: Vec<mirante_core::skills::library::VerifiedSkill>,
}

pub struct SpawnRequest {
    pub task: Task,
    pub session_id: String,
    /// Native id to resume (Claude session id / Codex thread id).
    pub resume: Option<String>,
    pub paths: Paths,
    /// The `mirante-mcp` binary to give this session as its memory server (Claude:
    /// `--mcp-config`, Codex: `-c mcp_servers.mirante-memory.*`). Set only for repos whose memory
    /// was explicitly enabled; `None` launches the agent exactly as before.
    pub mcp: Option<std::path::PathBuf>,
    /// The `mirante-mcp` binary to give this session as its `mirante-skills` server
    /// (`skill_propose`): set only when the task's client has skill learning on and its
    /// origin is confirmed.
    pub skills_mcp: Option<std::path::PathBuf>,
    /// The verified skills whose scope matches the task's origin (see
    /// `Hub::skills_for`); `None` when there are none.
    pub skills: Option<InjectedSkills>,
    /// A resumed Codex thread: the skills its history holds that were retired or changed
    /// since (see `Hub::codex_withdrawals`); the driver tells the agent not to follow them
    /// and removes their files. Empty otherwise.
    pub withdrawn: Vec<String>,
}

pub trait DriverFactory: Send + Sync {
    fn spawn(&self, req: SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError>;

    /// The memory MCP server binary, or why there is none (see [`real::mcp_bin`]).
    fn memory_server(&self) -> Result<std::path::PathBuf, String> {
        real::mcp_bin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fanout_drops_dead_receivers() {
        let mut f = Fanout::default();
        let (a, rx_a) = f.subscribe();
        let (_b, rx_b) = f.subscribe();
        f.send(b"x");
        assert_eq!(rx_a.recv().unwrap(), b"x");
        drop(rx_b);
        f.send(b"y");
        assert_eq!(rx_a.recv().unwrap(), b"y");
        f.remove(a);
        assert!(f.is_empty());
    }

    #[test]
    fn a_stalled_client_queue_is_bounded_and_resumes_after_a_repaint() {
        let mut f = Fanout::with_capacity(2);
        let (_id, rx) = f.subscribe();
        for b in [b"1", b"2", b"3", b"4"] {
            f.send(b);
        }
        assert!(f.needs_repaint());
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![b"1".to_vec(), b"2".to_vec()]);
        // Room again, but live bytes stay held back until the full screen is sent.
        f.send(b"5");
        assert!(rx.try_recv().is_err());
        f.repaint(b"SCREEN");
        assert!(!f.needs_repaint());
        f.send(b"6");
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![b"SCREEN".to_vec(), b"6".to_vec()]);
    }

    /// A repaint costs a whole screen render: a client still stalled is not retried on
    /// every chunk, only once [`REPAINT_RETRY`] passed.
    #[test]
    fn a_stalled_client_is_retried_after_a_pause_not_on_every_chunk() {
        let mut f = Fanout::with_capacity(1);
        let (_id, rx) = f.subscribe();
        f.send(b"1");
        f.send(b"2");
        assert!(f.needs_repaint());
        f.repaint(b"S1");
        assert!(!f.needs_repaint(), "its queue is still full: not retried at once");
        f.send(b"3");
        assert!(!f.needs_repaint());
        std::thread::sleep(REPAINT_RETRY + Duration::from_millis(20));
        assert!(f.needs_repaint());
        assert_eq!(rx.recv().unwrap(), b"1");
        f.repaint(b"S2");
        assert!(!f.needs_repaint());
        assert_eq!(rx.recv().unwrap(), b"S2");
    }

    #[test]
    fn the_per_client_queue_stays_small() {
        assert!(ATTACH_QUEUE <= 32, "{ATTACH_QUEUE} chunks of up to 8 KiB per client");
    }

    #[test]
    fn sink_tags_events_with_the_session() {
        let (tx, rx) = mpsc::channel();
        Sink::new("s1", tx).emit(DriverEvent::Boot(BootState::Ready));
        assert_eq!(rx.recv().unwrap(), ("s1".to_string(), DriverEvent::Boot(BootState::Ready)));
    }

    #[test]
    fn the_hold_flag_is_shared_between_clones() {
        let (tx, _rx) = mpsc::channel();
        let sink = Sink::new("s1", tx);
        let driver_side = sink.clone();
        assert!(!driver_side.held());
        sink.hold_flag().store(true, Ordering::SeqCst);
        assert!(driver_side.held());
    }
}
