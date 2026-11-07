//! Status fusion: one engine per session, fed hook events and transcript lines in arrival
//! order (transcript lines in file order). Rules come from docs/spikes/FINDINGS.md:
//!
//! - A turn opens only on a main-agent `UserPromptSubmit` hook or a main-chain transcript
//!   prompt line. Tool events never open a turn, so a stray `PreToolUse` after `Stop`
//!   (side agent, S3 mismatch 2) cannot flip the task back to `Working`. One exception, a
//!   heuristic: after a slash-command `UserPromptSubmit` (prompt shaped `/name`, see
//!   [`is_slash_command`]) the turn opens on the first main-agent `PreToolUse` (see
//!   [`StatusEngine`]). Local-command transcript lines (`<command-name>`,
//!   `<local-command-stdout>`, ... wrappers, `isMeta` lines) are never prompts.
//! - A turn closes on a main-agent `Stop`, the idle `Notification`, or the transcript's
//!   `system` `turn_duration` / `stop_hook_summary` line. The transcript end covers the
//!   deny path, where no hook fires (S3 mismatch 1).
//! - Transcript turns are counted on their own; a transcript turn end closes the fused turn
//!   only when both name the same turn, so a late transcript line from the previous turn
//!   cannot end the turn the hooks already opened. Symmetrically, a hook `Stop` whose
//!   turn the transcript already closed (its `stop_hook_summary` line was seen) is ignored
//!   when a newer turn has since been opened by either source (queued prompt, Task 8
//!   probe; or the next hook prompt). A hook `Stop` that arrives before the transcript has
//!   even written its turn's prompt line is remembered and pairs with that turn's summary.
//! - Side-agent events (`agent_id` present, `SubagentStart`/`SubagentStop`; transcript
//!   `isSidechain` / `agentId` / `parentToolUseID`) never open or close a turn. A side
//!   agent's `PermissionRequest` still means `NeedsYou`: its dialog blocks the session.
//! - A permission dialog is cleared only by the asking agent's `PostToolUse` (matched by
//!   `tool_use_id` when both carry one), by [`Input::PermissionAnswered`], or by the turn
//!   end. `PreToolUse` never clears it: parallel or stray tool calls start while it is open.
//! - `SessionStart` with `source` `startup` / `resume` / `clear` / `fork` is a fresh
//!   process or context: the engine resets and goes `Idle`, also from `Ended` (a resumed
//!   session). `compact` (or no source) fires after a mid-turn compaction and only means
//!   idle when no turn is open.
use std::collections::{HashMap, VecDeque};

use serde_json::Value;

use crate::hook::HookEvent;
use crate::model::Status;

/// Text of the Notification Claude Code sends after ~60s without input (2.1.285:
/// "Claude is waiting for your input"). The agent is idle; it does not need you.
pub const IDLE_NOTIFICATION: &str = "waiting for your input";

// One short-lived input per hook/line, passed by reference: boxing the hook buys nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Hook(HookEvent),
    Transcript(Value),
    /// Mirante itself answered the open permission dialog (no hook reports the answer).
    /// Allow: back to `Working` (or `Idle` if the asker was a side agent after the turn).
    /// Deny: Claude interrupts the turn; stay `NeedsYou` until the transcript ends it.
    PermissionAnswered { allowed: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub from: Status,
    pub to: Status,
    /// `hook:<event name>`, `transcript:<line kind>` or `mirante:<action>`; stored in the
    /// status event.
    pub cause: String,
}

/// An open permission dialog: who raised it (`None` is the main agent) and, when the
/// payload carried one, the tool call it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    agent: Option<String>,
    tool_use_id: Option<String>,
}

impl Pending {
    /// Is `ev` (a `PostToolUse`) the asked-for tool finishing? By `tool_use_id` when both
    /// sides have one, else by agent identity.
    fn answered_by(&self, ev: &HookEvent) -> bool {
        match (&self.tool_use_id, &ev.tool_use_id) {
            (Some(a), Some(b)) => a == b,
            _ => self.agent == ev.agent_id,
        }
    }
}

