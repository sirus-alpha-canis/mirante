use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{CoreError, Result};

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Time-ordered unique id (UUID v7, hyphenated).
pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Generates `as_str`/`parse` for a fieldless enum stored as TEXT in SQLite.
macro_rules! text_enum {
    ($name:ident, $kind:literal, { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }

            pub fn parse(s: &str) -> Result<Self> {
                match s {
                    $($text => Ok($name::$variant),)+
                    other => Err(CoreError::BadEnum { kind: $kind, value: other.to_string() }),
                }
            }
        }
    };
}

/// What the agent of a task is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Idle,
    Working,
    NeedsYou,
    Ended,
}
text_enum!(Status, "status", { Idle => "idle", Working => "working", NeedsYou => "needs_you", Ended => "ended" });

impl Status {
    /// Any live status may move to any other live status or to `Ended`. `Ended` may only
    /// move to `Idle` (a resumed session). Staying in the same status is not a transition.
    pub fn can_transition_to(self, to: Status) -> bool {
        match (self, to) {
            (a, b) if a == b => false,
            (Status::Ended, Status::Idle) => true,
            (Status::Ended, _) => false,
            _ => true,
        }
    }
}

/// Lifecycle of a task, independent of what its agent is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    /// Created, no session started yet.
    Created,
    /// At least one session was started.
    Active,
    /// The worktree is being removed. Persisted before the removal starts, so a crash
    /// mid-archive is finished (or undone) by recovery. No session may start.
    Archiving,
    /// Worktree removed, branch kept. Terminal.
    Archived,
}
text_enum!(Lifecycle, "lifecycle", {
    Created => "created", Active => "active", Archiving => "archiving", Archived => "archived"
});

impl Lifecycle {
    /// `Created -> Active`; `Created | Active -> Archiving -> Archived`; `Archiving -> Active`
    /// when the worktree could not be removed.
    pub fn can_transition_to(self, to: Lifecycle) -> bool {
        matches!(
            (self, to),
            (Lifecycle::Created, Lifecycle::Active)
                | (Lifecycle::Created | Lifecycle::Active, Lifecycle::Archiving)
                | (Lifecycle::Archiving, Lifecycle::Archived | Lifecycle::Active)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Codex,
}
text_enum!(AgentKind, "agent", { Claude => "claude", Codex => "codex" });

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionStatus {
    Pending,
    AllowedOnce,
    /// Allowed, and what it grants lasts for the rest of the session (Codex `grantRoot`).
    AllowedSession,
    Denied,
}
text_enum!(PermissionStatus, "permission_status", {
    Pending => "pending",
    AllowedOnce => "allowed_once",
    AllowedSession => "allowed_session",
    Denied => "denied",
});

impl PermissionStatus {
    /// The user said yes (once or for the session).
    pub fn allowed(self) -> bool {
        matches!(self, PermissionStatus::AllowedOnce | PermissionStatus::AllowedSession)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub repo_path: String,
    pub worktree_path: String,
    pub branch: String,
    pub agent: AgentKind,
    pub goal: String,
    pub notes: String,
    pub lifecycle: Lifecycle,
    pub status: Status,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Input for `Store::create_task`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTask {
    pub title: String,
    pub repo_path: String,
    pub worktree_path: String,
    pub branch: String,
    pub agent: AgentKind,
    pub goal: String,
}

/// The durable agent conversation of a task. `native_session_id` is the agent's own id
/// (Claude's `session_id` from `SessionStart`), used to relaunch with `--resume`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub task_id: String,
    pub native_session_id: Option<String>,
    pub created_at: i64,
}

/// One process activation of a conversation. Live while `ended_at` is `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub task_id: String,
    pub conversation_id: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub transcript_path: Option<String>,
    pub transcript_offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub id: String,
    pub session_id: String,
    /// 1-based position of the turn within its session.
    pub idx: i64,
    pub started_at: i64,
    pub ended_at: Option<i64>,
}

/// Append-only audit record. `seq` is monotonic across boots; `boot_id` names the
/// store opening (daemon boot) that wrote it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub boot_id: String,
    pub task_id: Option<String>,
    pub kind: String,
    pub payload: Value,
    pub ts_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: String,
    pub task_id: String,
    pub session_id: String,
    pub tool: String,
    pub detail: String,
    pub status: PermissionStatus,
    pub created_at: i64,
    pub answered_at: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_transitions() {
        assert!(Status::Idle.can_transition_to(Status::Working));
        assert!(Status::Working.can_transition_to(Status::NeedsYou));
        assert!(Status::NeedsYou.can_transition_to(Status::Idle));
        assert!(Status::Idle.can_transition_to(Status::Ended));
        assert!(Status::Ended.can_transition_to(Status::Idle));
        assert!(!Status::Ended.can_transition_to(Status::Working));
        assert!(!Status::Ended.can_transition_to(Status::NeedsYou));
        assert!(!Status::Working.can_transition_to(Status::Working));
    }

    #[test]
    fn lifecycle_transitions() {
        assert!(Lifecycle::Created.can_transition_to(Lifecycle::Active));
        assert!(!Lifecycle::Active.can_transition_to(Lifecycle::Created));
        assert!(!Lifecycle::Archived.can_transition_to(Lifecycle::Active));
        assert!(!Lifecycle::Active.can_transition_to(Lifecycle::Active));
    }

    #[test]
    fn archiving_sits_between_live_and_archived() {
        // Archived is only reached through Archiving (set before the worktree is removed).
        assert!(Lifecycle::Created.can_transition_to(Lifecycle::Archiving));
        assert!(Lifecycle::Active.can_transition_to(Lifecycle::Archiving));
        assert!(Lifecycle::Archiving.can_transition_to(Lifecycle::Archived));
        // A failed removal puts the task back.
        assert!(Lifecycle::Archiving.can_transition_to(Lifecycle::Active));
        assert!(!Lifecycle::Created.can_transition_to(Lifecycle::Archived));
        assert!(!Lifecycle::Active.can_transition_to(Lifecycle::Archived));
        assert!(!Lifecycle::Archived.can_transition_to(Lifecycle::Archiving));
        assert!(!Lifecycle::Archiving.can_transition_to(Lifecycle::Created));
        assert_eq!(Lifecycle::parse("archiving").unwrap(), Lifecycle::Archiving);
        assert_eq!(Lifecycle::Archiving.as_str(), "archiving");
    }

    #[test]
    fn text_enums_roundtrip_and_reject_unknown() {
        for s in [Status::Idle, Status::Working, Status::NeedsYou, Status::Ended] {
            assert_eq!(Status::parse(s.as_str()).unwrap(), s);
        }
        assert_eq!(PermissionStatus::parse("allowed_once").unwrap(), PermissionStatus::AllowedOnce);
        assert_eq!(PermissionStatus::parse("allowed_session").unwrap(), PermissionStatus::AllowedSession);
        assert!(PermissionStatus::AllowedSession.allowed() && PermissionStatus::AllowedOnce.allowed());
        assert!(!PermissionStatus::Denied.allowed() && !PermissionStatus::Pending.allowed());
        assert_eq!(AgentKind::parse("codex").unwrap(), AgentKind::Codex);
        assert_eq!(Lifecycle::parse("archived").unwrap(), Lifecycle::Archived);
        assert!(matches!(Status::parse("busy"), Err(CoreError::BadEnum { kind: "status", .. })));
    }

    #[test]
    fn ids_are_unique_and_time_ordered() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert!(a < b, "{a} < {b}");
    }
}
