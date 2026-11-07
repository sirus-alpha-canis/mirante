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
    /// Number of turns opened by either source (the fused turn counter).
    turn_seq: u64,
    /// Number of turns the transcript itself has opened, and whether one is open there.
    tx_turn: u64,
    tx_in_turn: bool,
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
            tx_turn: 0,
            tx_in_turn: false,
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
            Input::Transcript(line) => self.apply_transcript(line),
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
        // Both sources quiescent: re-align the transcript's turn counter with the fused one,
        // so a hook-only turn (no transcript prompt line) cannot desync later turns.
        if !self.tx_in_turn {
            self.tx_turn = self.turn_seq;
        }
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

    fn apply_transcript(&mut self, v: &Value) -> Option<Transition> {
        if is_sidechain(v) {
            return None;
        }
        if is_prompt(v) {
            if self.tx_in_turn {
                return None;
            }
            self.tx_in_turn = true;
            self.tx_turn += 1;
            if self.tx_turn > self.turn_seq {
                // The transcript saw the prompt before the hook did.
                self.open_turn();
                return self.set(Status::Working, "transcript:prompt".into());
            }
            return None;
        }
        if let Some(kind) = turn_end_kind(v) {
            if !self.tx_in_turn {
                return None;
            }
            self.tx_in_turn = false;
            if self.turn_open && self.tx_turn == self.turn_seq {
                self.close_turn();
                return self.set(Status::Idle, format!("transcript:{kind}"));
            }
            if !self.turn_open {
                self.tx_turn = self.turn_seq;
            }
        }
        None
    }
}

/// Subagent / sidechain transcript lines (own turns, own stop reasons).
pub fn is_sidechain(v: &Value) -> bool {
    v["isSidechain"] == true || !v["agentId"].is_null() || !v["parentToolUseID"].is_null()
}

/// A line that starts a turn: a typed prompt (`origin.kind == "human"`), a headless prompt
/// (`promptSource == "sdk"`), or a queued message delivered after a turn
/// (`promptSource == "queued"`). Tool results have no `origin` and are not prompts.
pub fn is_prompt(v: &Value) -> bool {
    v["type"] == "user"
        && (v["origin"]["kind"] == "human" || v["promptSource"] == "sdk" || v["promptSource"] == "queued")
}

