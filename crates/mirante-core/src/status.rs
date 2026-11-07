//! Status fusion: one engine per session, fed hook events and transcript lines in arrival
//! order (transcript lines in file order). Rules come from docs/spikes/FINDINGS.md:
//!
//! - A turn opens only on a main-agent `UserPromptSubmit` hook or a main-chain transcript
//!   prompt line. Tool events never open a turn, so a stray `PreToolUse` after `Stop`
//!   (side agent, S3 mismatch 2) cannot flip the task back to `Working`.
//! - A turn closes on a main-agent `Stop`, the idle `Notification`, or the transcript's
//!   `system` `turn_duration` / `stop_hook_summary` line. The transcript end covers the
//!   deny path, where no hook fires (S3 mismatch 1).
//! - Transcript turns are counted on their own; a transcript turn end closes the fused turn
//!   only when both name the same turn, so a late transcript line from the previous turn
//!   cannot end the turn the hooks already opened.
//! - Side-agent events (`agent_id` present; transcript `isSidechain` / `agentId` /
//!   `parentToolUseID`) never open or close a turn. A side agent's `PermissionRequest`
//!   still means `NeedsYou`: its dialog blocks the session.
use serde_json::Value;

use crate::hook::HookEvent;
use crate::model::Status;

/// Text of the Notification Claude Code sends after ~60s without input (2.1.285:
/// "Claude is waiting for your input"). The agent is idle; it does not need you.
pub const IDLE_NOTIFICATION: &str = "waiting for your input";

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Hook(HookEvent),
    Transcript(Value),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub from: Status,
    pub to: Status,
    /// `hook:<event name>` or `transcript:<line kind>`; stored in the status event.
    pub cause: String,
}

/// Who asked for permission: `None` is the main agent, `Some(id)` a side agent.
type Asker = Option<String>;

#[derive(Debug, Clone)]
pub struct StatusEngine {
    status: Status,
    turn_open: bool,
    /// Number of turns opened (the fused turn counter).
    turn_seq: u64,
    /// Pending permission dialog and who raised it.
    pending: Option<Asker>,
}

impl Default for StatusEngine {
    fn default() -> Self {
        StatusEngine::new()
    }
}

impl StatusEngine {
    pub fn new() -> Self {
        StatusEngine {
            status: Status::Idle,
            turn_open: false,
            turn_seq: 0,
            pending: None,
        }
    }

    pub fn status(&self) -> Status {
        self.status
    }

    pub fn turn_open(&self) -> bool {
        self.turn_open
    }

    /// Applies one input; returns the transition if the status changed.
    pub fn apply(&mut self, input: &Input) -> Option<Transition> {
        if self.status == Status::Ended {
            return None;
        }
        match input {
            Input::Hook(ev) => self.apply_hook(ev),
            // Transcript fusion is added in the next task.
            Input::Transcript(_) => None,
        }
    }

