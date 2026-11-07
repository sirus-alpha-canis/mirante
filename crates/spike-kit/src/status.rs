use serde::Serialize;

use crate::hook::{HookEvent, LoggedEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Idle,
    Working,
    NeedsYou,
    Ended,
}

/// Text of the Notification Claude Code sends after ~60s without input (2.1.285:
/// "Claude is waiting for your input"). The agent is idle, it does not need you.
pub const IDLE_NOTIFICATION: &str = "waiting for your input";

/// Deterministic mapping from one hook event to the task status. Unknown or
/// neutral events (SubagentStop, PreCompact, anything new) keep the current status.
pub fn next_status(current: Option<Status>, ev: &HookEvent) -> Option<Status> {
    match ev.hook_event_name.as_str() {
        "SessionStart" => Some(Status::Idle),
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some(Status::Working),
        "Notification" if ev.message.as_deref().is_some_and(|m| m.contains(IDLE_NOTIFICATION)) => {
            Some(Status::Idle)
        }
        "PermissionRequest" | "Notification" => Some(Status::NeedsYou),
        "Stop" => Some(Status::Idle),
        "SessionEnd" => Some(Status::Ended),
        _ => current,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub ts_ms: u64,
    pub event: String,
    pub status: Status,
}

/// Only the events that changed the status.
pub fn timeline(events: &[LoggedEvent]) -> Vec<Transition> {
    let mut current = None;
    let mut out = Vec::new();
    for e in events {
        let next = next_status(current, &e.event);
        if next != current {
            if let Some(status) = next {
                out.push(Transition { ts_ms: e.ts_ms, event: e.event.hook_event_name.clone(), status });
            }
            current = next;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(name: &str) -> HookEvent {
        HookEvent {
            hook_event_name: name.into(),
            session_id: None,
            transcript_path: None,
            source: None,
            message: None,
            tool_name: None,
        }
    }

    fn notification(message: &str) -> HookEvent {
        HookEvent { message: Some(message.into()), ..ev("Notification") }
    }

    #[test]
    fn idle_notification_maps_to_idle() {
        let n = notification("Claude is waiting for your input");
        assert_eq!(next_status(Some(Status::Idle), &n), Some(Status::Idle));
        assert_eq!(next_status(Some(Status::Working), &n), Some(Status::Idle));
    }

    #[test]
    fn other_notifications_map_to_needs_you() {
        let n = notification("Claude needs your permission to use Bash");
        assert_eq!(next_status(Some(Status::Working), &n), Some(Status::NeedsYou));
        assert_eq!(next_status(Some(Status::Working), &ev("Notification")), Some(Status::NeedsYou));
    }

    fn logged(ts: u64, name: &str) -> LoggedEvent {
        LoggedEvent { ts_ms: ts, task_id: None, event: ev(name) }
    }

    #[test]
    fn maps_each_known_event() {
        assert_eq!(next_status(None, &ev("SessionStart")), Some(Status::Idle));
        assert_eq!(next_status(None, &ev("UserPromptSubmit")), Some(Status::Working));
        assert_eq!(next_status(None, &ev("PreToolUse")), Some(Status::Working));
        assert_eq!(next_status(None, &ev("PostToolUse")), Some(Status::Working));
        assert_eq!(next_status(None, &ev("PermissionRequest")), Some(Status::NeedsYou));
        assert_eq!(next_status(None, &ev("Notification")), Some(Status::NeedsYou));
        assert_eq!(next_status(None, &ev("Stop")), Some(Status::Idle));
        assert_eq!(next_status(None, &ev("SessionEnd")), Some(Status::Ended));
    }

    #[test]
    fn neutral_and_unknown_events_keep_current_status() {
        assert_eq!(next_status(Some(Status::Working), &ev("SubagentStop")), Some(Status::Working));
        assert_eq!(next_status(Some(Status::Working), &ev("PreCompact")), Some(Status::Working));
        assert_eq!(next_status(Some(Status::Idle), &ev("SomethingNew")), Some(Status::Idle));
    }

    #[test]
    fn timeline_keeps_only_changes() {
        let events = vec![
            logged(1, "SessionStart"),
            logged(2, "UserPromptSubmit"),
            logged(3, "PreToolUse"),
            logged(4, "PostToolUse"),
            logged(5, "Notification"),
            logged(6, "PostToolUse"),
            logged(7, "Stop"),
            logged(8, "SessionEnd"),
        ];
        let t: Vec<(u64, Status)> = timeline(&events).iter().map(|t| (t.ts_ms, t.status)).collect();
        assert_eq!(
            t,
            vec![
                (1, Status::Idle),
                (2, Status::Working),
                (5, Status::NeedsYou),
                (6, Status::Working),
                (7, Status::Idle),
                (8, Status::Ended),
            ]
        );
    }
}