/// `Some(subtype)` for the transcript's authoritative turn-end lines.
pub fn turn_end_kind(v: &Value) -> Option<&str> {
    if v["type"] != "system" {
        return None;
    }
    v["subtype"].as_str().filter(|s| *s == "turn_duration" || *s == "stop_hook_summary")
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
#[cfg(test)]
mod fusion_tests {
    use super::*;
    use serde_json::json;

    fn hook(name: &str) -> Input {
        Input::Hook(HookEvent::named(name))
    }
    fn side_hook(name: &str) -> Input {
        Input::Hook(HookEvent { agent_id: Some("a-1".into()), ..HookEvent::named(name) })
    }
    fn tx(v: Value) -> Input {
        Input::Transcript(v)
    }
    fn prompt() -> Input {
        tx(
            json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "typed", "message": {"content": "touch s3-denied.txt"}}),
        )
    }
    fn system(subtype: &str) -> Input {
        tx(json!({"type": "system", "subtype": subtype}))
    }

    /// S3 mismatch 1 (3/3 runs): after "No" on a permission dialog no hook fires; the
    /// transcript ends the turn with `turn_duration` only (no `stop_hook_summary`).
    #[test]
    fn deny_path_is_closed_by_transcript_turn_duration() {
        let mut e = StatusEngine::new();
        e.apply_all(&[hook("SessionStart"), hook("UserPromptSubmit"), hook("PreToolUse"), hook("PermissionRequest")]);
        assert_eq!(e.status(), Status::NeedsYou);
        let t = e.apply_all(&[
            prompt(),
            tx(json!({"type": "assistant", "message": {"stop_reason": "tool_use", "content": [{"type": "tool_use", "name": "Bash"}]}})),
            tx(json!({"type": "user", "message": {"content": [{"type": "text", "text": "[Request interrupted by user for tool use]"}]}})),
            system("turn_duration"),
        ]);
        assert_eq!(
            t,
            vec![Transition { from: Status::NeedsYou, to: Status::Idle, cause: "transcript:turn_duration".into() }]
        );
        assert!(!e.turn_open());
    }

    /// S3 mismatch 2 (run 2): a PreToolUse with the same session id arrives ~1.7s after
    /// Stop, from a side agent. With or without `agent_id`, it must not flip to Working.
    #[test]
    fn stray_pre_tool_use_after_stop_stays_idle() {
        for stray in [hook("PreToolUse"), side_hook("PreToolUse")] {
            let mut e = StatusEngine::new();
            e.apply_all(&[
                hook("UserPromptSubmit"),
                hook("PreToolUse"),
                hook("PermissionRequest"),
                hook("PostToolUse"),
                hook("Stop"),
            ]);
            assert_eq!(e.apply(&stray), None);
            assert_eq!(e.status(), Status::Idle);
        }
    }

    #[test]
    fn transcript_prompt_before_hook_opens_the_turn_once() {
        let mut e = StatusEngine::new();
        assert_eq!(e.apply(&prompt()).unwrap().cause, "transcript:prompt");
        assert_eq!(e.apply(&hook("UserPromptSubmit")), None);
        assert_eq!(e.apply(&hook("Stop")).unwrap().to, Status::Idle);
        // The transcript's own end for the same turn is now a no-op.
        assert_eq!(e.apply(&system("stop_hook_summary")), None);
        assert_eq!(e.apply(&system("turn_duration")), None);
    }

    #[test]
    fn late_transcript_end_of_previous_turn_does_not_end_the_new_turn() {
        let mut e = StatusEngine::new();
        e.apply_all(&[hook("UserPromptSubmit"), prompt(), hook("Stop"), hook("UserPromptSubmit")]);
        assert_eq!(e.status(), Status::Working);
        assert_eq!(e.apply(&system("stop_hook_summary")), None);
        assert_eq!(e.apply(&system("turn_duration")), None);
        assert_eq!(e.status(), Status::Working);
        // The new turn's own prompt + end still close it.
        e.apply(&prompt());
        assert_eq!(e.apply(&system("turn_duration")).unwrap().to, Status::Idle);
    }

    #[test]
    fn hook_only_turn_does_not_desync_later_transcript_ends() {
        let mut e = StatusEngine::new();
        // Turn 1 has no transcript prompt line (e.g. a hook-only prompt).
        e.apply_all(&[hook("UserPromptSubmit"), hook("Stop")]);
        // Turn 2 is the deny path: only the transcript can close it.
        e.apply_all(&[hook("UserPromptSubmit"), hook("PreToolUse"), hook("PermissionRequest"), prompt()]);
        assert_eq!(e.apply(&system("turn_duration")).unwrap().to, Status::Idle);
    }

    #[test]
    fn queued_prompt_line_opens_a_turn() {
        let mut e = StatusEngine::new();
        e.apply_all(&[hook("UserPromptSubmit"), prompt(), hook("Stop"), system("turn_duration")]);
        let queued = tx(json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "queued", "message": {}}));
        assert_eq!(e.apply(&queued).unwrap().to, Status::Working);
        assert_eq!(e.apply(&hook("PreToolUse")), None);
        assert_eq!(e.apply(&system("turn_duration")).unwrap().to, Status::Idle);
    }

    #[test]
    fn sidechain_transcript_lines_are_ignored() {
        let mut e = StatusEngine::new();
        e.apply_all(&[hook("UserPromptSubmit"), prompt()]);
        let side_lines = [
            tx(json!({"type": "system", "subtype": "turn_duration", "isSidechain": true})),
            tx(json!({"type": "system", "subtype": "turn_duration", "agentId": "a-1"})),
            tx(json!({"type": "system", "subtype": "stop_hook_summary", "parentToolUseID": "toolu_1"})),
            tx(json!({"type": "user", "isSidechain": true, "origin": {"kind": "human"}})),
        ];
        assert!(e.apply_all(&side_lines).is_empty());
        assert_eq!(e.status(), Status::Working);
    }

    #[test]
    fn tool_result_lines_are_not_prompts() {
        let mut e = StatusEngine::new();
        let tool_result =
            tx(json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t1"}]}}));
        assert_eq!(e.apply(&tool_result), None);
        assert_eq!(e.status(), Status::Idle);
    }
}