/// Fused status of one Claude Code session.
///
/// Slash commands (heuristic): a `UserPromptSubmit` whose `prompt` is a command
/// ([`is_slash_command`]) does not open a turn, because many commands (`/context`, `/cost`,
/// `/clear`) never run the model and fire no `Stop` (2.1.285 fires no hook at all for
/// `/context`; docs/spikes/raw/slash-command.md). The turn opens when the command turns out
/// to run the model: on the next main-agent `PreToolUse` or the transcript's prompt line. A
/// `Stop`, a non-slash prompt or any `SessionStart` forgets the pending command.
///
/// Stop pairing: each main-agent hook `Stop` belongs to the same turn as one transcript
/// `stop_hook_summary` line; whichever comes first is queued until its partner arrives.
/// A lost hook delivery would leave a summary waiting forever and make every later `Stop`
/// look late; instead, a `Stop` set aside as late that gets no successor before the
/// transcript closes the current turn is taken to have been that turn's own `Stop`, so a
/// lost `Stop` costs at most one turn of lag (the transcript still closes it).
#[derive(Debug, Clone)]
pub struct StatusEngine {
    status: Status,
    turn_open: bool,
    /// Number of turns opened by either source (the fused turn counter).
    turn_seq: u64,
    /// Number of turns the transcript itself has opened, and whether one is open there.
    tx_turn: u64,
    tx_in_turn: bool,
    /// Pending permission dialog.
    pending: Option<Pending>,
    /// A slash-command prompt was submitted and has not opened a turn yet.
    slash_pending: bool,
    /// Transcript turns whose `stop_hook_summary` was seen before their hook `Stop`.
    /// The next hook `Stop` belongs to the front one. Entries older than the previous
    /// transcript turn are pruned when a new one opens; cleared on reset.
    unmatched_summaries: VecDeque<u64>,
    /// Transcript turns whose hook `Stop` came before their `stop_hook_summary` line
    /// (including a `Stop` that came before the turn's transcript prompt line).
    early_stops: Vec<u64>,
    /// The last main-agent `Stop` was set aside as late and no `Stop` has come since. If
    /// the transcript closes the current turn before another `Stop`, the set-aside one was
    /// this turn's (its predecessor's delivery was lost): pair the summary with it.
    late_stop_unpaired: bool,
    /// Latest `tool_use_id` per (agent, tool name) from `PreToolUse`, for a
    /// `PermissionRequest` whose payload lacks one. Dropped when the tool finishes.
    recent_tool_ids: HashMap<(Option<String>, Option<String>), String>,
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
            slash_pending: false,
            unmatched_summaries: VecDeque::new(),
            early_stops: Vec::new(),
            late_stop_unpaired: false,
            recent_tool_ids: HashMap::new(),
        }
    }

    pub fn status(&self) -> Status {
        self.status
    }

    pub fn turn_open(&self) -> bool {
        self.turn_open
    }

    /// Applies one input; returns the transition if the status changed. Once `Ended`, only
    /// a fresh-session `SessionStart` (resume, ...) is applied.
    pub fn apply(&mut self, input: &Input) -> Option<Transition> {
        if self.status == Status::Ended && !matches!(input, Input::Hook(ev) if is_fresh_start(ev)) {
            return None;
        }
        match input {
            Input::Hook(ev) => self.apply_hook(ev),
            Input::Transcript(line) => self.apply_transcript(line),
            Input::PermissionAnswered { allowed } => self.apply_answer(*allowed),
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

    /// Opens a turn from a hook. If the transcript never wrote the previous turn's prompt
    /// (a hook-only turn), it skipped that turn: re-align its counter so its next prompt
    /// line names this turn. Early Stops for skipped turns can no longer pair.
    fn open_turn_from_hook(&mut self) {
        if !self.tx_in_turn && self.tx_turn < self.turn_seq {
            self.tx_turn = self.turn_seq;
            let tx_turn = self.tx_turn;
            self.early_stops.retain(|t| *t > tx_turn);
        }
        self.open_turn();
    }

    fn open_turn(&mut self) {
        self.turn_open = true;
        self.turn_seq += 1;
        self.pending = None;
        self.slash_pending = false;
    }

    /// The transcript counter is NOT re-aligned here: the transcript may still be about to
    /// write this turn's prompt line (it lags the hooks). [`Self::open_turn_from_hook`]
    /// re-aligns it once the next turn proves the transcript skipped this one.
    fn close_turn(&mut self) {
        self.turn_open = false;
        self.pending = None;
    }

    /// A new process or a cleared context: nothing from before can still be in flight.
    fn reset(&mut self) {
        self.turn_open = false;
        self.tx_in_turn = false;
        self.tx_turn = self.turn_seq;
        self.pending = None;
        self.slash_pending = false;
        self.unmatched_summaries.clear();
        self.early_stops.clear();
        self.late_stop_unpaired = false;
        self.recent_tool_ids.clear();
    }

    /// Status after a dialog goes away: `Working` inside a turn, else `Idle`.
    fn after_dialog(&self) -> Status {
        if self.turn_open { Status::Working } else { Status::Idle }
    }

    fn apply_hook(&mut self, ev: &HookEvent) -> Option<Transition> {
        let cause = format!("hook:{}", ev.hook_event_name);
        let side = ev.is_side_agent();
        if ev.hook_event_name == "SessionStart" {
            // `/compact` and friends: whatever command was pending has run.
            self.slash_pending = false;
        }
        match ev.hook_event_name.as_str() {
            "SessionStart" if is_fresh_start(ev) => {
                self.reset();
                self.set(Status::Idle, cause)
            }
            // `compact` (or an unknown source) also fires after a mid-turn compaction; it
            // only means idle when no turn is open.
            "SessionStart" if !self.turn_open => self.set(Status::Idle, cause),
            "UserPromptSubmit" if !side && !self.turn_open => {
                if ev.prompt.as_deref().is_some_and(is_slash_command) {
                    self.slash_pending = true;
                    return None;
                }
                self.open_turn_from_hook();
                self.set(Status::Working, cause)
            }
            "PreToolUse" if !side && self.slash_pending && !self.turn_open => {
                self.remember_tool(ev);
                self.open_turn_from_hook();
                self.set(Status::Working, cause)
            }
            "PreToolUse" => {
                self.remember_tool(ev);
                // Progress inside a turn. Never clears a dialog: a parallel or stray tool
                // call can start while the dialog waits.
                if self.pending.is_none() && self.turn_open {
                    return self.set(Status::Working, cause);
                }
                None
            }
            "PostToolUse" => {
                self.forget_tool(ev);
                self.apply_post_tool_use(ev, cause)
            }
            "PermissionRequest" => {
                // Some payloads lack `tool_use_id`: borrow it from the latest PreToolUse of
                // the same agent and tool, so a parallel tool finishing is not the answer.
                let tool_use_id = ev
                    .tool_use_id
                    .clone()
                    .or_else(|| self.recent_tool_ids.get(&(ev.agent_id.clone(), ev.tool_name.clone())).cloned());
                self.pending = Some(Pending { agent: ev.agent_id.clone(), tool_use_id });
                self.set(Status::NeedsYou, cause)
            }
            "Notification" if ev.message.as_deref().is_some_and(|m| m.contains(IDLE_NOTIFICATION)) => {
                self.slash_pending = false;
                self.close_turn();
                self.set(Status::Idle, cause)
            }
            "Notification" => self.set(Status::NeedsYou, cause),
            "Stop" if !side => {
                self.slash_pending = false;
                self.late_stop_unpaired = false;
                match self.unmatched_summaries.pop_front() {
                    // The transcript already closed this Stop's turn and a newer turn has
                    // opened since (from either source): the Stop is late and must not end
                    // the newer turn.
                    Some(stopped) if self.turn_open && self.turn_seq.max(self.tx_turn) > stopped => {
                        self.late_stop_unpaired = true;
                        return None;
                    }
                    Some(_) => {}
                    None if self.tx_in_turn => self.early_stops.push(self.tx_turn),
                    // The transcript has not written this turn's prompt yet: its summary
                    // will come under the next transcript turn number.
                    None if self.turn_open && self.tx_turn < self.turn_seq => self.early_stops.push(self.tx_turn + 1),
                    None => {}
                }
                self.close_turn();
                self.set(Status::Idle, cause)
            }
            "SessionEnd" => {
                self.slash_pending = false;
                self.close_turn();
                self.set(Status::Ended, cause)
            }
            _ => None,
        }
    }

    fn apply_post_tool_use(&mut self, ev: &HookEvent, cause: String) -> Option<Transition> {
        match &self.pending {
            Some(p) if p.answered_by(ev) => {
                self.pending = None;
                let to = self.after_dialog();
                self.set(to, cause)
            }
            Some(_) => None,
            None if self.turn_open => self.set(Status::Working, cause),
            None => None,
        }
    }

    fn remember_tool(&mut self, ev: &HookEvent) {
        if let Some(id) = &ev.tool_use_id {
            self.recent_tool_ids.insert((ev.agent_id.clone(), ev.tool_name.clone()), id.clone());
        }
    }

    fn forget_tool(&mut self, ev: &HookEvent) {
        if let Some(id) = &ev.tool_use_id {
            self.recent_tool_ids.retain(|_, v| v != id);
        }
    }

    fn apply_answer(&mut self, allowed: bool) -> Option<Transition> {
        self.pending.as_ref()?;
        if allowed {
            self.pending = None;
            let to = self.after_dialog();
            return self.set(to, "mirante:permission_allowed".into());
        }
        if self.turn_open {
            // Claude ends the turn with "[Request interrupted by user for tool use]"; the
            // transcript's `turn_duration` closes it. Until then the dialog stands.
            return None;
        }
        // A side agent's dialog after the main turn: nothing else will end it.
        self.pending = None;
        self.set(Status::Idle, "mirante:permission_denied".into())
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
            let tx_turn = self.tx_turn;
            self.early_stops.retain(|t| t + 1 >= tx_turn);
            // A summary two transcript turns back still waiting for its Stop will not get
            // one (lost delivery): drop it so it cannot shadow a later Stop.
            self.unmatched_summaries.retain(|t| t + 1 >= tx_turn);
            if self.tx_turn > self.turn_seq {
                // The transcript saw the prompt before the hook did.
                self.open_turn();
                return self.set(Status::Working, "transcript:prompt".into());
            }
            return None;
        }
        if let Some(kind) = turn_end_kind(v) {
            if kind == "stop_hook_summary" && self.tx_in_turn {
                // Pair the summary with its hook Stop, whichever arrives first. Outside a
                // transcript turn (tail attached mid-turn, a cleared turn) it has no Stop.
                match self.early_stops.iter().position(|t| *t == self.tx_turn) {
                    Some(i) => {
                        self.early_stops.remove(i);
                    }
                    // The Stop set aside as late was this turn's (see `late_stop_unpaired`).
                    None if self.late_stop_unpaired => self.late_stop_unpaired = false,
                    None => self.unmatched_summaries.push_back(self.tx_turn),
                }
            }
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

/// `SessionStart` that begins a fresh process or context (not a mid-turn compaction).
fn is_fresh_start(ev: &HookEvent) -> bool {
    ev.hook_event_name == "SessionStart"
        && matches!(ev.source.as_deref(), Some("startup" | "resume" | "clear" | "fork"))
}

/// Subagent / sidechain transcript lines (own turns, own stop reasons).
pub fn is_sidechain(v: &Value) -> bool {
    v["isSidechain"] == true || !v["agentId"].is_null() || !v["parentToolUseID"].is_null()
}

/// A line that starts a turn: a typed prompt (`origin.kind == "human"`), a headless prompt
/// (`promptSource == "sdk"`), or a queued message delivered after a turn
/// (`promptSource == "queued"`). Tool results have no `origin` and are not prompts; nor are
/// `isMeta` lines or local-command wrappers ([`is_local_command_text`]), which a local slash
/// command writes without ever running the model.
pub fn is_prompt(v: &Value) -> bool {
    v["type"] == "user"
        && v["isMeta"] != true
        && (v["origin"]["kind"] == "human" || v["promptSource"] == "sdk" || v["promptSource"] == "queued")
        && !first_text(&v["message"]["content"]).is_some_and(is_local_command_text)
}

/// The string content, or the first text block of an array content.
fn first_text(content: &Value) -> Option<&str> {
    match content {
        Value::String(s) => Some(s),
        Value::Array(items) => items.iter().find_map(|i| i["text"].as_str()),
        _ => None,
    }
}

/// Text Claude Code writes for a local slash command: the command echo
/// (`<command-name>` / `<command-message>` / `<command-args>`) or its output
/// (`<local-command-stdout>` / `-stderr>` / `-caveat>`). 2.1.285 writes these as `system`
/// `local_command` lines; older or other versions may write them as user lines.
pub fn is_local_command_text(text: &str) -> bool {
    let t = text.trim_start();
    ["<command-name>", "<command-message>", "<command-args>", "<local-command-"].iter().any(|p| t.starts_with(p))
}

/// A slash command: `/` then a name of letters, digits, `_`, `:` or `-`, then whitespace
/// or the end (`/review src`, `/plugin:cmd`). `/Users/me/x.rs explain` is a normal prompt.
pub fn is_slash_command(prompt: &str) -> bool {
    let Some(rest) = prompt.trim_start().strip_prefix('/') else {
        return false;
    };
    let name_len =
        rest.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '-'))).unwrap_or(rest.len());
    name_len > 0 && rest[name_len..].chars().next().is_none_or(char::is_whitespace)
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

/// Review fixes: resume/clear, dialog clearing, late hook Stop, slash commands, and fixtures
/// replaying the real Week-1 sequences (docs/spikes/raw/task8-outputs.md).
#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use serde_json::json;

    fn hook(name: &str) -> Input {
        Input::Hook(HookEvent::named(name))
    }
    fn start(source: &str) -> Input {
        Input::Hook(HookEvent { source: Some(source.into()), ..HookEvent::named("SessionStart") })
    }
    fn ups(prompt: &str) -> Input {
        Input::Hook(HookEvent { prompt: Some(prompt.into()), ..HookEvent::named("UserPromptSubmit") })
    }
    fn tool(name: &str, id: &str) -> Input {
        Input::Hook(HookEvent {
            tool_name: Some("Bash".into()),
            tool_use_id: Some(id.into()),
            ..HookEvent::named(name)
        })
    }
    fn side_tool(name: &str, agent: &str) -> Input {
        Input::Hook(HookEvent { agent_id: Some(agent.into()), ..HookEvent::named(name) })
    }
    fn tx(v: Value) -> Input {
        Input::Transcript(v)
    }
    fn typed(text: &str) -> Input {
        tx(json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "typed", "message": {"content": text}}))
    }
    fn queued(text: &str) -> Input {
        tx(json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "queued", "message": {"content": text}}))
    }
    fn system(subtype: &str) -> Input {
        tx(json!({"type": "system", "subtype": subtype}))
    }
    fn op(operation: &str, reason: Option<&str>) -> Input {
        let mut v = json!({"type": "queue-operation", "operation": operation, "content": "stop, say BANANA"});
        if let Some(r) = reason {
            v["reason"] = json!(r);
        }
        tx(v)
    }
    fn assistant(stop_reason: &str) -> Input {
        tx(json!({"type": "assistant", "message": {"stop_reason": stop_reason, "content": []}}))
    }
    fn tool_result() -> Input {
        tx(json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1"}]}}))
    }
    fn statuses(e: &mut StatusEngine, inputs: &[Input]) -> Vec<Status> {
        e.apply_all(inputs).into_iter().map(|t| t.to).collect()
    }

    // --- SessionStart by source -------------------------------------------------------

    #[test]
    fn resume_after_end_goes_idle() {
        let mut e = StatusEngine::new();
        e.apply_all(&[start("startup"), ups("hi"), hook("Stop"), hook("SessionEnd")]);
        assert_eq!(e.status(), Status::Ended);
        let t = e.apply(&start("resume")).unwrap();
        assert_eq!((t.from, t.to, t.cause.as_str()), (Status::Ended, Status::Idle, "hook:SessionStart"));
        assert_eq!(e.apply(&ups("again")).unwrap().to, Status::Working);
    }

    #[test]
    fn resume_after_kill_mid_turn_goes_idle() {
        let mut e = StatusEngine::new();
        // Killed while a dialog was open: no Stop, no SessionEnd, transcript mid-turn.
        e.apply_all(&[start("startup"), ups("touch x"), typed("touch x"), tool("PreToolUse", "t1")]);
        e.apply(&hook("PermissionRequest"));
        assert_eq!(e.status(), Status::NeedsYou);
        assert_eq!(e.apply(&start("resume")).unwrap().to, Status::Idle);
        assert!(!e.turn_open());
        // The resumed session's first turn opens and the transcript alone can close it.
        assert_eq!(statuses(&mut e, &[ups("next"), typed("next")]), vec![Status::Working]);
        assert_eq!(e.apply(&system("turn_duration")).unwrap().to, Status::Idle);
    }

    #[test]
    fn clear_resets() {
        let mut e = StatusEngine::new();
        e.apply_all(&[ups("long job"), typed("long job"), tool("PreToolUse", "t1")]);
        assert_eq!(e.apply(&start("clear")).unwrap().to, Status::Idle);
        assert!(!e.turn_open());
        // Late end lines of the cleared turn change nothing; the next turn works normally.
        assert!(e.apply_all(&[system("stop_hook_summary"), system("turn_duration")]).is_empty());
        assert_eq!(statuses(&mut e, &[ups("new"), typed("new"), hook("Stop")]), vec![Status::Working, Status::Idle]);
    }

    #[test]
    fn fork_is_a_fresh_session_too() {
        let mut e = StatusEngine::new();
        e.apply_all(&[ups("x"), hook("SessionEnd")]);
        assert_eq!(e.apply(&start("fork")).unwrap().to, Status::Idle);
    }

    #[test]
    fn compact_mid_turn_still_ignored() {
        let mut e = StatusEngine::new();
        e.apply(&ups("big task"));
        assert_eq!(e.apply(&start("compact")), None);
        assert_eq!(e.status(), Status::Working);
        assert!(e.turn_open());
        e.apply(&hook("Stop"));
        assert_eq!(e.apply(&start("compact")), None);
        assert_eq!(e.status(), Status::Idle);
    }

    #[test]
    fn compact_or_unknown_source_does_not_revive_an_ended_session() {
        let mut e = StatusEngine::new();
        e.apply(&hook("SessionEnd"));
        assert_eq!(e.apply(&start("compact")), None);
        assert_eq!(e.apply(&hook("SessionStart")), None);
        assert_eq!(e.status(), Status::Ended);
    }

    // --- Pending permission ------------------------------------------------------------

    #[test]
    fn stray_or_parallel_pre_tool_use_during_needs_you_stays_needs_you() {
        let mut e = StatusEngine::new();
        e.apply_all(&[ups("go"), tool("PreToolUse", "t1")]);
        e.apply(&Input::Hook(HookEvent { tool_use_id: Some("t1".into()), ..HookEvent::named("PermissionRequest") }));
        assert_eq!(e.status(), Status::NeedsYou);
        assert_eq!(e.apply(&tool("PreToolUse", "t2")), None);
        assert_eq!(e.apply(&hook("PreToolUse")), None);
        assert_eq!(e.apply(&side_tool("PreToolUse", "a-1")), None);
        // A parallel tool of the same agent finishing is not the answer to the dialog.
        assert_eq!(e.apply(&tool("PostToolUse", "t2")), None);
        assert_eq!(e.status(), Status::NeedsYou);
    }

    #[test]
    fn post_tool_use_with_matching_tool_use_id_clears() {
        let mut e = StatusEngine::new();
        e.apply_all(&[ups("go"), tool("PreToolUse", "t1")]);
        e.apply(&Input::Hook(HookEvent { tool_use_id: Some("t1".into()), ..HookEvent::named("PermissionRequest") }));
        let t = e.apply(&tool("PostToolUse", "t1")).unwrap();
        assert_eq!((t.from, t.to), (Status::NeedsYou, Status::Working));
    }

    #[test]
    fn post_tool_use_without_ids_falls_back_to_agent_identity() {
        let mut e = StatusEngine::new();
        e.apply_all(&[ups("go"), hook("PreToolUse"), hook("PermissionRequest")]);
        assert_eq!(e.apply(&side_tool("PostToolUse", "a-1")), None);
        assert_eq!(e.apply(&tool("PostToolUse", "t9")).unwrap().to, Status::Working);
    }

    #[test]
    fn permission_answered_allow_goes_working() {
        let mut e = StatusEngine::new();
        e.apply_all(&[ups("go"), hook("PreToolUse"), hook("PermissionRequest")]);
        let t = e.apply(&Input::PermissionAnswered { allowed: true }).unwrap();
        assert_eq!((t.from, t.to, t.cause.as_str()), (Status::NeedsYou, Status::Working, "mirante:permission_allowed"));
        // The dialog is gone: a later PostToolUse is plain progress.
        assert_eq!(e.apply(&hook("PostToolUse")), None);
    }

    #[test]
    fn permission_answered_deny_waits_for_the_turn_end() {
        let mut e = StatusEngine::new();
        e.apply_all(&[ups("go"), typed("go"), hook("PreToolUse"), hook("PermissionRequest")]);
        assert_eq!(e.apply(&Input::PermissionAnswered { allowed: false }), None);
        assert_eq!(e.status(), Status::NeedsYou);
        assert_eq!(e.apply(&system("turn_duration")).unwrap().to, Status::Idle);
    }

    #[test]
    fn permission_answered_for_a_side_agent_after_the_turn() {
        let mut e = StatusEngine::new();
        e.apply_all(&[ups("go"), hook("Stop"), side_tool("PermissionRequest", "a-1")]);
        assert_eq!(e.apply(&Input::PermissionAnswered { allowed: false }).unwrap().to, Status::Idle);
        let mut e = StatusEngine::new();
        assert_eq!(e.apply(&Input::PermissionAnswered { allowed: true }), None);
    }

    // --- Late hook Stop -------------------------------------------------------------

    /// Real queued-probe order (task8-outputs.md "Probe"): the transcript closes turn 1 and
    /// opens turn 2 (queued prompt) BEFORE turn 1's hook Stop is delivered.
    #[test]
    fn late_stop_of_previous_turn_does_not_idle_the_queued_turn() {
        let mut e = StatusEngine::new();
        let t = statuses(
            &mut e,
            &[
                start("startup"),
                ups("list 60 fruits"),
                typed("list 60 fruits"),
                ups("now say BANANA"), // fires at enqueue, mid-turn
                op("enqueue", None),
                assistant("end_turn"),
                assistant("end_turn"),
                op("dequeue", None),
                system("stop_hook_summary"),
                system("turn_duration"),
                queued("now say BANANA"),
            ],
        );
        assert_eq!(t, vec![Status::Working, Status::Idle, Status::Working]);
        // Turn 1's Stop arrives late: turn 2 keeps working.
        assert_eq!(e.apply(&hook("Stop")), None);
        assert_eq!(e.status(), Status::Working);
        assert!(e.turn_open());
        // Turn 2's own Stop and end lines close it exactly once.
        let tail = [hook("Stop"), assistant("end_turn"), system("stop_hook_summary"), system("turn_duration")];
        let t = statuses(&mut e, &tail);
        assert_eq!(t, vec![Status::Idle]);
        // And the next turn still opens and closes normally.
        let next = [ups("three"), typed("three"), system("stop_hook_summary"), hook("Stop"), system("turn_duration")];
        let t = statuses(&mut e, &next);
        assert_eq!(t, vec![Status::Working, Status::Idle]);
    }

    /// Same probe, hook Stop delivered in real-time order (it ran ~1ms before the summary).
    #[test]
    fn queued_probe_in_real_time_order_has_no_flicker() {
        let mut e = StatusEngine::new();
        let t = statuses(
            &mut e,
            &[
                ups("list 60 fruits"),
                typed("list 60 fruits"),
                ups("now say BANANA"),
                op("enqueue", None),
                assistant("end_turn"),
                hook("Stop"),
                op("dequeue", None),
                system("stop_hook_summary"),
                system("turn_duration"),
                queued("now say BANANA"),
                assistant("end_turn"),
                hook("Stop"),
                system("stop_hook_summary"),
                system("turn_duration"),
            ],
        );
        assert_eq!(t, vec![Status::Working, Status::Idle, Status::Working, Status::Idle]);
    }

    #[test]
    fn stop_after_its_own_summary_line_is_harmless() {
        let mut e = StatusEngine::new();
        let turn = [ups("a"), typed("a"), system("stop_hook_summary"), hook("Stop"), system("turn_duration")];
        let t = statuses(&mut e, &turn);
        assert_eq!(t, vec![Status::Working, Status::Idle]);
        let t = statuses(&mut e, &[ups("b"), typed("b"), hook("Stop"), system("stop_hook_summary")]);
        assert_eq!(t, vec![Status::Working, Status::Idle]);
    }

    // --- Slash commands --------------------------------------------------------------

    #[test]
    fn slash_command_opens_a_turn_only_on_tool_use_or_transcript_prompt() {
        // A local command (/context): UPS, no model turn, no Stop.
        let mut e = StatusEngine::new();
        assert_eq!(e.apply(&ups("/context")), None);
        assert_eq!(e.status(), Status::Idle);
        assert!(!e.turn_open());
        // A command that runs the model: the first tool use opens the turn.
        let mut e = StatusEngine::new();
        let t = statuses(&mut e, &[ups("/review"), hook("PreToolUse"), hook("Stop")]);
        assert_eq!(t, vec![Status::Working, Status::Idle]);
        // ... or its transcript prompt line does.
        let mut e = StatusEngine::new();
        let t = statuses(&mut e, &[ups("/review"), typed("/review"), hook("Stop")]);
        assert_eq!(t, vec![Status::Working, Status::Idle]);
        // A Stop forgets the pending slash command: a later stray PreToolUse stays idle.
        let mut e = StatusEngine::new();
        e.apply_all(&[ups("/context"), hook("Stop")]);
        assert_eq!(e.apply(&hook("PreToolUse")), None);
        assert_eq!(e.status(), Status::Idle);
    }

    // --- Fixtures from the real Week-1 runs -------------------------------------------

    /// Task 8 steer run (re-run, delay 6): "stop, say BANANA" typed during step 1 is absorbed
    /// at the next tool boundary. Hooks: UPS, PreToolUse, UPS (enqueue), PostToolUse, Stop,
    /// SubagentStop. Transcript: prompt, tool_use, enqueue, remove absorbed_mid_turn,
    /// tool_result, queued_command attachment, end_turn, stop_hook_summary, turn_duration.
    #[test]
    fn fixture_task8_steer_run_is_working_throughout_then_one_idle() {
        let attachment =
            tx(json!({"type": "attachment", "attachment": {"type": "queued_command", "prompt": "stop, say BANANA"}}));
        let run = |tail: &[Input]| {
            let mut e = StatusEngine::new();
            let inputs = [
                start("startup"),
                ups("run five steps"),
                typed("run five steps"),
                assistant("tool_use"),
                tool("PreToolUse", "toolu_1"),
                ups("stop, say BANANA"),
                op("enqueue", None),
                op("remove", Some("absorbed_mid_turn")),
                tool("PostToolUse", "toolu_1"),
                tool_result(),
                attachment.clone(),
                assistant("end_turn"),
            ];
            let mut seen = Vec::new();
            for i in &inputs {
                if let Some(t) = e.apply(i) {
                    seen.push(t.to);
                }
                if seen.len() == 1 {
                    assert_eq!(e.status(), Status::Working);
                }
            }
            seen.extend(statuses(&mut e, tail));
            seen
        };
        let hook_first = [hook("Stop"), system("stop_hook_summary"), system("turn_duration"), hook("SubagentStop")];
        let tx_first = [system("stop_hook_summary"), system("turn_duration"), hook("Stop"), hook("SubagentStop")];
        assert_eq!(run(&hook_first), vec![Status::Working, Status::Idle]);
        assert_eq!(run(&tx_first), vec![Status::Working, Status::Idle]);
    }

    /// Queued probe: hooks UPS, UPS, Stop, Stop, SubagentStop; transcript prompt, enqueue,
    /// end_turn x2, dequeue, stop_hook_summary, turn_duration, queued prompt, end_turn,
    /// stop_hook_summary, turn_duration. Interleaved as observed (hook timestamps vs
    /// transcript file order): Idle -> Working (turn 1) -> Idle -> Working (turn 2) -> Idle.
    #[test]
    fn fixture_queued_probe_has_no_flicker() {
        let mut e = StatusEngine::new();
        let mut got = e.apply_all(&[
            start("startup"),
            ups("list 60 fruits"),
            typed("list 60 fruits"),
            ups("now say BANANA"),
            op("enqueue", None),
            assistant("end_turn"),
            assistant("end_turn"),
            op("dequeue", None),
            system("stop_hook_summary"),
            system("turn_duration"),
            queued("now say BANANA"),
            hook("Stop"), // turn 1's Stop, delivered after the transcript moved on
        ]);
        assert_eq!(e.status(), Status::Working, "turn 1's late Stop must not idle turn 2");
        // Turn 2's answer is written (16:37:02.186) just before its Stop hook runs (…422222).
        got.extend(e.apply_all(&[
            assistant("end_turn"),
            hook("Stop"),
            system("stop_hook_summary"),
            system("turn_duration"),
            hook("SubagentStop"),
        ]));
        let got: Vec<(Status, &str)> = got.iter().map(|t| (t.to, t.cause.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (Status::Working, "hook:UserPromptSubmit"),
                (Status::Idle, "transcript:stop_hook_summary"),
                (Status::Working, "transcript:prompt"),
                (Status::Idle, "hook:Stop"),
            ]
        );
    }
}

