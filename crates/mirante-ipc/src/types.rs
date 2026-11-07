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
    pub const MESSAGE_SEND: &str = "message.send";
    pub const PERMISSION_LIST: &str = "permission.list";
    pub const PERMISSION_ANSWER: &str = "permission.answer";
    pub const EVENTS_SUBSCRIBE: &str = "events.subscribe";
    pub const ATTACH: &str = "attach";
    pub const HOOK_EVENT: &str = "hook.event";
    pub const DAEMON_STATUS: &str = "daemon.status";
    pub const DAEMON_SHUTDOWN: &str = "daemon.shutdown";
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
pub struct HookParams {
    pub task: String,
    /// The hook's stdin, verbatim (parsed by the daemon so errors are reported, not lost).
    pub payload: String,
    pub ts_ms: i64,
}

/// First response of `events.subscribe`; `event` notifications with `seq > seq` follow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub boot_id: String,
    pub seq: u64,
    pub tasks: Vec<TaskView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub boot_id: String,
    pub version: String,
    pub live_sessions: usize,
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
        let v = TaskView { task, live: true, boot: Some(BootState::Ready), needs_you: 1, queued: 0, detail: None };
        let j = serde_json::to_value(&v).unwrap();
        assert_eq!(j["id"], "t1");
        assert_eq!(j["status"], "needs_you");
        assert_eq!(j["boot"], "ready");
        assert_eq!(serde_json::from_value::<TaskView>(j).unwrap(), v);
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