    /// Applies inputs in order and returns only the transitions.
    pub fn apply_all<'a>(&mut self, inputs: impl IntoIterator<Item = &'a Input>) -> Vec<Transition> {
        inputs.into_iter().filter_map(|i| self.apply(i)).collect()
    }

    fn set(&mut self, to: Status, cause: String) -> Option<Transition> {
        if !self.status.can_transition_to(to) {
            return None;
        }
        let from = self.status;
        self.status = to;
        Some(Transition { from, to, cause })
    }

    fn open_turn(&mut self) {
        self.turn_open = true;
        self.turn_seq += 1;
        self.pending = None;
    }

    fn close_turn(&mut self) {
        self.turn_open = false;
        self.pending = None;
    }

    fn apply_hook(&mut self, ev: &HookEvent) -> Option<Transition> {
        let cause = format!("hook:{}", ev.hook_event_name);
        let side = ev.is_side_agent();
        match ev.hook_event_name.as_str() {
            // `SessionStart` also fires after a mid-turn compaction; it only means idle
            // when no turn is open.
            "SessionStart" if !self.turn_open => self.set(Status::Idle, cause),
            "UserPromptSubmit" if !side && !self.turn_open => {
                self.open_turn();
                self.set(Status::Working, cause)
            }
            "PreToolUse" | "PostToolUse" => {
                let asker: Asker = ev.agent_id.clone();
                match &self.pending {
                    // A dialog is open: only the agent that asked can clear it.
                    Some(p) if *p != asker => None,
                    Some(_) => {
                        self.pending = None;
                        let to = if self.turn_open { Status::Working } else { Status::Idle };
                        self.set(to, cause)
                    }
                    None if self.turn_open => self.set(Status::Working, cause),
                    None => None,
                }
            }
            "PermissionRequest" => {
                self.pending = Some(ev.agent_id.clone());
                self.set(Status::NeedsYou, cause)
            }
            "Notification" if ev.message.as_deref().is_some_and(|m| m.contains(IDLE_NOTIFICATION)) => {
                self.close_turn();
                self.set(Status::Idle, cause)
            }
            "Notification" => self.set(Status::NeedsYou, cause),
            "Stop" if !side => {
                self.close_turn();
                self.set(Status::Idle, cause)
            }
            "SessionEnd" => {
                self.close_turn();
                self.set(Status::Ended, cause)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(name: &str) -> Input {
        Input::Hook(HookEvent::named(name))
    }
    fn side(name: &str) -> Input {
        Input::Hook(HookEvent {
            agent_id: Some("a-1".into()),
            agent_type: Some("general-purpose".into()),
            ..HookEvent::named(name)
        })
    }
    fn notification(msg: &str) -> Input {
        Input::Hook(HookEvent { message: Some(msg.into()), ..HookEvent::named("Notification") })
    }
    fn statuses(e: &mut StatusEngine, inputs: &[Input]) -> Vec<Status> {
        e.apply_all(inputs).into_iter().map(|t| t.to).collect()
    }

    #[test]
    fn plain_turn_with_tools() {
        let mut e = StatusEngine::new();
        let got = statuses(
            &mut e,
            &[
                hook("SessionStart"),
                hook("UserPromptSubmit"),
                hook("PreToolUse"),
                hook("PostToolUse"),
                hook("Stop"),
                hook("SessionEnd"),
            ],
        );
        assert_eq!(got, vec![Status::Working, Status::Idle, Status::Ended]);
    }

    #[test]
    fn allowed_permission_goes_needs_you_then_working() {
        let mut e = StatusEngine::new();
        let t = e.apply_all(&[
            hook("UserPromptSubmit"),
            hook("PreToolUse"),
            hook("PermissionRequest"),
            hook("PostToolUse"),
            hook("Stop"),
        ]);
        let got: Vec<(Status, &str)> = t.iter().map(|t| (t.to, t.cause.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (Status::Working, "hook:UserPromptSubmit"),
                (Status::NeedsYou, "hook:PermissionRequest"),
                (Status::Working, "hook:PostToolUse"),
                (Status::Idle, "hook:Stop"),
            ]
        );
    }

    #[test]
    fn idle_notification_is_idle_other_notifications_need_you() {
        let mut e = StatusEngine::new();
        e.apply(&hook("UserPromptSubmit"));
        assert_eq!(e.apply(&notification("Claude needs your permission to use Bash")).unwrap().to, Status::NeedsYou);
        assert_eq!(e.apply(&notification("Claude is waiting for your input")).unwrap().to, Status::Idle);
        assert!(!e.turn_open());
    }

    #[test]
    fn session_start_mid_turn_does_not_reset_to_idle() {
        let mut e = StatusEngine::new();
        e.apply(&hook("UserPromptSubmit"));
        assert_eq!(e.apply(&hook("SessionStart")), None);
        assert_eq!(e.status(), Status::Working);
    }

    #[test]
    fn ended_is_terminal_for_the_engine() {
        let mut e = StatusEngine::new();
        e.apply(&hook("SessionEnd"));
        assert_eq!(e.apply(&hook("UserPromptSubmit")), None);
        assert_eq!(e.status(), Status::Ended);
    }

    #[test]
    fn unknown_and_neutral_events_change_nothing() {
        let mut e = StatusEngine::new();
        e.apply(&hook("UserPromptSubmit"));
        assert!(e.apply_all(&[hook("SubagentStop"), hook("PreCompact"), hook("SomethingNew")]).is_empty());
        assert_eq!(e.status(), Status::Working);
    }

    #[test]
    fn side_agent_stop_and_prompt_do_not_move_turn_boundaries() {
        let mut e = StatusEngine::new();
        e.apply(&hook("UserPromptSubmit"));
        assert_eq!(e.apply(&side("Stop")), None);
        assert!(e.turn_open());
        e.apply(&hook("Stop"));
        assert_eq!(e.apply(&side("UserPromptSubmit")), None);
        assert_eq!(e.status(), Status::Idle);
    }

    #[test]
    fn side_agent_tool_use_does_not_clear_main_agent_dialog() {
        let mut e = StatusEngine::new();
        e.apply_all(&[hook("UserPromptSubmit"), hook("PreToolUse"), hook("PermissionRequest")]);
        assert_eq!(e.apply(&side("PostToolUse")), None);
        assert_eq!(e.status(), Status::NeedsYou);
        assert_eq!(e.apply(&hook("PostToolUse")).unwrap().to, Status::Working);
    }

    #[test]
    fn side_agent_permission_after_turn_needs_you_until_it_proceeds() {
        let mut e = StatusEngine::new();
        e.apply_all(&[hook("UserPromptSubmit"), hook("Stop")]);
        assert_eq!(e.apply(&side("PermissionRequest")).unwrap().to, Status::NeedsYou);
        assert_eq!(e.apply(&hook("PreToolUse")), None);
        assert_eq!(e.apply(&side("PostToolUse")).unwrap().to, Status::Idle);
    }
}