/// Re-review fixes: Stop/summary pairing that cannot poison later turns, a late Stop vs a
/// hook-opened next turn, strict slash-command detection, and the real `/context` capture
/// (docs/spikes/raw/slash-command.md).
#[cfg(test)]
mod pairing_tests {
    use super::*;
    use serde_json::json;

    fn hook(name: &str) -> Input {
        Input::Hook(HookEvent::named(name))
    }
    fn start(source: &str) -> Input {
        Input::Hook(HookEvent { source: Some(source.into()), ..HookEvent::named("SessionStart") })
    }
    fn ups(prompt: &str) -> Input {
        Input::Hook(HookEvent { prompt: Some(prompt.into()), ..HookEvent::named("UserPromptSubmit") })
    }
    fn tool(name: &str, tool_name: &str, id: Option<&str>) -> Input {
        Input::Hook(HookEvent {
            tool_name: Some(tool_name.into()),
            tool_use_id: id.map(Into::into),
            ..HookEvent::named(name)
        })
    }
    fn typed(text: &str) -> Input {
        Input::Transcript(
            json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "typed", "message": {"content": text}}),
        )
    }
    fn system(subtype: &str) -> Input {
        Input::Transcript(json!({"type": "system", "subtype": subtype}))
    }
    fn run(e: &mut StatusEngine, inputs: &[Input]) -> Vec<(Status, String)> {
        e.apply_all(inputs).into_iter().map(|t| (t.to, t.cause)).collect()
    }
    fn pairs(v: &[(Status, &str)]) -> Vec<(Status, String)> {
        v.iter().map(|(s, c)| (*s, c.to_string())).collect()
    }

    /// P1: the transcript lags a whole turn (hook Stop before the transcript prompt line).
    /// That Stop pairs with the turn's summary when it shows up, so the next turn's own
    /// Stop still idles it.
    #[test]
    fn transcript_lagging_a_whole_turn_does_not_poison_the_next_stop() {
        let mut e = StatusEngine::new();
        let first =
            run(&mut e, &[ups("hi"), hook("Stop"), typed("hi"), system("stop_hook_summary"), system("turn_duration")]);
        assert_eq!(first, pairs(&[(Status::Working, "hook:UserPromptSubmit"), (Status::Idle, "hook:Stop")]));
        let second = run(&mut e, &[ups("two"), typed("two"), hook("Stop")]);
        assert_eq!(second, pairs(&[(Status::Working, "hook:UserPromptSubmit"), (Status::Idle, "hook:Stop")]));
        assert!(!e.turn_open());
        assert!(e.apply_all(&[system("stop_hook_summary"), system("turn_duration")]).is_empty());
        assert_eq!(e.status(), Status::Idle);
    }

    /// P8: turn 1's hook Stop is lost. At most the next turn lags (its Stop is taken for
    /// turn 1's, the transcript closes it); every later turn idles on its own Stop again.
    #[test]
    fn one_lost_stop_lags_at_most_one_turn() {
        let mut e = StatusEngine::new();
        e.apply_all(&[ups("a"), typed("a"), system("stop_hook_summary"), system("turn_duration")]);
        assert_eq!(e.status(), Status::Idle);
        let mut idled_on_stop = Vec::new();
        for _ in 0..4 {
            e.apply_all(&[ups("n"), typed("n")]);
            assert_eq!(e.status(), Status::Working);
            e.apply(&hook("Stop"));
            idled_on_stop.push(e.status() == Status::Idle);
            e.apply_all(&[system("stop_hook_summary"), system("turn_duration")]);
            assert_eq!(e.status(), Status::Idle);
        }
        assert_eq!(idled_on_stop, vec![false, true, true, true]);
    }

    /// P3: turn a's Stop arrives after the transcript closed a AND after b's hook prompt
    /// (b's transcript line not yet written). The late Stop must not idle turn b.
    #[test]
    fn late_stop_does_not_close_a_hook_opened_next_turn() {
        let mut e = StatusEngine::new();
        let got = run(
            &mut e,
            &[ups("a"), typed("a"), system("stop_hook_summary"), system("turn_duration"), ups("b"), hook("Stop")],
        );
        assert_eq!(
            got,
            pairs(&[
                (Status::Working, "hook:UserPromptSubmit"),
                (Status::Idle, "transcript:stop_hook_summary"),
                (Status::Working, "hook:UserPromptSubmit"),
            ])
        );
        assert!(e.turn_open());
        let got = run(&mut e, &[typed("b"), hook("Stop"), system("stop_hook_summary"), system("turn_duration")]);
        assert_eq!(got, pairs(&[(Status::Idle, "hook:Stop")]));
        // And the turn after that pairs normally.
        let got = run(&mut e, &[ups("c"), typed("c"), hook("Stop")]);
        assert_eq!(got, pairs(&[(Status::Working, "hook:UserPromptSubmit"), (Status::Idle, "hook:Stop")]));
    }

    /// M1: only `/name` (letters, digits, `_`, `:`, `-`) followed by whitespace or the end
    /// is a slash command; a prompt starting with a path is a normal prompt.
    #[test]
    fn only_command_shaped_prompts_are_slash_commands() {
        for normal in ["/Users/me/x.rs explain this file", "/", "/ hi", "/tmp/a", "/x.rs"] {
            let mut e = StatusEngine::new();
            assert_eq!(e.apply(&ups(normal)).map(|t| t.to), Some(Status::Working), "{normal:?}");
        }
        for slash in ["/context", "/review src", "/my-plugin:cmd\targs", "/compact\n", "  /cost"] {
            let mut e = StatusEngine::new();
            assert_eq!(e.apply(&ups(slash)), None, "{slash:?}");
        }
    }

    /// M2 (P5): `/compact` then `SessionStart` (compact): the pending command is forgotten,
    /// so a stray tool event does not open a turn.
    #[test]
    fn any_session_start_forgets_a_pending_slash_command() {
        let mut e = StatusEngine::new();
        assert!(e.apply_all(&[ups("/compact"), start("compact"), hook("PreToolUse")]).is_empty());
        assert_eq!(e.status(), Status::Idle);
        assert!(!e.turn_open());
    }

    /// M4: a `PermissionRequest` without `tool_use_id` borrows the id of the latest
    /// `PreToolUse` of the same agent and tool, so a parallel tool finishing first does not
    /// clear the dialog.
    #[test]
    fn permission_request_without_id_borrows_it_from_the_matching_pre_tool_use() {
        let mut e = StatusEngine::new();
        e.apply_all(&[
            ups("go"),
            tool("PreToolUse", "Bash", Some("t1")),
            tool("PreToolUse", "Read", Some("t2")),
            tool("PermissionRequest", "Bash", None),
        ]);
        assert_eq!(e.status(), Status::NeedsYou);
        assert_eq!(e.apply(&tool("PostToolUse", "Read", Some("t2"))), None);
        assert_eq!(e.status(), Status::NeedsYou);
        assert_eq!(e.apply(&tool("PostToolUse", "Bash", Some("t1"))).unwrap().to, Status::Working);
    }

    /// Real capture (Claude Code 2.1.285, docs/spikes/raw/slash-command.md): `/context` fires
    /// NO hook at all and writes two `system`/`local_command` lines plus an `isMeta` user
    /// line; none opens a turn. The next prompt is a normal turn.
    #[test]
    fn fixture_real_context_command_opens_no_turn() {
        let lines = [
            json!({"type": "system", "subtype": "local_command", "isMeta": false, "isSidechain": false, "level": "info",
                   "content": "<command-name>/context</command-name>\n            <command-message>context</command-message>\n            <command-args></command-args>"}),
            json!({"type": "system", "subtype": "local_command", "isMeta": false, "isSidechain": false, "level": "info",
                   "content": "<local-command-stdout> Context Usage ...</local-command-stdout>",
                   "commandRun": {"command": "context", "args": ""}}),
            json!({"type": "user", "isMeta": true, "isSidechain": false, "promptId": "p-1",
                   "message": {"role": "user", "content": "## Context Usage\n\n**Model:** ..."}}),
        ];
        let mut e = StatusEngine::new();
        e.apply(&start("startup"));
        for l in &lines {
            assert_eq!(e.apply(&Input::Transcript(l.clone())), None);
        }
        assert!(!e.turn_open());
        let got = run(
            &mut e,
            &[
                typed("Responda apenas OK."),
                ups("Responda apenas OK."),
                Input::Transcript(
                    json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "OK"}]}}),
                ),
                hook("Stop"),
                system("stop_hook_summary"),
                system("turn_duration"),
                hook("SessionEnd"),
            ],
        );
        assert_eq!(
            got,
            pairs(&[
                (Status::Working, "transcript:prompt"),
                (Status::Idle, "hook:Stop"),
                (Status::Ended, "hook:SessionEnd")
            ])
        );
    }

    /// Defensive: a version that writes the command or its output as a human user line, with
    /// or without a `UserPromptSubmit` for the command, still opens no turn.
    #[test]
    fn local_command_wrapper_user_lines_are_not_prompts() {
        let wrappers = [
            "<command-name>/context</command-name>\n<command-message>context</command-message>\n<command-args></command-args>",
            "<command-message>context</command-message>\n<command-name>/context</command-name>",
            "<local-command-stdout>Context Usage</local-command-stdout>",
            "<local-command-stderr>oops</local-command-stderr>",
            "<local-command-caveat>Caveat: ...</local-command-caveat>",
        ];
        for w in wrappers {
            for content in [json!(w), json!([{"type": "text", "text": w}])] {
                let line = Input::Transcript(
                    json!({"type": "user", "origin": {"kind": "human"}, "message": {"content": content}}),
                );
                let mut e = StatusEngine::new();
                assert_eq!(e.apply(&line), None, "{w:?}");
                let mut e = StatusEngine::new();
                assert!(e.apply_all(&[ups("/context"), line.clone()]).is_empty(), "{w:?}");
                assert!(!e.turn_open());
            }
        }
    }
}
