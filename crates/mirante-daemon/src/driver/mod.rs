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
use std::sync::{mpsc, Arc};

use mirante_core::model::{Status, Task};
use mirante_ipc::types::BootState;
use mirante_ipc::Paths;
use serde_json::Value;

#[cfg(test)]
pub mod fake;
pub mod real;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverError {
    /// The agent cannot do this (e.g. a trust prompt on Codex).
    Unsupported(&'static str),
    /// The dialog the operation targets is not on screen / not pending.
    NoDialog(String),
    Failed(String),
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DriverError::Unsupported(what) => write!(f, "not supported by this agent: {what}"),
            DriverError::NoDialog(m) => write!(f, "no matching dialog: {m}"),
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
    /// Codex: the thread id, used for `thread/resume`.
    NativeId(String),
    Boot(BootState),
    /// A step for the feed and audit trail (command finished, agent message, ...).
    Step(Value),
    Exited {
        code: Option<i32>,
    },
}

/// Channel end a driver emits into. Every event is tagged with the driver's session id so
/// late events from an ended session are ignored.
#[derive(Clone)]
pub struct Sink {
    session_id: String,
    tx: mpsc::Sender<(String, DriverEvent)>,
}

impl Sink {
    pub fn new(session_id: &str, tx: mpsc::Sender<(String, DriverEvent)>) -> Sink {
        Sink { session_id: session_id.to_string(), tx }
    }

    pub fn emit(&self, event: DriverEvent) {
        let _ = self.tx.send((self.session_id.clone(), event));
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

/// Fan-out of output bytes to attached clients; dead receivers are dropped on send.
#[derive(Default)]
pub struct Fanout {
    next: u64,
    subs: HashMap<u64, mpsc::Sender<Vec<u8>>>,
}

impl Fanout {
    pub fn subscribe(&mut self) -> (u64, mpsc::Receiver<Vec<u8>>) {
        self.next += 1;
        let (tx, rx) = mpsc::channel();
        self.subs.insert(self.next, tx);
        (self.next, rx)
    }

    pub fn remove(&mut self, id: u64) {
        self.subs.remove(&id);
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.subs.retain(|_, tx| tx.send(bytes.to_vec()).is_ok());
    }

    pub fn is_empty(&self) -> bool {
        self.subs.is_empty()
    }
}

pub trait Driver: Send + Sync {
    /// Delivers a message now: types it into the TUI (Claude) or steers / starts a turn
    /// (Codex). Returns once the message is handed to the driver's worker.
    fn send(&self, text: &str) -> Result<(), DriverError>;
    /// Esc (Claude) or `turn/interrupt` (Codex).
    fn interrupt(&self) -> Result<(), DriverError>;
    /// Answers the permission dialog / approval request named by `key` (Codex: the request
    /// id; Claude: the request detail, checked against the dialog on screen). Selects only
    /// a one-time "Yes" or "No", never an "always" or "auto" option.
    fn answer_permission(&self, key: &str, allow: bool) -> Result<(), DriverError>;
    /// Answers Claude's workspace-trust dialog. `false` ends the session.
    fn trust(&self, accept: bool) -> Result<(), DriverError>;
    /// True while a dialog would capture typed text (a message must not be typed then).
    fn dialog_open(&self) -> bool;
    /// Claude: start (or switch) tailing the session transcript at `offset`.
    fn tail_transcript(&self, path: &Path, offset: u64);
    fn attach(&self, rows: u16, cols: u16) -> Result<Attachment, DriverError>;
    fn input(&self, attachment: u64, bytes: &[u8]) -> Result<(), DriverError>;
    fn detach(&self, attachment: u64);
    /// Last lines of the screen (Claude) or stream (Codex), for the CLI.
    fn screen_tail(&self) -> String;
    /// Ends the agent gracefully, then kills it. Blocks until the process is gone; the
    /// driver still emits `Exited`.
    fn stop(&self);
}

pub struct SpawnRequest {
    pub task: Task,
    pub session_id: String,
    /// Native id to resume (Claude session id / Codex thread id).
    pub resume: Option<String>,
    pub paths: Paths,
}

pub trait DriverFactory: Send + Sync {
    fn spawn(&self, req: SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError>;
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
    fn sink_tags_events_with_the_session() {
        let (tx, rx) = mpsc::channel();
        Sink::new("s1", tx).emit(DriverEvent::Boot(BootState::Ready));
        assert_eq!(rx.recv().unwrap(), ("s1".to_string(), DriverEvent::Boot(BootState::Ready)));
    }
}
