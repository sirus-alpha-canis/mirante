//! Status fusion: one engine per session, fed hook events and transcript lines in arrival
//! order (transcript lines in file order). Rules come from docs/spikes/FINDINGS.md:
//!
//! - A turn opens only on a main-agent `UserPromptSubmit` hook or a main-chain transcript
//!   prompt line. Tool events never open a turn on their own, so a stray `PreToolUse` after
//!   `Stop` (side agent, S3 mismatch 2) cannot flip the task back to `Working`. Exceptions,
//!   both heuristics: after a slash-command `UserPromptSubmit` (prompt shaped `/name`, see
//!   [`is_slash_command`]) the turn opens on the first main-agent `PreToolUse`; after a
//!   queued prompt the transcript confirmed, on the first main-agent `PreToolUse` after the
//!   turn's `Stop` (see [`StatusEngine`]). Local-command transcript lines (`<command-name>`,
//!   `<local-command-stdout>`, ... wrappers, `isMeta` lines) are never prompts.
//! - A turn closes on a main-agent `Stop` (or `StopFailure`, its API-error form), the
//!   idle `Notification`, or the transcript's `system` `turn_duration` /
//!   `stop_hook_summary` line. The transcript end covers the deny path, where no hook fires
//!   (S3 mismatch 1); so does the next prompt while the main agent's dialog is open.
//! - Each source numbers turns on its own and a turn end closes only the turn it names, so
//!   a late line or a late `Stop` of an earlier turn cannot end a newer one (queued prompt,
//!   Task 8 probe; or the next hook prompt), and either source may run whole turns ahead.
//!   Cross-checks re-align the numbering when a `Stop` is lost or a prompt is queued.
//! - Side-agent events (`agent_id` present, `SubagentStart`/`SubagentStop`; transcript
//!   `isSidechain` / `agentId` / `parentToolUseID`) never open or close a turn. A side
//!   agent's `PermissionRequest` still means `NeedsYou`: its dialog blocks the session.
//!   Background agents (Claude's default, FINDINGS-threads S1) outlive the main turn: the
//!   dialog of an agent between its `SubagentStart` and `SubagentStop` survives the main
//!   turn's end, and that agent's `SubagentStop` clears it.
//! - A `<task-notification>` (a background agent finished) starts a main-agent turn in both
//!   sources: its `UserPromptSubmit` and its transcript line (`origin.kind`
//!   `task-notification`). It is Claude resuming on its own, not the person's prompt.
//! - A permission dialog is cleared only by the asking agent's `PostToolUse` or
//!   `PostToolUseFailure` (matched by `tool_use_id` when both carry one), by
//!   [`Input::PermissionAnswered`], or by the turn end. `PreToolUse` never clears it:
//!   parallel or stray tool calls start while it is open.
//! - `SessionStart` with `source` `startup` / `resume` / `clear` / `fork` is a fresh
//!   process or context: the engine resets and goes `Idle`, also from `Ended` (a resumed
//!   session). `compact` (or no source) fires after a mid-turn compaction and only means
//!   idle when no turn is open.
//! - `/compact` is no turn (no prompt or assistant line, no `Stop`), but Claude works for
//!   tens of seconds: a compaction is `Working` from its start (the main agent's
//!   `PreCompact` hook, or the transcript's plain `/compact …` user line) until either
//!   source shows it over (the transcript's `system` `compact_boundary` or a
//!   `<local-command-stdout>` / `-stderr>` line; any `SessionStart`; a turn end, a prompt,
//!   the idle `Notification`). Each source writes its start before its end, so whatever the
//!   interleaving, the last marker delivered is an end.
use std::collections::{HashMap, HashSet, VecDeque};

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
    PermissionAnswered {
        allowed: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub from: Status,
    pub to: Status,
    /// `hook:<event name>`, `transcript:<line kind>` or `mirante:<action>`; stored in the
    /// status event.
    pub cause: String,
}

/// An open permission dialog: who raised it (`None` is the main agent), the tool call it is
/// about when the payload carried one, and the tool's name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    agent: Option<String>,
    tool_use_id: Option<String>,
    tool_name: Option<String>,
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

/// How many recent turns of each source the engine remembers for alignment.
const HISTORY: usize = 8;
/// Most side agents the engine tracks as running at once.
const MAX_SIDE_AGENTS: usize = 64;

/// A `UserPromptSubmit` that arrived while the hook stream was inside a turn. It is a steer
/// (absorbed at the next tool boundary), a queued prompt (run as the next turn), or, when
/// the turn's own `Stop` was lost, the next turn's prompt. The hook stream cannot tell them
/// apart (a steer and a queued prompt both arrive between a `PreToolUse` and its
/// `PostToolUse`); the transcript does: a queued prompt opens the next transcript turn as a
/// `queued` line, a steer leaves an `enqueue` line and no turn, a next prompt neither.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MidPrompt {
    prompt: Option<String>,
    slash: bool,
    /// Main-agent hook activity (tool use, `Stop`) came after it, before any later prompt.
    followed: bool,
    /// The hook turn ended after it (a main-agent `Stop`, or a deny inferred from a prompt).
    stopped: bool,
    /// Counted as a turn of its own (queued, or the next turn's prompt).
    counted: bool,
}

/// One turn as the hook stream saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HookTurn {
    id: u64,
    prompt: Option<String>,
    mids: Vec<MidPrompt>,
    /// Over in the hook stream: its `Stop`, a deny inferred from the next prompt, or the
    /// transcript closing it while a `Stop` taken as late stood for it.
    done: bool,
    /// The `Stop` that closed it was really the next turn's (its own was the one taken as
    /// late): a queued turn adopted after it is already over.
    next_stop: bool,
}

/// One turn the transcript closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TxTurn {
    id: u64,
    /// `queue-operation` `enqueue` lines inside it (one per mid-turn prompt).
    enqueued: usize,
    /// `queued` prompt lines that opened the turns right after it.
    queued_after: usize,
    /// Closed by `stop_hook_summary` (Stop hooks ran: a hook `Stop` was sent). A deny turn
    /// has only `turn_duration`.
    summary: bool,
}

impl TxTurn {
    /// How many of the turn's mid-turn prompts the transcript accounts for (steered or
    /// queued); any more were the next turns' prompts.
    fn mid_slots(&self) -> usize {
        self.enqueued.max(self.queued_after)
    }
}

/// Fused status of one Claude Code session.
///
/// Each source numbers turns on its own, and both number the same turns, so a turn end
/// from either source names the turn it ends and can never end a newer one:
///
/// - Hooks: a main-agent `UserPromptSubmit` outside a hook turn opens the next one; `Stop`
///   closes it. A prompt inside a hook turn is a [`MidPrompt`]. A prompt while a main-agent
///   dialog is open means the dialog was denied and the turn is over (the deny path fires no
///   `Stop`). A slash command ([`is_slash_command`]) opens no turn by itself (`/context`
///   fires no `Stop`; 2.1.285 fires no hook at all for it); its turn opens on the next
///   main-agent `PreToolUse`, or is counted when its `Stop` arrives. A queued prompt's turn
///   opens on the next main-agent `PreToolUse` or `Stop` once the transcript confirmed it.
/// - Transcript: a prompt line ([`is_prompt`]) opens the next turn; `stop_hook_summary` or
///   `turn_duration` closes it. A model-running slash command writes a `<command-message>`
///   wrapper user line instead of a prompt: its turn opens on the next main-chain
///   `assistant` line, or is counted when its turn end arrives. (Counting it from the hook
///   `PreToolUse` instead would break when the transcript is ahead: its end lines would
///   already have been dropped.)
///
/// The fused turn is open while the newest turn either source has opened has not been
/// closed by either source. Cross-checks keep the numbering aligned:
///
/// - A queued line renumbers the hook stream if it already went past the queued prompt's
///   turn ([`StatusEngine::confirm_queued`]); a typed prompt line does the same for
///   mid-turn prompts the transcript does not account for: the turn's `Stop` was lost and
///   the "mid-turn" prompt was the next turn's ([`StatusEngine::recount`]).
/// - A new hook turn opening while the previous one has no `Stop` but the transcript wrote
///   its `stop_hook_summary` expects that `Stop` late (the queued-probe order): the next
///   `Stop` is set aside. If that `Stop` was really lost, the set-aside one was the current
///   turn's: the transcript closing that same turn settles it, so a lost `Stop` costs at most
///   one turn of lag.
/// - A transcript prompt whose text matches a later hook turn than the one it would number
///   skips the hook-only turns in between (a prompt no transcript line was written for).
///
/// Duplicate deliveries: a second `UserPromptSubmit` with the current turn's prompt text is
/// dropped; a second `Stop` finds no open turn and is ignored.
#[derive(Debug, Clone)]
pub struct StatusEngine {
    status: Status,
    /// Pending permission dialogs, oldest first (Claude shows them one at a time, in order).
    pending: VecDeque<Pending>,
    /// Latest `tool_use_id` per (agent, tool name) from `PreToolUse`, for a
    /// `PermissionRequest` whose payload lacks one. Dropped when the tool finishes.
    recent_tool_ids: HashMap<(Option<String>, Option<String>), String>,
    /// Every turn up to this number is over.
    ended: u64,

    /// Number of the hook stream's latest turn.
    h_turn: u64,
    /// The hook stream's recent turns, newest last (`h_turn` when it has a record).
    h_turns: VecDeque<HookTurn>,
    /// A slash-command prompt was submitted and has not opened a hook turn yet.
    slash_pending: bool,
    /// The main agent asked for permission in the current hook turn and no answer was seen.
    main_dialog: bool,
    /// Main-agent `Stop`s that found no hook turn to end since the last one closed (a queued
    /// turn not confirmed yet, a duplicate).
    stray_stops: usize,
    /// Hook turns whose `Stop` is still expected after a newer hook turn opened.
    late_stops: Vec<u64>,
    /// A `Stop` was set aside as late while this hook turn was current: if the transcript
    /// closes this turn, that `Stop` was this turn's own (its predecessor's was lost).
    late_taken: Option<u64>,

    /// Number of the transcript's latest turn, and whether it is still open there.
    t_turn: u64,
    t_open: bool,
    /// `enqueue` lines in the open transcript turn.
    t_enqueued: usize,
    /// A model-running command's wrapper line was written and no transcript turn holds it.
    t_command: bool,
    /// The transcript's recently closed turns, newest last.
    t_closed: VecDeque<TxTurn>,
    /// A compaction runs (see the module rules): `Working` even with no turn open.
    compacting: bool,
    /// Side agents between their `SubagentStart` and `SubagentStop` (or seen asking after
    /// the main turn): background agents outlive the main turn, and so do their dialogs.
    side_running: HashSet<String>,
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
            pending: VecDeque::new(),
            recent_tool_ids: HashMap::new(),
            ended: 0,
            h_turn: 0,
            h_turns: VecDeque::new(),
            slash_pending: false,
            main_dialog: false,
            stray_stops: 0,
            late_stops: Vec::new(),
            late_taken: None,
            t_turn: 0,
            t_open: false,
            t_enqueued: 0,
            t_command: false,
            t_closed: VecDeque::new(),
            compacting: false,
            side_running: HashSet::new(),
        }
    }

    pub fn status(&self) -> Status {
        self.status
    }

    /// How many permission dialogs wait for an answer.
    pub fn pending_dialogs(&self) -> usize {
        self.pending.len()
    }

    /// The tool call a `PermissionRequest` is about: its own `tool_use_id`, else the one of
    /// the latest `PreToolUse` of the same agent and tool (what the engine pairs it with).
    pub fn tool_use_id_for(&self, ev: &HookEvent) -> Option<String> {
        ev.tool_use_id
            .clone()
            .or_else(|| self.recent_tool_ids.get(&(ev.agent_id.clone(), ev.tool_name.clone())).cloned())
    }

    pub fn turn_open(&self) -> bool {
        self.h_turn.max(self.t_turn) > self.ended
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

    /// Settles the status after an event: a dialog dies with the turn it was raised in;
    /// then `NeedsYou` while a dialog is open, else `Working` inside a turn, else `Idle`.
    fn settle(&mut self, was_open: bool, cause: String) -> Option<Transition> {
        if was_open && !self.turn_open() {
            // A running background agent's dialog is still on screen (S1).
            let running = &self.side_running;
            self.pending.retain(|p| p.agent.as_ref().is_some_and(|a| running.contains(a)));
        }
        let to = if !self.pending.is_empty() {
            Status::NeedsYou
        } else if self.turn_open() || self.compacting {
            Status::Working
        } else {
            Status::Idle
        };
        self.set(to, cause)
    }

    /// Ends every turn either source has opened (idle notification, session end).
    fn end_all(&mut self) {
        self.ended = self.ended.max(self.h_turn).max(self.t_turn);
        if let Some(t) = self.h_turns.back_mut() {
            t.done = true;
        }
        self.slash_pending = false;
        self.main_dialog = false;
        self.compacting = false;
        self.pending.clear();
        self.side_running.clear();
    }

    /// A new process or a cleared context: nothing from before can still be in flight.
    fn reset(&mut self) {
        let n = self.ended.max(self.h_turn).max(self.t_turn);
        *self = StatusEngine { status: self.status, ended: n, h_turn: n, t_turn: n, ..StatusEngine::new() };
    }

    // --- Hook stream ----------------------------------------------------------------

    /// Is the hook stream inside its latest turn?
    fn hook_turn_open(&self) -> bool {
        self.h_turns.back().is_some_and(|t| t.id == self.h_turn && !t.done)
    }

    fn push_hook_turn(&mut self, prompt: Option<String>, done: bool) {
        self.h_turn += 1;
        self.h_turns.push_back(HookTurn { id: self.h_turn, prompt, mids: Vec::new(), done, next_stop: false });
        if done {
            self.ended = self.ended.max(self.h_turn);
        }
        if self.h_turns.len() > HISTORY {
            self.h_turns.pop_front();
        }
        self.main_dialog = false;
        self.stray_stops = 0;
    }

    /// Closes the current hook turn (its `Stop`, or an inferred deny).
    fn close_hook_turn(&mut self) {
        if let Some(t) = self.h_turns.back_mut().filter(|t| !t.done) {
            t.done = true;
            t.mids.iter_mut().for_each(|m| m.stopped = true);
            self.ended = self.ended.max(t.id);
        }
        self.main_dialog = false;
    }

    /// After the hook turn closed: if the transcript confirmed one of its mid-turn prompts
    /// as queued and the hook stream has not counted it yet, count it now (its turn shows in
    /// the hook stream only as tool events and a `Stop`).
    fn take_queued(&mut self) -> bool {
        let Some(t) = self.h_turns.back() else {
            return false;
        };
        let confirmed = self.tx_closed(t.id).map_or(0, |c| c.queued_after);
        let counted = t.mids.iter().filter(|m| m.counted).count();
        if !t.done || counted >= confirmed {
            return false;
        }
        let t = self.h_turns.back_mut().expect("checked above");
        match t.mids.iter_mut().find(|m| !m.counted) {
            Some(m) => {
                m.counted = true;
                true
            }
            None => false,
        }
    }

    /// Marks main-agent activity (a `Stop` if `stop`) after the current turn's mid-turn
    /// prompts. Activity vouches only for the latest one: a slash command followed by
    /// another prompt and then tool use was a local command.
    fn follow_mids(&mut self, stop: bool) {
        if self.hook_turn_open() {
            if let Some(t) = self.h_turns.back_mut() {
                if let Some(m) = t.mids.last_mut() {
                    m.followed = true;
                }
                t.mids.iter_mut().for_each(|m| m.stopped |= stop);
            }
        }
    }

    fn tx_closed(&self, id: u64) -> Option<TxTurn> {
        self.t_closed.iter().rev().find(|t| t.id == id).copied()
    }

    fn apply_hook(&mut self, ev: &HookEvent) -> Option<Transition> {
        let cause = format!("hook:{}", ev.hook_event_name);
        let side = ev.is_side_agent();
        let was_open = self.turn_open();
        let was_compacting = self.compacting;
        if ev.hook_event_name == "SessionStart" {
            // `/compact` and friends: whatever command was pending has run.
            self.slash_pending = false;
            // Fired right after a compaction (`source: compact`).
            self.compacting = false;
        }
        match ev.hook_event_name.as_str() {
            "PreCompact" if !side => self.compacting = true,
            "UserPromptSubmit" | "Stop" | "StopFailure"
                if !side && !ev.prompt.as_deref().is_some_and(is_slash_command) =>
            {
                self.compacting = false;
            }
            _ => {}
        }
        match ev.hook_event_name.as_str() {
            "SessionStart" if is_fresh_start(ev) => {
                self.reset();
                self.set(Status::Idle, cause)
            }
            // `compact` (or an unknown source) also fires after a mid-turn compaction; it
            // only means idle when no turn is open.
            "SessionStart" if !was_open => self.set(Status::Idle, cause),
            "PreCompact" if !side && !was_compacting => self.settle(was_open, cause),
            "UserPromptSubmit" if !side => self.hook_prompt(ev, was_open, cause),
            "PreToolUse" => {
                self.remember_tool(ev);
                if !side && self.hook_turn_open() {
                    self.follow_mids(false);
                } else if !side && (std::mem::take(&mut self.slash_pending) || self.take_queued()) {
                    // A pending slash command or a queued prompt turns out to run the model.
                    self.push_hook_turn(None, false);
                } else if !side {
                    // Tool use after a stray Stop: that Stop did not end the turn these tools
                    // belong to (a queued turn not confirmed yet ends after its tools).
                    self.stray_stops = 0;
                }
                // Never clears a dialog: a parallel or stray tool call can start while the
                // dialog waits.
                self.settle(was_open, cause)
            }
            // A tool that ran and failed finished all the same.
            "PostToolUse" | "PostToolUseFailure" => {
                self.forget_tool(ev);
                if !side {
                    self.follow_mids(false);
                }
                if let Some(i) = self.pending.iter().position(|p| p.answered_by(ev)) {
                    self.remove_dialog(i);
                }
                self.settle(was_open, cause)
            }
            // The main agent's dialog in a turn the transcript already closed: it was
            // answered before this delivery came in.
            "PermissionRequest" if !side && self.hook_turn_open() && self.tx_closed(self.h_turn).is_some() => None,
            "SubagentStart" => {
                if let Some(a) = &ev.agent_id {
                    if self.side_running.len() < MAX_SIDE_AGENTS {
                        self.side_running.insert(a.clone());
                    }
                }
                None
            }
            // That agent is gone, and its dialog (if any) with it.
            "SubagentStop" => {
                if let Some(a) = &ev.agent_id {
                    self.side_running.remove(a);
                    self.pending.retain(|p| p.agent.as_deref() != Some(a.as_str()));
                    if self.pending.iter().all(|p| p.agent.is_some()) {
                        self.main_dialog = false;
                    }
                }
                self.settle(was_open, cause)
            }
            "PermissionRequest" => {
                // A side agent asking while no main turn runs is a background one.
                if let Some(a) = ev.agent_id.as_ref().filter(|_| !was_open) {
                    if self.side_running.len() < MAX_SIDE_AGENTS {
                        self.side_running.insert(a.clone());
                    }
                }
                // Some payloads lack `tool_use_id`: borrow it from the latest PreToolUse of
                // the same agent and tool, so a parallel tool finishing is not the answer.
                let tool_use_id = self.tool_use_id_for(ev);
                let p = Pending { agent: ev.agent_id.clone(), tool_use_id, tool_name: ev.tool_name.clone() };
                // A duplicate delivery of the same dialog is not a second dialog (without a
                // `tool_use_id`, only a repeat of the newest one can be told apart).
                let dup =
                    if p.tool_use_id.is_some() { self.pending.contains(&p) } else { self.pending.back() == Some(&p) };
                if !dup {
                    self.pending.push_back(p);
                }
                if !side {
                    self.main_dialog = true;
                }
                self.set(Status::NeedsYou, cause)
            }
            "Notification" if ev.message.as_deref().is_some_and(|m| m.contains(IDLE_NOTIFICATION)) => {
                self.end_all();
                self.set(Status::Idle, cause)
            }
            "Notification" => self.set(Status::NeedsYou, cause),
            // `StopFailure`: the turn ended on an API error instead of normally.
            "Stop" | "StopFailure" if !side => {
                self.hook_stop();
                self.settle(was_open, cause)
            }
            "SessionEnd" => {
                self.end_all();
                self.set(Status::Ended, cause)
            }
            _ => None,
        }
    }

    fn hook_prompt(&mut self, ev: &HookEvent, was_open: bool, cause: String) -> Option<Transition> {
        let text = ev.prompt.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        let slash = text.as_deref().is_some_and(is_slash_command);
        if !slash {
            // A command still pending when a normal prompt comes was a local one.
            self.slash_pending = false;
        }
        if self.hook_turn_open() {
            let t = self.h_turns.back().expect("open hook turn has a record");
            // The same prompt delivered twice (a slash command may really be run again).
            if text.is_some() && !slash && (t.prompt == text || t.mids.iter().any(|m| m.prompt == text)) {
                return None;
            }
            let (id, mids) = (t.id, t.mids.len());
            let closed = self.tx_closed(id);
            // The transcript closed this turn with fewer mid-turn prompts than the hook
            // stream holds: the extra ones were the next turns' prompts. Recount, then look
            // at this prompt again from the turn it falls in.
            if let Some(c) = closed.filter(|c| mids > c.mid_slots()) {
                if self.recount_record(self.h_turns.len() - 1, c.mid_slots()) {
                    return self.hook_prompt(ev, was_open, cause);
                }
            }
            // Over when its dialog was dismissed (deny: no Stop), or when the transcript
            // closed it with no room left for another mid-turn prompt (its Stop was lost).
            if !self.main_dialog && closed.is_none_or(|c| mids < c.mid_slots()) {
                let m = MidPrompt { prompt: text, slash, followed: false, stopped: false, counted: false };
                self.h_turns.back_mut().expect("open hook turn has a record").mids.push(m);
                return None;
            }
            if self.main_dialog {
                // The dialog was denied: it is gone with its turn.
                self.pending.retain(|p| p.agent.is_some());
            } else if closed.is_some_and(|c| c.summary) {
                // Its Stop hooks ran, so its Stop was sent: expect it late.
                self.late_stops.push(id);
            }
            self.close_hook_turn();
        }
        // The previous turn, closed by both sources, holding a mid-turn prompt its transcript
        // turn does not account for (activity showed up after the transcript moved on).
        if let Some(t) = self.h_turns.back().filter(|t| t.done && t.id == self.h_turn) {
            if let Some(c) = self.tx_closed(t.id) {
                self.recount_record(self.h_turns.len() - 1, c.mid_slots());
            }
        }
        // Confirmed queued turns the hook stream saw no event of (their Stop was lost).
        while self.take_queued() {
            self.push_hook_turn(None, true);
        }
        self.slash_pending = slash;
        if !slash {
            self.push_hook_turn(text, false);
        }
        self.settle(was_open, cause)
    }

    fn hook_stop(&mut self) {
        if !self.late_stops.is_empty() {
            // A turn the transcript already closed (Stop hooks ran) still owed its Stop and a
            // newer turn has opened since: this is that Stop.
            self.late_stops.remove(0);
            self.late_taken = Some(self.h_turn);
            self.follow_mids(true);
            return;
        }
        if self.hook_turn_open() {
            self.follow_mids(true);
            self.close_hook_turn();
        } else if std::mem::take(&mut self.slash_pending) || self.take_queued() {
            // A slash command or a queued prompt that ran the model without a tool call.
            self.push_hook_turn(None, true);
        } else {
            // No hook turn to end: a duplicate, or a queued turn not confirmed yet.
            self.stray_stops += 1;
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

    /// Drops dialog `i`; the main agent's dialog flag goes with its last main-agent dialog.
    fn remove_dialog(&mut self, i: usize) {
        let Some(p) = self.pending.remove(i) else {
            return;
        };
        if p.agent.is_none() && self.pending.iter().all(|q| q.agent.is_some()) {
            self.main_dialog = false;
        }
    }

    /// Mirante answered the oldest open dialog (the one Claude shows).
    fn apply_answer(&mut self, allowed: bool) -> Option<Transition> {
        if self.pending.is_empty() {
            return None;
        }
        let was_open = self.turn_open();
        if allowed {
            self.remove_dialog(0);
            return self.settle(was_open, "mirante:permission_allowed".into());
        }
        if was_open {
            // Claude ends the turn with "[Request interrupted by user for tool use]"; the
            // transcript's `turn_duration` closes it. Until then the dialog stands.
            return None;
        }
        // A side agent's dialog after the main turn: nothing else will end it.
        self.remove_dialog(0);
        self.settle(was_open, "mirante:permission_denied".into())
    }

    // --- Transcript stream ----------------------------------------------------------

    fn apply_transcript(&mut self, v: &Value) -> Option<Transition> {
        if is_sidechain(v) {
            return None;
        }
        let was_open = self.turn_open();
        match compact_marker(v) {
            // Only a change matters (an end with no compaction running changes nothing).
            Some(start) if start == self.compacting => return None,
            Some(start) => {
                self.compacting = start;
                let kind = if start { "compact" } else { v["subtype"].as_str().unwrap_or("local_command") };
                return self.settle(was_open, format!("transcript:{kind}"));
            }
            None if is_prompt(v) || is_task_notification_line(v) || turn_end_kind(v).is_some() => {
                self.compacting = false
            }
            None => {}
        }
        if is_prompt(v) || is_task_notification_line(v) {
            if self.t_open {
                return None;
            }
            let id = self.prompt_turn_number(v);
            if v["promptSource"] == "queued" {
                self.confirm_queued();
            } else {
                self.recount();
            }
            self.open_tx_turn(id);
            return self.settle(was_open, "transcript:prompt".into());
        }
        if is_command_wrapper(v) {
            if !self.t_open {
                self.t_command = true;
            }
            return None;
        }
        if v["type"] == "assistant" && !self.t_open && self.t_command {
            // The model answers the command: it runs as a turn.
            self.recount();
            self.open_tx_turn(self.t_turn + 1);
            return self.settle(was_open, "transcript:command".into());
        }
        if v["type"] == "queue-operation" && v["operation"] == "enqueue" && self.t_open {
            self.t_enqueued += 1;
            return None;
        }
        let kind = turn_end_kind(v)?;
        if !self.t_open && self.t_command {
            // A command turn whose answer line was not seen: its end still counts it.
            self.recount();
            self.open_tx_turn(self.t_turn + 1);
        }
        if !self.t_open {
            // The second end line of a closed turn, or the end of a turn this tail never saw
            // open (attached mid-turn, cleared context).
            return None;
        }
        self.close_tx_turn(kind == "stop_hook_summary");
        self.settle(was_open, format!("transcript:{kind}"))
    }

    /// The next transcript number, unless the prompt's text is the prompt of a later hook
    /// turn while the next number's hook turn had another text: the transcript never wrote
    /// a line for the turns in between (hook-only prompts), so skip them.
    fn prompt_turn_number(&self, v: &Value) -> u64 {
        let next = self.t_turn + 1;
        let Some(text) = first_text(&v["message"]["content"]).map(str::trim).filter(|s| !s.is_empty()) else {
            return next;
        };
        let differs = self.h_turns.iter().any(|t| t.id == next && t.prompt.as_deref().is_some_and(|p| p != text));
        if !differs {
            return next;
        }
        self.h_turns.iter().find(|t| t.id > next && t.prompt.as_deref() == Some(text)).map_or(next, |t| t.id)
    }

    fn open_tx_turn(&mut self, id: u64) {
        self.t_turn = id;
        self.t_open = true;
        self.t_enqueued = 0;
        self.t_command = false;
    }

    fn close_tx_turn(&mut self, summary: bool) {
        let id = self.t_turn;
        self.t_open = false;
        self.t_closed.push_back(TxTurn { id, enqueued: self.t_enqueued, queued_after: 0, summary });
        if self.t_closed.len() > HISTORY {
            self.t_closed.pop_front();
        }
        self.ended = self.ended.max(id);
        // A Stop expected late for an older turn will not come any more (lost).
        self.late_stops.retain(|t| *t >= id);
        // The flag names one turn: settled when the transcript closes that turn, dropped once
        // the transcript is past it, kept while the transcript is still behind it.
        match self.late_taken {
            Some(t) if t == id => {
                self.late_taken = None;
                self.settle_late_stop(id);
            }
            Some(t) if t < id => self.late_taken = None,
            _ => {}
        }
    }

    /// The transcript closed turn `id` while a `Stop` taken as late stood for it: that
    /// `Stop` was this turn's own (its predecessor's was lost). A `Stop` that closed the
    /// hook record since was the next turn's, and a prompt that came after the set-aside
    /// `Stop` was the next turn's prompt, not a mid-turn one.
    fn settle_late_stop(&mut self, id: u64) {
        let Some(t) = self.h_turns.back_mut().filter(|t| t.id == id) else {
            return;
        };
        t.next_stop = t.done;
        t.done = true;
        let dialog = std::mem::take(&mut self.main_dialog);
        let Some(j) = t.mids.iter().position(|m| !m.stopped && !m.counted) else {
            return;
        };
        let mut rest = t.mids.split_off(j).into_iter();
        while let Some(m) = rest.next() {
            if m.slash && !m.followed {
                self.slash_pending = true; // a local command: no turn
                continue;
            }
            self.push_hook_turn(m.prompt, false);
            self.slash_pending = false;
            // A dialog raised after that prompt is the new turn's.
            self.main_dialog = dialog && m.followed;
            if let Some(t) = self.h_turns.back_mut() {
                t.mids = rest.collect();
            }
            return;
        }
    }

    /// A `queued` line opens the turn after `t_turn`: one of that turn's mid-turn prompts
    /// was queued. If the hook stream already went past that turn without counting it,
    /// count it now.
    fn confirm_queued(&mut self) {
        let prev = self.t_turn;
        let Some(c) = self.t_closed.iter_mut().rev().find(|t| t.id == prev) else {
            return;
        };
        c.queued_after += 1;
        let Some(i) = self.h_turns.iter().position(|t| t.id == prev) else {
            return;
        };
        let t = &mut self.h_turns[i];
        if !t.done {
            return; // still in that turn: its PreToolUse / Stop will count it (take_queued)
        }
        let Some(m) = t.mids.iter_mut().find(|m| !m.counted) else {
            return;
        };
        m.counted = true;
        let next_stop = t.next_stop;
        if i + 1 == self.h_turns.len() {
            // Nothing after it yet but maybe its Stop, which then found no turn to end, or a
            // newer prompt (a pending slash command): either way it is over.
            let done = self.stray_stops > 0 || next_stop || self.slash_pending;
            self.push_hook_turn(None, done);
        } else {
            // Its events were taken as stray; the turns after it were numbered one too low.
            self.renumber_after(i, 1);
            let t = HookTurn { id: prev + 1, prompt: None, mids: Vec::new(), done: true, next_stop: false };
            self.h_turns.insert(i + 1, t);
            self.ended = self.ended.max(prev + 1);
        }
    }

    /// A non-queued prompt opens the turn after `t_turn`: mid-turn prompts of that turn the
    /// transcript does not account for were new prompts (that turn's `Stop` was lost, so the
    /// hook stream took them for mid-turn ones). Renumber the hook stream after them.
    fn recount(&mut self) {
        let prev = self.t_turn;
        let Some(slots) = self.tx_closed(prev).map(|c| c.mid_slots()) else {
            return;
        };
        if let Some(i) = self.h_turns.iter().position(|t| t.id == prev) {
            self.recount_record(i, slots);
        }
    }

    /// Hook record `i` holds more mid-turn prompts than its transcript turn accounts for
    /// (`slots`): the first extra one was the next turn's prompt (this turn's `Stop` was
    /// lost). Counts it as a turn; the prompts after it move to that turn. Returns whether
    /// one was counted.
    fn recount_record(&mut self, i: usize, slots: usize) -> bool {
        let prev = self.h_turns[i].id;
        // A slash command with no activity after it was a local command, not a turn.
        let Some(j) = (slots..self.h_turns[i].mids.len()).find(|&j| {
            let m = &self.h_turns[i].mids[j];
            !m.counted && !(m.slash && !m.followed)
        }) else {
            return false;
        };
        self.h_turns[i].mids[j].counted = true;
        self.renumber_after(i, 1);
        let last = i + 1 == self.h_turns.len();
        let t = &mut self.h_turns[i];
        // The hook events after that prompt were the new turn's: a `Stop` among them ended
        // it; the hook stream moving on to a later record did too.
        let done = !last || (t.done && t.mids[j].stopped);
        if last {
            let mids = t.mids.split_off(j + 1);
            let prompt = t.mids[j].prompt.clone();
            t.done = true;
            self.h_turns.push_back(HookTurn { id: self.h_turn, prompt, mids, done, next_stop: false });
            if self.h_turns.len() > HISTORY {
                self.h_turns.pop_front();
            }
        }
        if done {
            self.ended = self.ended.max(prev + 1);
        }
        true
    }

    /// Adds `shift` to the number of every hook turn after index `i` (and to `h_turn`).
    fn renumber_after(&mut self, i: usize, shift: u64) {
        let from = self.h_turns[i].id;
        self.h_turn += shift;
        for t in self.h_turns.iter_mut().skip(i + 1) {
            t.id += shift;
            if t.done {
                self.ended = self.ended.max(t.id);
            }
        }
        for t in &mut self.late_stops {
            if *t > from {
                *t += shift;
            }
        }
        if let Some(t) = self.late_taken.as_mut().filter(|t| **t > from) {
            *t += shift;
        }
    }
}

/// A main-chain transcript line that starts a compaction (`Some(true)`: the plain user line
/// `/compact …` Claude Code 2.1.285 writes when the command is submitted, with no `origin`)
/// or shows one over (`Some(false)`: the `system` `compact_boundary` line, or the command's
/// output, a `<local-command-stdout>` / `-stderr>` line, `system` `local_command` or user).
fn compact_marker(v: &Value) -> Option<bool> {
    if v["type"] == "system" && v["subtype"] == "compact_boundary" {
        return Some(false);
    }
    let text = match v["type"].as_str() {
        Some("system") if v["subtype"] == "local_command" => v["content"].as_str(),
        Some("user") if v["isMeta"] != true => first_text(&v["message"]["content"]),
        _ => None,
    }?
    .trim_start();
    if text.starts_with("<local-command-stdout>") || text.starts_with("<local-command-stderr>") {
        return Some(false);
    }
    let compact = v["type"] == "user"
        && is_slash_command(text)
        && text.strip_prefix("/compact").is_some_and(|rest| rest.chars().next().is_none_or(char::is_whitespace));
    compact.then_some(true)
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

/// A `UserPromptSubmit` prompt that is Claude Code's own `<task-notification>`: a
/// background agent finished and the main agent resumes on its own (S1). Not the person's
/// words: never shown as theirs, mined or labelled as the operator's.
pub fn is_task_notification(prompt: &str) -> bool {
    prompt.trim_start().starts_with("<task-notification>")
}

/// The main-chain user line Claude Code writes for a `<task-notification>` (2.1.285:
/// `promptSource: "system"`, `origin.kind: "task-notification"`). It starts a turn of the
/// main agent, as its `UserPromptSubmit` hook does, so both sources count it.
pub fn is_task_notification_line(v: &Value) -> bool {
    v["type"] == "user" && v["isMeta"] != true && !is_sidechain(v) && v["origin"]["kind"] == "task-notification"
}

/// The user line a model-running slash command writes in place of a prompt
/// (`<command-message>review is running…</command-message><command-name>/review…`). A local
/// command may write the same shape (older versions), so the line alone opens no turn.
fn is_command_wrapper(v: &Value) -> bool {
    v["type"] == "user"
        && v["isMeta"] != true
        && first_text(&v["message"]["content"])
            .is_some_and(|t| ["<command-name>", "<command-message>"].iter().any(|p| t.trim_start().starts_with(p)))
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

/// What Claude Code (2.1.285) writes, as the whole text of a user line, when Esc stops a
/// turn that was streaming text: no Stop hook and no `turn_duration` follow (Task 21 live
/// run). The deny path's "[Request interrupted by user for tool use]" is not this line:
/// that turn is closed by its `turn_duration`.
pub const INTERRUPTED: &str = "[Request interrupted by user]";

/// `Some(kind)` for the transcript's authoritative turn-end lines: the `system`
/// `turn_duration` / `stop_hook_summary` lines, and `interrupted` for the user line that is
/// exactly [`INTERRUPTED`].
pub fn turn_end_kind(v: &Value) -> Option<&str> {
    if v["type"] == "user" {
        return first_text(&v["message"]["content"]).filter(|t| t.trim() == INTERRUPTED).map(|_| "interrupted");
    }
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

    fn side_of(name: &str, agent: &str) -> Input {
        Input::Hook(HookEvent {
            agent_id: Some(agent.into()),
            agent_type: Some("general-purpose".into()),
            tool_name: Some("Bash".into()),
            ..HookEvent::named(name)
        })
    }
    fn prompt_hook(text: &str) -> Input {
        Input::Hook(HookEvent { prompt: Some(text.into()), ..HookEvent::named("UserPromptSubmit") })
    }
    fn notification_line() -> Input {
        Input::Transcript(serde_json::json!({
            "type": "user", "isSidechain": false, "promptSource": "system",
            "origin": {"kind": "task-notification", "producer": "session-task"},
            "message": {"role": "user", "content": "<task-notification>\n<task-id>a-1</task-id>\n<status>completed</status>\n</task-notification>"}
        }))
    }
    fn summary_line() -> Input {
        Input::Transcript(serde_json::json!({"type": "system", "subtype": "stop_hook_summary"}))
    }

    /// S1: background agents outlive the main turn. One that asks for permission after the
    /// main `Stop` keeps the task `NeedsYou` through the main agent's next turns (each
    /// started by a `<task-notification>`), until its own tool runs or it stops.
    #[test]
    fn a_background_agent_s_dialog_outlives_the_main_turns() {
        let mut e = StatusEngine::new();
        e.apply_all(&[
            hook("SessionStart"),
            prompt_hook("use 2 agents"),
            side_of("SubagentStart", "a-1"),
            side_of("SubagentStart", "a-2"),
            hook("Stop"),
        ]);
        assert_eq!(e.status(), Status::Idle);
        e.apply(&side_of("PermissionRequest", "a-1"));
        assert_eq!(e.status(), Status::NeedsYou);
        // a-2 finishes: Claude resumes on its own and ends that turn.
        e.apply_all(&[
            side_of("SubagentStop", "a-2"),
            notification_line(),
            prompt_hook("<task-notification>\n<task-id>a-2</task-id>"),
            summary_line(),
            hook("Stop"),
        ]);
        assert_eq!(e.status(), Status::NeedsYou, "the background dialog is still on screen");
        assert_eq!(e.pending_dialogs(), 1);
        // a-1 stops (denied in the terminal): its dialog is gone with it.
        e.apply(&side_of("SubagentStop", "a-1"));
        assert_eq!(e.status(), Status::Idle);
        assert_eq!(e.pending_dialogs(), 0);
    }

    /// Without `SubagentStart` (an older Claude) a side dialog still dies with the turn.
    #[test]
    fn a_side_dialog_of_an_unknown_agent_dies_with_the_turn() {
        let mut e = StatusEngine::new();
        e.apply_all(&[hook("SessionStart"), prompt_hook("go"), side_of("PermissionRequest", "a-9"), hook("Stop")]);
        assert_eq!(e.status(), Status::Idle);
    }

    /// The `<task-notification>` prompt line opens a transcript turn, as its hook does: both
    /// sources keep counting the same turns.
    #[test]
    fn a_task_notification_is_a_turn_in_both_sources() {
        let mut e = StatusEngine::new();
        let first =
            Input::Transcript(serde_json::json!({"type": "user", "promptSource": "sdk", "message": {"content": "go"}}));
        e.apply_all(&[hook("SessionStart"), first, prompt_hook("go"), summary_line(), hook("Stop")]);
        for (n, id) in [(2, "a-1"), (3, "a-2")] {
            e.apply_all(&[notification_line(), prompt_hook(&format!("<task-notification>\n<task-id>{id}</task-id>"))]);
            assert_eq!(e.status(), Status::Working);
            assert_eq!((e.h_turn, e.t_turn), (n, n), "the transcript counts the notification turns too");
            e.apply_all(&[summary_line(), hook("Stop")]);
            assert_eq!(e.status(), Status::Idle);
        }
    }

    #[test]
    fn task_notification_prompts_are_recognized() {
        assert!(is_task_notification("<task-notification>\n<task-id>x</task-id>"));
        assert!(is_task_notification("  <task-notification>"));
        assert!(!is_task_notification("please read <task-notification>"));
        assert!(!is_task_notification("hello"));
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
        assert!(e.apply_all(&[hook("SubagentStop"), hook("SomethingNew")]).is_empty());
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

    /// Task 21 live run (2.1.285): Esc while Claude streams a text answer writes only the
    /// user line "[Request interrupted by user]": no Stop hook, no `turn_duration`. The
    /// turn must still end, or the board says `working` until the next turn's Stop.
    #[test]
    fn an_interrupted_text_turn_is_closed_by_the_interrupt_line() {
        let mut e = StatusEngine::new();
        e.apply_all(&[hook("SessionStart"), hook("UserPromptSubmit")]);
        assert_eq!(e.status(), Status::Working);
        let t = e.apply_all(&[
            prompt(),
            tx(json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "um\ndois\ntrês"}]}})),
            tx(json!({"type": "user", "interruptedMessageId": "msg_1", "message": {"content": [{"type": "text", "text": "[Request interrupted by user]"}]}})),
        ]);
        assert_eq!(
            t,
            vec![Transition { from: Status::Working, to: Status::Idle, cause: "transcript:interrupted".into() }]
        );
        assert!(!e.turn_open());
        // A `turn_duration` after it (if a version writes one) is the second end line of a
        // closed turn: nothing changes.
        assert_eq!(e.apply(&system("turn_duration")), None);
        // A side agent's interrupt line never ends the main turn.
        let mut e = StatusEngine::new();
        e.apply_all(&[hook("UserPromptSubmit"), prompt()]);
        let side = tx(
            json!({"type": "user", "isSidechain": true, "message": {"content": [{"type": "text", "text": "[Request interrupted by user]"}]}}),
        );
        assert_eq!(e.apply(&side), None);
        assert_eq!(e.status(), Status::Working);
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

    /// Without prompt texts a hook-only turn looks exactly like a transcript lagging a whole
    /// turn (the prompt line would then be turn 1's); the texts tell them apart.
    #[test]
    fn hook_only_turn_does_not_desync_later_transcript_ends() {
        let ups = |p: &str| Input::Hook(HookEvent { prompt: Some(p.into()), ..HookEvent::named("UserPromptSubmit") });
        let mut e = StatusEngine::new();
        // Turn 1 has no transcript prompt line (e.g. a hook-only prompt).
        e.apply_all(&[ups("blocked prompt"), hook("Stop")]);
        // Turn 2 is the deny path: only the transcript can close it.
        e.apply_all(&[ups("touch s3-denied.txt"), hook("PreToolUse"), hook("PermissionRequest"), prompt()]);
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

    // --- Several pending dialogs ---------------------------------------------------------

    fn perm(id: &str) -> Input {
        tool("PermissionRequest", id)
    }

    #[test]
    fn two_dialogs_are_answered_oldest_first_and_need_you_until_both_are_gone() {
        let mut e = StatusEngine::new();
        e.apply_all(&[ups("go"), tool("PreToolUse", "t1"), perm("t1"), tool("PreToolUse", "t2"), perm("t2")]);
        assert_eq!((e.status(), e.pending_dialogs()), (Status::NeedsYou, 2));
        // Mirante answers the older one: the newer still waits.
        assert_eq!(e.apply(&Input::PermissionAnswered { allowed: true }), None);
        assert_eq!((e.status(), e.pending_dialogs()), (Status::NeedsYou, 1));
        // The answered tool finishing is plain progress; the newer dialog still stands.
        assert_eq!(e.apply(&tool("PostToolUse", "t1")), None);
        assert_eq!(e.status(), Status::NeedsYou);
        let t = e.apply(&tool("PostToolUse", "t2")).unwrap();
        assert_eq!((t.from, t.to), (Status::NeedsYou, Status::Working));
        assert_eq!(e.pending_dialogs(), 0);
    }

    #[test]
    fn post_tool_use_clears_only_its_own_dialog() {
        let mut e = StatusEngine::new();
        e.apply_all(&[ups("go"), tool("PreToolUse", "t1"), perm("t1"), tool("PreToolUse", "t2"), perm("t2")]);
        assert_eq!(e.apply(&tool("PostToolUse", "t2")), None);
        assert_eq!((e.status(), e.pending_dialogs()), (Status::NeedsYou, 1));
        assert_eq!(e.apply(&tool("PostToolUse", "t1")).unwrap().to, Status::Working);
    }

    #[test]
    fn tool_use_id_for_borrows_from_the_matching_pre_tool_use() {
        let mut e = StatusEngine::new();
        e.apply_all(&[ups("go"), tool("PreToolUse", "t1")]);
        let bare = HookEvent { tool_name: Some("Bash".into()), ..HookEvent::named("PermissionRequest") };
        assert_eq!(e.tool_use_id_for(&bare).as_deref(), Some("t1"));
        let own = HookEvent { tool_use_id: Some("t9".into()), ..bare.clone() };
        assert_eq!(e.tool_use_id_for(&own).as_deref(), Some("t9"));
        e.apply(&tool("PostToolUse", "t1"));
        assert_eq!(e.tool_use_id_for(&bare), None);
    }

    #[test]
    fn post_tool_use_failure_clears_its_own_dialog_and_tool_bookkeeping() {
        let mut e = StatusEngine::new();
        e.apply_all(&[ups("go"), tool("PreToolUse", "t1"), perm("t1"), tool("PreToolUse", "t2"), perm("t2")]);
        assert_eq!(e.apply(&tool("PostToolUseFailure", "t2")), None);
        assert_eq!((e.status(), e.pending_dialogs()), (Status::NeedsYou, 1));
        let t = e.apply(&tool("PostToolUseFailure", "t1")).unwrap();
        assert_eq!((t.to, t.cause.as_str()), (Status::Working, "hook:PostToolUseFailure"));
        let bare = HookEvent { tool_name: Some("Bash".into()), ..HookEvent::named("PermissionRequest") };
        assert_eq!(e.tool_use_id_for(&bare), None, "the failed tool's id is forgotten");
    }

    #[test]
    fn stop_failure_ends_the_turn_like_stop() {
        let mut e = StatusEngine::new();
        e.apply_all(&[start("startup"), ups("go"), tool("PreToolUse", "t1")]);
        assert_eq!(e.status(), Status::Working);
        let t = e.apply(&hook("StopFailure")).unwrap();
        assert_eq!((t.to, t.cause.as_str()), (Status::Idle, "hook:StopFailure"));
        assert!(!e.turn_open());
        // A side agent's StopFailure moves no turn boundary.
        e.apply(&ups("again"));
        assert_eq!(e.apply(&side_tool("StopFailure", "a-1")), None);
        assert_eq!(e.status(), Status::Working);
    }

    #[test]
    fn a_duplicate_permission_request_is_one_dialog() {
        let mut e = StatusEngine::new();
        e.apply_all(&[ups("go"), tool("PreToolUse", "t1"), perm("t1"), perm("t1")]);
        assert_eq!(e.pending_dialogs(), 1);
        assert_eq!(e.apply(&tool("PostToolUse", "t1")).unwrap().to, Status::Working);
    }

    #[test]
    fn the_turn_end_clears_every_dialog() {
        let mut e = StatusEngine::new();
        e.apply_all(&[
            ups("go"),
            typed("go"),
            tool("PreToolUse", "t1"),
            perm("t1"),
            side_tool("PermissionRequest", "a-1"),
        ]);
        assert_eq!(e.pending_dialogs(), 2);
        assert_eq!(e.apply(&Input::PermissionAnswered { allowed: false }), None);
        assert_eq!(e.apply(&system("turn_duration")).unwrap().to, Status::Idle);
        assert_eq!(e.pending_dialogs(), 0);
    }

    #[test]
    fn side_agent_dialogs_after_the_turn_are_denied_one_by_one() {
        let mut e = StatusEngine::new();
        e.apply_all(&[
            ups("go"),
            hook("Stop"),
            side_tool("PermissionRequest", "a-1"),
            side_tool("PermissionRequest", "a-2"),
        ]);
        assert_eq!(e.pending_dialogs(), 2);
        assert_eq!(e.apply(&Input::PermissionAnswered { allowed: false }), None);
        assert_eq!(e.status(), Status::NeedsYou);
        assert_eq!(e.apply(&Input::PermissionAnswered { allowed: false }).unwrap().to, Status::Idle);
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

    /// The live `/compact keep the plan` (Task 21, Claude Code 2.1.285), in file order: the
    /// plain user line at submission, then ~19 s later the boundary, the summary, the
    /// caveat, the command echo and its output. Hooks: `PreCompact` before, `SessionStart`
    /// (`compact`) after.
    fn live_compact() -> Vec<Value> {
        vec![
            json!({"isSidechain": false, "type": "user", "message": {"role": "user", "content": "/compact keep the plan"}}),
            json!({"isSidechain": false, "type": "system", "subtype": "compact_boundary", "content": "Conversation compacted",
                   "level": "info", "compactMetadata": {"trigger": "manual", "durationMs": 19289}}),
            json!({"isSidechain": false, "type": "user", "isVisibleInTranscriptOnly": true, "isCompactSummary": true,
                   "message": {"role": "user", "content": "This session is being continued from a previous conversation …"}}),
            json!({"isSidechain": false, "type": "user", "isMeta": true,
                   "message": {"role": "user", "content": "<local-command-caveat>The command below was run directly …"}}),
            json!({"isSidechain": false, "type": "user", "message": {"role": "user",
                   "content": "<command-name>/compact</command-name>\n            <command-message>compact</command-message>\n            <command-args>keep the plan</command-args>"}}),
            json!({"isSidechain": false, "type": "user", "message": {"role": "user",
                   "content": "<local-command-stdout>\u{1b}[2mCompacted (ctrl+o to see full summary)\u{1b}[22m</local-command-stdout>"}}),
        ]
    }

    #[test]
    fn a_compaction_is_working_until_it_is_over_whichever_source_says_so() {
        let tx = |v: &Value| Input::Transcript(v.clone());
        let lines = live_compact();
        // Both sources, hooks first.
        let mut e = StatusEngine::new();
        e.apply(&start("startup"));
        let mut inputs = vec![ups("/compact keep the plan"), hook("PreCompact")];
        inputs.extend(lines.iter().map(tx));
        inputs.push(start("compact"));
        let got = run(&mut e, &inputs);
        assert_eq!(got, pairs(&[(Status::Working, "hook:PreCompact"), (Status::Idle, "transcript:compact_boundary")]));
        assert!(!e.turn_open(), "a compaction is not a turn");
        // Transcript only (the hooks were lost): the plain line starts it, the boundary ends it.
        let mut e = StatusEngine::new();
        let got = run(&mut e, &lines.iter().map(tx).collect::<Vec<_>>());
        assert_eq!(
            got,
            pairs(&[(Status::Working, "transcript:compact"), (Status::Idle, "transcript:compact_boundary")])
        );
        // A failed or cancelled compaction writes no boundary: its output line ends it.
        let mut e = StatusEngine::new();
        let err = json!({"type": "system", "subtype": "local_command",
                         "content": "<local-command-stderr>Error: Compaction canceled.</local-command-stderr>"});
        let got = run(&mut e, &[tx(&lines[0]), tx(&err)]);
        assert_eq!(got, pairs(&[(Status::Working, "transcript:compact"), (Status::Idle, "transcript:local_command")]));
        // Hooks only.
        let mut e = StatusEngine::new();
        let got = run(&mut e, &[hook("PreCompact"), start("compact")]);
        assert_eq!(got, pairs(&[(Status::Working, "hook:PreCompact"), (Status::Idle, "hook:SessionStart")]));
        // The transcript ahead of the hooks: the late PreCompact starts it again, the
        // SessionStart after it ends it.
        let mut e = StatusEngine::new();
        let mut inputs: Vec<Input> = lines.iter().map(tx).collect();
        inputs.extend([hook("PreCompact"), start("compact")]);
        assert_eq!(e.apply_all(&inputs).last().map(|t| t.to), Some(Status::Idle));
        assert_eq!(e.status(), Status::Idle);
        // A prompt typed meanwhile ends it too (a normal turn follows).
        let mut e = StatusEngine::new();
        e.apply_all(&[hook("PreCompact"), ups("next")]);
        assert_eq!(e.apply(&hook("Stop")).map(|t| t.to), Some(Status::Idle));
    }

    #[test]
    fn an_auto_compaction_inside_a_turn_keeps_it_working_and_ends_with_the_turn() {
        let mut e = StatusEngine::new();
        assert_eq!(e.apply(&ups("go")).map(|t| t.to), Some(Status::Working));
        assert!(e.apply_all(&[hook("PreCompact"), start("compact")]).is_empty());
        assert_eq!(e.status(), Status::Working);
        assert_eq!(e.apply(&hook("Stop")).map(|t| t.to), Some(Status::Idle));
        // A lost SessionStart: the turn end still ends the compaction.
        let mut e = StatusEngine::new();
        e.apply_all(&[ups("go"), hook("PreCompact"), hook("Stop")]);
        assert_eq!(e.status(), Status::Idle);
    }

    #[test]
    fn only_the_compact_command_line_starts_a_compaction() {
        for other in ["/compactor", "/compact-x now", "compact", "/context"] {
            let mut e = StatusEngine::new();
            let line = json!({"type": "user", "message": {"role": "user", "content": other}});
            assert_eq!(e.apply(&Input::Transcript(line)), None, "{other:?}");
        }
        let meta = json!({"type": "user", "isMeta": true, "message": {"content": "/compact"}});
        let side = json!({"type": "user", "isSidechain": true, "message": {"content": "/compact"}});
        let mut e = StatusEngine::new();
        assert!(e.apply_all(&[Input::Transcript(meta), Input::Transcript(side)]).is_empty());
        // A side agent's PreCompact is not the session compacting.
        let side_pre = Input::Hook(HookEvent { agent_id: Some("a1".into()), ..HookEvent::named("PreCompact") });
        assert_eq!(e.apply(&side_pre), None);
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

/// Reviewer probes (round 3): model-running slash commands (`/review`, whose transcript
/// prompt line is a `<command-message>` wrapper) and a late-Stop flag going stale across a
/// deny-path turn.
#[cfg(test)]
mod probe_tests {
    use super::*;
    use serde_json::json;

    fn hook(name: &str) -> Input {
        Input::Hook(HookEvent::named(name))
    }
    fn ups(p: &str) -> Input {
        Input::Hook(HookEvent { prompt: Some(p.into()), ..HookEvent::named("UserPromptSubmit") })
    }
    fn typed(t: &str) -> Input {
        Input::Transcript(
            json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "typed", "message": {"content": t}}),
        )
    }
    fn queued(t: &str) -> Input {
        Input::Transcript(
            json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "queued", "message": {"content": t}}),
        )
    }
    /// The user line Claude Code writes for a model-running slash command, then the model's
    /// first answer line (2.1.285).
    fn wrapper(cmd: &str) -> Input {
        let text = format!("<command-message>{cmd} is running…</command-message>\n<command-name>/{cmd}</command-name>");
        Input::Transcript(
            json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "typed", "message": {"content": text}}),
        )
    }
    fn system(s: &str) -> Input {
        Input::Transcript(json!({"type": "system", "subtype": s}))
    }
    fn trace(e: &mut StatusEngine, inputs: &[Input]) -> Vec<Status> {
        inputs
            .iter()
            .map(|i| {
                e.apply(i);
                e.status()
            })
            .collect()
    }

    /// A: model-running slash command, permission denied: the deny `turn_duration` closes it
    /// and the next turn works normally.
    #[test]
    fn a_slash_command_deny_is_closed_by_turn_duration() {
        let mut e = StatusEngine::new();
        let t = trace(
            &mut e,
            &[
                ups("/review"),
                wrapper("review"),
                hook("PreToolUse"),
                hook("PermissionRequest"),
                system("turn_duration"),
                typed("next"),
                ups("next"),
                hook("PreToolUse"),
                hook("PostToolUse"),
                hook("Stop"),
                system("stop_hook_summary"),
                system("turn_duration"),
            ],
        );
        assert_eq!(t[4], Status::Idle, "deny turn_duration closes the slash turn: {t:?}");
        assert_eq!(t[7], Status::Working, "turn 2 works: {t:?}");
        assert_eq!(t[9], Status::Idle, "turn 2 idles on its Stop: {t:?}");
    }

    /// B: slash turn, then a deny-path turn (transcript prompt first): its `turn_duration`
    /// must close it.
    #[test]
    fn b_slash_command_then_deny_turn() {
        let mut e = StatusEngine::new();
        let t = trace(
            &mut e,
            &[
                ups("/review"),
                wrapper("review"),
                hook("PreToolUse"),
                hook("Stop"),
                system("stop_hook_summary"),
                system("turn_duration"),
                typed("two"),
                ups("two"),
                hook("PreToolUse"),
                hook("PermissionRequest"),
                system("turn_duration"),
                typed("three"),
                ups("three"),
                hook("PreToolUse"),
                hook("PostToolUse"),
                hook("Stop"),
                system("stop_hook_summary"),
                system("turn_duration"),
            ],
        );
        assert_eq!(t[10], Status::Idle, "deny turn_duration closes turn two: {t:?}");
        assert_eq!(t[13], Status::Working, "turn three works: {t:?}");
        assert_eq!(t[15], Status::Idle, "turn three idles on its Stop: {t:?}");
    }

    /// C: slash turn with a message queued mid-turn: the queued turn is Working.
    #[test]
    fn c_slash_command_then_queued_turn() {
        let mut e = StatusEngine::new();
        let t = trace(
            &mut e,
            &[
                ups("/review"),
                wrapper("review"),
                hook("PreToolUse"),
                ups("then BANANA"),
                hook("PostToolUse"),
                hook("Stop"),
                system("stop_hook_summary"),
                system("turn_duration"),
                queued("then BANANA"),
                hook("PreToolUse"),
                hook("PostToolUse"),
                hook("Stop"),
                system("stop_hook_summary"),
                system("turn_duration"),
            ],
        );
        assert_eq!(t[8], Status::Working, "queued turn works: {t:?}");
        assert_eq!(t[11], Status::Idle, "queued turn idles on its Stop: {t:?}");
    }

    /// D: slash turn, then a normal turn whose summary comes before its Stop.
    #[test]
    fn d_slash_command_then_summary_first_turn() {
        let mut e = StatusEngine::new();
        let t = trace(
            &mut e,
            &[
                ups("/review"),
                wrapper("review"),
                hook("PreToolUse"),
                hook("Stop"),
                system("stop_hook_summary"),
                system("turn_duration"),
                typed("two"),
                ups("two"),
                system("stop_hook_summary"),
                system("turn_duration"),
                hook("Stop"),
                typed("three"),
                ups("three"),
                hook("Stop"),
                system("stop_hook_summary"),
                system("turn_duration"),
            ],
        );
        assert_eq!(t[8], Status::Idle, "{t:?}");
        assert_eq!(t[12], Status::Working, "{t:?}");
        assert_eq!(t[13], Status::Idle, "turn three idles on its own Stop: {t:?}");
    }

    /// E2: a queued turn whose Stop comes late, then a deny-path turn (no Stop, no summary),
    /// then another queued turn with a late Stop: that late Stop must not idle it.
    #[test]
    fn e2_late_stop_flag_does_not_survive_a_deny_turn() {
        let mut e = StatusEngine::new();
        let t = trace(
            &mut e,
            &[
                ups("a"),
                typed("a"),
                ups("b"),
                system("stop_hook_summary"),
                system("turn_duration"),
                queued("b"),
                hook("Stop"),
                hook("PreToolUse"),
                hook("PermissionRequest"),
                system("turn_duration"),
                ups("d"),
                typed("d"),
                ups("e"),
                system("stop_hook_summary"),
                system("turn_duration"),
                queued("e"),
                hook("Stop"),
                hook("PreToolUse"),
            ],
        );
        assert_eq!(t[9], Status::Idle, "{t:?}");
        assert_eq!(t[16], Status::Working, "late Stop of d must not idle queued e: {t:?}");
    }
}

/// Systematic interleaving harness: canonical, well-formed session scripts, each delivered
/// in many orders between the two sources (hooks and transcript keep their own order), with
/// at most one lost `Stop` and one duplicated hook event, checked against invariants after
/// every delivered event.
///
/// Delivery orders: the hook stream shifted against the transcript by -2..=+2 turns in
/// quarter-turn steps with jitter (hooks entirely ahead or behind), or a seeded per-event
/// random merge whose drift is bounded to [`MAX_DRIFT`] turns. `SessionStart` is always
/// delivered first and `SessionEnd` last (no prompt can precede the first, nothing follows
/// the second).
///
/// Invariants ("turn k is over" = an end event of k, or any event of a later turn, was
/// delivered: each source delivers in order, so a later event proves the earlier turn ended):
///
/// - I-a: once every event of turns <= k (both sources) is delivered and none of a later
///   turn, the status is `Idle` (`Ended` after `SessionEnd`).
/// - I-b: while a turn's starts from both sources are delivered (hook prompt or the hook
///   event that opens a slash / queued turn; transcript prompt, queued line or command
///   answer) and it is not over, the status is not `Idle`. Holds with a lost `Stop` too.
///   One relaxation, [`dup_stop_before_queued`]: a duplicated `Stop` of the turn before a
///   queued turn.
/// - I-c: `NeedsYou` only while a delivered `PermissionRequest` is unresolved: its
///   `PostToolUse` not delivered and its turn not over.
/// - I-d: a hook `Stop` delivered while no later-turn event is delivered idles the session,
///   except at most one turn after a lost `Stop` (the `Stop` after a lost one is first taken
///   for the lost one, late; the transcript then closes that turn).
/// - Every run ends `Ended` (`SessionEnd` is last).
///
/// Local commands (`/context`) are not turns: they only have to stay `Idle` (I-a).
#[cfg(test)]
mod interleaving_tests {
    use super::*;
    use serde_json::json;

    /// Scripts x interleavings per script (keep the run well under a few seconds).
    const SCRIPTS: u64 = 200;
    const INTERLEAVINGS: u64 = 50;
    const MAX_TURNS: u64 = 7;
    /// Bound on how many turns one source may run ahead of the other in a random merge.
    const MAX_DRIFT: usize = 2;

    /// xorshift64*: tiny, deterministic, no dependency.
    struct Rng(u64);
    impl Rng {
        fn new(seed: u64) -> Rng {
            Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
        }
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
        fn percent(&mut self, p: u64) -> bool {
            self.below(100) < p
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Kind {
        /// Prompt, answer.
        Plain,
        /// Prompt, one tool call.
        Tool,
        /// Tool call behind an allowed permission dialog.
        Allow,
        /// Permission denied: no `Stop`, no `stop_hook_summary`.
        Deny,
        /// A second prompt mid-turn, absorbed at the tool boundary.
        Steer,
        /// A second prompt mid-turn, run as the next turn ([`Kind::QueuedRun`]).
        Queued,
        /// The queued prompt's turn: no `UserPromptSubmit`, a `queued` transcript line.
        QueuedRun,
        /// `/review`: a model-running slash command with a tool call.
        Slash,
        /// `/review` whose tool call is denied.
        SlashDeny,
        /// `/context` (2.1.285): no hook at all, `system` `local_command` lines.
        Local,
        /// `/context` from a version that fires `UserPromptSubmit` for it.
        LocalUps,
        /// A tool turn followed by a stray `PreToolUse` (no `agent_id`) after its `Stop`.
        Stray,
        /// Two tool calls in one turn, each behind its own allowed dialog (sequential).
        TwoDialogs,
        /// The API call fails: the tool reports `PostToolUseFailure`, the turn ends with
        /// `StopFailure` instead of `Stop`.
        Failure,
        /// `/compact` (Task 21 live run): `UserPromptSubmit`, `PreCompact`, then
        /// `SessionStart` (`compact`); the plain `/compact` user line, then the boundary, the
        /// summary, the echo and its output. No turn; `Working` while it runs.
        Compact,
    }

    const PICK: [Kind; 14] = [
        Kind::Plain,
        Kind::Tool,
        Kind::Allow,
        Kind::Deny,
        Kind::Steer,
        Kind::Queued,
        Kind::Slash,
        Kind::SlashDeny,
        Kind::Local,
        Kind::LocalUps,
        Kind::Stray,
        Kind::TwoDialogs,
        Kind::Failure,
        Kind::Compact,
    ];

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Role {
        /// The hook event that opens the turn.
        HookStart,
        /// The transcript line that opens the turn.
        TxStart,
        Stop,
        /// `stop_hook_summary` / `turn_duration`.
        TxEnd,
        Perm,
        /// The `PostToolUse` answering the turn's dialog.
        PermDone,
        Other,
    }

    #[derive(Debug, Clone)]
    struct Ev {
        input: Input,
        turn: usize,
        hook: bool,
        role: Role,
        label: String,
    }

    fn hook_ev(turn: usize, role: Role, label: &str, ev: HookEvent) -> Ev {
        Ev { input: Input::Hook(ev), turn, hook: true, role, label: format!("H {label}") }
    }
    fn tx_ev(turn: usize, role: Role, label: &str, v: Value) -> Ev {
        Ev { input: Input::Transcript(v), turn, hook: false, role, label: format!("T {label}") }
    }
    fn ups(text: &str) -> HookEvent {
        HookEvent { prompt: Some(text.into()), ..HookEvent::named("UserPromptSubmit") }
    }
    fn tool(name: &str, id: &str) -> HookEvent {
        HookEvent { tool_name: Some("Bash".into()), tool_use_id: Some(id.into()), ..HookEvent::named(name) }
    }
    fn user(source: &str, text: &str) -> Value {
        json!({"type": "user", "origin": {"kind": "human"}, "promptSource": source, "message": {"content": text}})
    }
    fn assistant(stop: &str) -> Value {
        json!({"type": "assistant", "message": {"stop_reason": stop, "content": []}})
    }
    fn system(subtype: &str) -> Value {
        json!({"type": "system", "subtype": subtype})
    }
    fn op(operation: &str) -> Value {
        json!({"type": "queue-operation", "operation": operation, "content": "queued"})
    }
    fn tool_result() -> Value {
        json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "x"}]}})
    }

    /// One canonical turn: its hook events and its transcript lines, each in source order.
    fn turn_events(k: usize, kind: Kind, rng: &mut Rng) -> (Vec<Ev>, Vec<Ev>) {
        use Role::*;
        let p = format!("prompt {k}");
        let q = format!("prompt {}", k + 1); // the queued prompt is the next turn's
        let id = format!("toolu_{k}");
        let mut h = Vec::new();
        let mut t = Vec::new();
        let stop = |h: &mut Vec<Ev>| h.push(hook_ev(k, Stop, "Stop", HookEvent::named("Stop")));
        let ends = |t: &mut Vec<Ev>| {
            t.push(tx_ev(k, Other, "assistant end_turn", assistant("end_turn")));
            t.push(tx_ev(k, TxEnd, "stop_hook_summary", system("stop_hook_summary")));
            t.push(tx_ev(k, TxEnd, "turn_duration", system("turn_duration")));
        };
        let tool_use = |h: &mut Vec<Ev>, t: &mut Vec<Ev>| {
            t.push(tx_ev(k, Other, "assistant tool_use", assistant("tool_use")));
            h.push(hook_ev(k, Other, "PreToolUse", tool("PreToolUse", &id)));
        };
        let wrapper = json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "typed",
            "message": {"content": "<command-message>review is running…</command-message>\n<command-name>/review</command-name>"}});
        let meta = json!({"type": "user", "isMeta": true, "message": {"content": "Review the diff ..."}});
        match kind {
            Kind::Plain | Kind::Tool | Kind::Allow | Kind::Stray | Kind::Deny => {
                h.push(hook_ev(k, HookStart, "UPS", ups(&p)));
                t.push(tx_ev(k, TxStart, "prompt", user("typed", &p)));
                if kind != Kind::Plain {
                    tool_use(&mut h, &mut t);
                }
                if matches!(kind, Kind::Allow | Kind::Deny) {
                    h.push(hook_ev(k, Perm, "PermissionRequest", tool("PermissionRequest", &id)));
                }
                if kind == Kind::Deny {
                    let interrupted = json!({"type": "user", "message": {"content": [{"type": "text",
                        "text": "[Request interrupted by user for tool use]"}]}});
                    t.push(tx_ev(k, Other, "interrupted", interrupted));
                    t.push(tx_ev(k, TxEnd, "turn_duration", system("turn_duration")));
                    return (h, t);
                }
                if kind != Kind::Plain {
                    let role = if kind == Kind::Allow { PermDone } else { Other };
                    h.push(hook_ev(k, role, "PostToolUse", tool("PostToolUse", &id)));
                    t.push(tx_ev(k, Other, "tool_result", tool_result()));
                }
                ends(&mut t);
                stop(&mut h);
                if kind == Kind::Stray {
                    h.push(hook_ev(k, Other, "PreToolUse (stray)", HookEvent::named("PreToolUse")));
                }
            }
            Kind::Failure => {
                h.push(hook_ev(k, HookStart, "UPS", ups(&p)));
                t.push(tx_ev(k, TxStart, "prompt", user("typed", &p)));
                tool_use(&mut h, &mut t);
                // Half the time the failing tool ran behind an allowed dialog.
                let asked = rng.percent(50);
                if asked {
                    h.push(hook_ev(k, Perm, "PermissionRequest", tool("PermissionRequest", &id)));
                }
                let role = if asked { PermDone } else { Other };
                h.push(hook_ev(k, role, "PostToolUseFailure", tool("PostToolUseFailure", &id)));
                t.push(tx_ev(k, Other, "tool_result", tool_result()));
                ends(&mut t);
                h.push(hook_ev(k, Stop, "StopFailure", HookEvent::named("StopFailure")));
            }
            Kind::TwoDialogs => {
                h.push(hook_ev(k, HookStart, "UPS", ups(&p)));
                t.push(tx_ev(k, TxStart, "prompt", user("typed", &p)));
                for suffix in ["a", "b"] {
                    let id = format!("toolu_{k}{suffix}");
                    t.push(tx_ev(k, Other, "assistant tool_use", assistant("tool_use")));
                    h.push(hook_ev(k, Other, "PreToolUse", tool("PreToolUse", &id)));
                    h.push(hook_ev(k, Perm, "PermissionRequest", tool("PermissionRequest", &id)));
                    h.push(hook_ev(k, PermDone, "PostToolUse", tool("PostToolUse", &id)));
                    t.push(tx_ev(k, Other, "tool_result", tool_result()));
                }
                ends(&mut t);
                stop(&mut h);
            }
            Kind::Steer => {
                h.push(hook_ev(k, HookStart, "UPS", ups(&p)));
                t.push(tx_ev(k, TxStart, "prompt", user("typed", &p)));
                tool_use(&mut h, &mut t);
                h.push(hook_ev(k, Other, "UPS (steer)", ups(&format!("steer {k}"))));
                t.push(tx_ev(k, Other, "enqueue", op("enqueue")));
                t.push(tx_ev(k, Other, "remove absorbed", op("remove")));
                h.push(hook_ev(k, Other, "PostToolUse", tool("PostToolUse", &id)));
                t.push(tx_ev(k, Other, "tool_result", tool_result()));
                t.push(tx_ev(
                    k,
                    Other,
                    "attachment",
                    json!({"type": "attachment", "attachment": {"type": "queued_command"}}),
                ));
                ends(&mut t);
                stop(&mut h);
            }
            Kind::Queued => {
                h.push(hook_ev(k, HookStart, "UPS", ups(&p)));
                t.push(tx_ev(k, TxStart, "prompt", user("typed", &p)));
                let with_tool = rng.percent(50);
                if with_tool {
                    tool_use(&mut h, &mut t);
                }
                h.push(hook_ev(k, Other, "UPS (queued)", ups(&q)));
                t.push(tx_ev(k, Other, "enqueue", op("enqueue")));
                if with_tool {
                    h.push(hook_ev(k, Other, "PostToolUse", tool("PostToolUse", &id)));
                    t.push(tx_ev(k, Other, "tool_result", tool_result()));
                }
                t.push(tx_ev(k, Other, "assistant end_turn", assistant("end_turn")));
                t.push(tx_ev(k, Other, "dequeue", op("dequeue")));
                t.push(tx_ev(k, TxEnd, "stop_hook_summary", system("stop_hook_summary")));
                t.push(tx_ev(k, TxEnd, "turn_duration", system("turn_duration")));
                stop(&mut h);
            }
            Kind::QueuedRun => {
                t.push(tx_ev(k, TxStart, "queued prompt", user("queued", &p)));
                if rng.percent(50) {
                    // The first hook event of a queued turn is what opens it in the hooks.
                    t.push(tx_ev(k, Other, "assistant tool_use", assistant("tool_use")));
                    h.push(hook_ev(k, HookStart, "PreToolUse", tool("PreToolUse", &id)));
                    h.push(hook_ev(k, Other, "PostToolUse", tool("PostToolUse", &id)));
                    t.push(tx_ev(k, Other, "tool_result", tool_result()));
                    ends(&mut t);
                    stop(&mut h);
                } else {
                    ends(&mut t);
                    h.push(hook_ev(k, Stop, "Stop", HookEvent::named("Stop")));
                }
            }
            Kind::Slash | Kind::SlashDeny => {
                h.push(hook_ev(k, Other, "UPS /review", ups("/review")));
                t.push(tx_ev(k, Other, "command wrapper", wrapper));
                t.push(tx_ev(k, Other, "isMeta prompt", meta));
                t.push(tx_ev(k, TxStart, "assistant tool_use", assistant("tool_use")));
                h.push(hook_ev(k, HookStart, "PreToolUse", tool("PreToolUse", &id)));
                if kind == Kind::SlashDeny {
                    h.push(hook_ev(k, Perm, "PermissionRequest", tool("PermissionRequest", &id)));
                    t.push(tx_ev(k, TxEnd, "turn_duration", system("turn_duration")));
                    return (h, t);
                }
                h.push(hook_ev(k, Other, "PostToolUse", tool("PostToolUse", &id)));
                t.push(tx_ev(k, Other, "tool_result", tool_result()));
                ends(&mut t);
                stop(&mut h);
            }
            Kind::Local | Kind::LocalUps => {
                if kind == Kind::LocalUps {
                    h.push(hook_ev(k, Other, "UPS /context", ups("/context")));
                }
                let echo = json!({"type": "system", "subtype": "local_command",
                    "content": "<command-name>/context</command-name>\n<command-message>context</command-message>"});
                let out = json!({"type": "system", "subtype": "local_command",
                    "content": "<local-command-stdout>Context Usage</local-command-stdout>"});
                let meta = json!({"type": "user", "isMeta": true, "message": {"content": "## Context Usage"}});
                t.push(tx_ev(k, Other, "local_command echo", echo));
                t.push(tx_ev(k, Other, "local_command stdout", out));
                t.push(tx_ev(k, Other, "isMeta context", meta));
            }
            Kind::Compact => {
                h.push(hook_ev(k, Other, "UPS /compact", ups("/compact keep")));
                h.push(hook_ev(k, Other, "PreCompact", HookEvent::named("PreCompact")));
                h.push(hook_ev(
                    k,
                    Other,
                    "SessionStart compact",
                    HookEvent { source: Some("compact".into()), ..HookEvent::named("SessionStart") },
                ));
                let line = |text: &str| json!({"type": "user", "message": {"role": "user", "content": text}});
                t.push(tx_ev(k, Other, "/compact line", line("/compact keep")));
                t.push(tx_ev(k, Other, "compact_boundary", system("compact_boundary")));
                let summary = json!({"type": "user", "isCompactSummary": true, "message": {"content": "This session is being continued"}});
                t.push(tx_ev(k, Other, "summary", summary));
                t.push(tx_ev(k, Other, "echo", line("<command-name>/compact</command-name>")));
                t.push(tx_ev(k, Other, "stdout", line("<local-command-stdout>Compacted</local-command-stdout>")));
            }
        }
        (h, t)
    }

    #[derive(Debug, Clone)]
    struct Case {
        kinds: Vec<Kind>,
        mode: String,
        lost: Option<usize>,
        /// The duplicated hook event: its turn and label.
        dup: Option<(usize, String)>,
        /// Delivered events in order (`SessionStart` first, `SessionEnd` last).
        events: Vec<Ev>,
    }

    fn script(seed: u64) -> Vec<Kind> {
        let mut rng = Rng::new(seed);
        let n = 2 + rng.below(MAX_TURNS - 1);
        let mut kinds = Vec::new();
        while (kinds.len() as u64) < n {
            let k = PICK[rng.below(PICK.len() as u64) as usize];
            kinds.push(k);
            if k == Kind::Queued {
                kinds.push(Kind::QueuedRun);
            }
        }
        kinds
    }

    fn build(kinds: &[Kind], seed: u64) -> Case {
        let mut rng = Rng::new(seed);
        let mut hooks = Vec::new();
        let mut txs = Vec::new();
        for (i, kind) in kinds.iter().enumerate() {
            let (h, t) = turn_events(i + 1, *kind, &mut rng);
            hooks.extend(h);
            txs.extend(t);
        }
        // At most one lost Stop, at most one duplicated hook event (delivered twice in a row).
        let mut lost = None;
        let stops: Vec<usize> = (0..hooks.len()).filter(|i| hooks[*i].role == Role::Stop).collect();
        if !stops.is_empty() && rng.percent(25) {
            let i = stops[rng.below(stops.len() as u64) as usize];
            lost = Some(hooks[i].turn);
            hooks.remove(i);
        }
        let mut dup = None;
        if !hooks.is_empty() && rng.percent(25) {
            let i = rng.below(hooks.len() as u64) as usize;
            let mut copy = hooks[i].clone();
            copy.label.push_str(" (dup)");
            copy.role = Role::Other;
            dup = Some((copy.turn, hooks[i].label.clone()));
            hooks.insert(i + 1, copy);
        }
        let (mode, order) =
            if rng.percent(40) { shifted(&hooks, &txs, &mut rng) } else { merged(&hooks, &txs, &mut rng) };
        let start = HookEvent { source: Some("startup".into()), ..HookEvent::named("SessionStart") };
        let mut events = vec![hook_ev(0, Role::Other, "SessionStart startup", start)];
        events.extend(order);
        events.push(hook_ev(kinds.len() + 1, Role::Other, "SessionEnd", HookEvent::named("SessionEnd")));
        Case { kinds: kinds.to_vec(), mode, lost, dup, events }
    }

    /// Hooks shifted by a whole number of quarter turns against the transcript, with jitter.
    fn shifted(hooks: &[Ev], txs: &[Ev], rng: &mut Rng) -> (String, Vec<Ev>) {
        let quarters = rng.below(17) as i64 - 8; // -2..=+2 turns; negative: hooks ahead
        let stamp = |evs: &[Ev], offset: i64, rng: &mut Rng| -> Vec<i64> {
            let mut out: Vec<i64> = Vec::new();
            for (i, e) in evs.iter().enumerate() {
                let n = evs.iter().filter(|x| x.turn == e.turn).count() as i64;
                let p = evs[..i].iter().filter(|x| x.turn == e.turn).count() as i64;
                let mut ts = e.turn as i64 * 1000 + (p + 1) * 1000 / (n + 1) + offset + rng.below(150) as i64;
                if let Some(prev) = out.last() {
                    ts = ts.max(prev + 1);
                }
                out.push(ts);
            }
            out
        };
        let ht = stamp(hooks, quarters * 250, rng);
        let tt = stamp(txs, 0, rng);
        let (mut i, mut j) = (0, 0);
        let mut out = Vec::new();
        while i < hooks.len() || j < txs.len() {
            let take_hook = j == txs.len() || (i < hooks.len() && (ht[i], rng.below(2)) < (tt[j], 1));
            if take_hook {
                out.push(hooks[i].clone());
                i += 1;
            } else {
                out.push(txs[j].clone());
                j += 1;
            }
        }
        (format!("hooks shifted {:+} quarter turns", quarters), out)
    }

    /// Per-event random merge, drift bounded to MAX_DRIFT turns.
    fn merged(hooks: &[Ev], txs: &[Ev], rng: &mut Rng) -> (String, Vec<Ev>) {
        let p = 15 + rng.below(71);
        let (mut i, mut j) = (0, 0);
        let mut out = Vec::new();
        while i < hooks.len() || j < txs.len() {
            let take_hook = if i == hooks.len() {
                false
            } else if j == txs.len() {
                true
            } else if hooks[i].turn > txs[j].turn + MAX_DRIFT {
                false
            } else if txs[j].turn > hooks[i].turn + MAX_DRIFT {
                true
            } else {
                rng.percent(p)
            };
            if take_hook {
                out.push(hooks[i].clone());
                i += 1;
            } else {
                out.push(txs[j].clone());
                j += 1;
            }
        }
        (format!("random merge, hook bias {p}%"), out)
    }

    /// Replays a case, checking the invariants after every event. Returns the first violation.
    /// Checks one case; on success returns how many turns lagged (I-d's allowance).
    fn check(case: &Case) -> Result<usize, String> {
        let n_turns = case.kinds.len() + 2;
        let evs = &case.events;
        let total: Vec<usize> = (0..n_turns).map(|k| evs.iter().filter(|e| e.turn == k).count()).collect();
        let mut seen = vec![0usize; n_turns];
        let mut delivered = vec![false; evs.len()];
        let mut lags: Vec<usize> = Vec::new();
        let mut e = StatusEngine::new();
        for (i, ev) in evs.iter().enumerate() {
            e.apply(&ev.input);
            delivered[i] = true;
            seen[ev.turn] += 1;
            let st = e.status();
            let ended =
                evs[..=i].iter().any(|x| matches!(&x.input, Input::Hook(h) if h.hook_event_name == "SessionEnd"));
            let fail = |what: &str| Err(format!("{what} after event #{i} ({}): status {st:?}", ev.label));
            if ended {
                if st != Status::Ended {
                    return fail("not Ended after SessionEnd");
                }
                continue;
            }
            let latest = (0..n_turns).rev().find(|k| seen[*k] > 0).unwrap_or(0);
            // "Turn k is over": an end event of it, or any event of a later turn, delivered.
            let over = |k: usize| {
                latest > k || (0..=i).any(|x| evs[x].turn == k && matches!(evs[x].role, Role::Stop | Role::TxEnd))
            };
            let has = |k: usize, role: Role| evs.iter().any(|x| x.turn == k && x.role == role);
            let got = |k: usize, role: Role| (0..=i).any(|x| evs[x].turn == k && evs[x].role == role);
            let count = |k: usize, role: Role| (0..=i).filter(|x| evs[*x].turn == k && evs[*x].role == role).count();
            // I-a
            for k in 0..n_turns {
                let complete = (0..=k).all(|j| seen[j] == total[j]) && (k + 1..n_turns).all(|j| seen[j] == 0);
                if complete && st != Status::Idle {
                    return fail(&format!("I-a: turn {k} fully delivered, nothing later"));
                }
            }
            // I-b
            for k in 1..=case.kinds.len() {
                let kind = case.kinds[k - 1];
                if matches!(kind, Kind::Local | Kind::LocalUps) || dup_stop_before_queued(case, k) {
                    continue;
                }
                let started = (!has(k, Role::HookStart) || got(k, Role::HookStart)) && got(k, Role::TxStart);
                if started && !over(k) && st == Status::Idle {
                    return fail(&format!("I-b: turn {k} ({kind:?}) started in both sources and not over"));
                }
            }
            // I-c
            if st == Status::NeedsYou {
                let open_dialog =
                    (1..=case.kinds.len()).any(|k| count(k, Role::Perm) > count(k, Role::PermDone) && !over(k));
                if !open_dialog {
                    return fail("I-c: NeedsYou without an unresolved PermissionRequest");
                }
            }
            // I-d
            if ev.hook && ev.role == Role::Stop && latest == ev.turn && st != Status::Idle {
                lags.push(ev.turn);
                let allowed = case.lost.is_some_and(|l| ev.turn > l) && lags.len() == 1;
                if !allowed {
                    return fail(&format!("I-d: Stop of turn {} did not idle (lagging turns {lags:?})", ev.turn));
                }
            }
        }
        Ok(lags.len())
    }

    /// The one relaxed case (I-b): the turn before queued turn `k` had its `Stop` delivered
    /// twice. A duplicate carries nothing that sets it apart from the queued turn's own
    /// `Stop` (no timestamp or event id in the payload Mirante reads, and a queued turn
    /// without tool use shows the hooks nothing else), so the queued turn may be taken as
    /// over until the transcript says otherwise. Erring the other way would make every
    /// real tool-less queued turn lag.
    fn dup_stop_before_queued(case: &Case, k: usize) -> bool {
        case.kinds[k - 1] == Kind::QueuedRun
            && case.dup.as_ref().is_some_and(|(t, l)| *t + 1 == k && (l == "H Stop" || l == "H StopFailure"))
    }

    fn describe(case: &Case, seed: (u64, u64), err: &str) -> String {
        let mut e = StatusEngine::new();
        let mut s = format!(
            "{err}\n  seeds: script {} interleaving {} (reproduce: run_case({}, {}))\n  script: {:?}\n  mode: {}; lost Stop of turn: {:?}; duplicated hook event (turn, label): {:?}\n  delivered:\n",
            seed.0, seed.1, seed.0, seed.1, case.kinds, case.mode, case.lost, case.dup
        );
        for (i, ev) in case.events.iter().enumerate() {
            e.apply(&ev.input);
            s.push_str(&format!(
                "    #{i:<3} t{} {:<28} -> {:<8} (hook turn {}, transcript turn {}{}, ended {}, late {:?})\n",
                ev.turn,
                ev.label,
                format!("{:?}", e.status()),
                e.h_turn,
                e.t_turn,
                if e.t_open { " open" } else { "" },
                e.ended,
                e.late_stops
            ));
        }
        s
    }

    /// Ok: (a Stop was lost, turns that lagged).
    fn run_case(script_seed: u64, inter_seed: u64) -> Result<(bool, usize), String> {
        let kinds = script(script_seed);
        let case = build(&kinds, script_seed.wrapping_mul(1_000_003) ^ inter_seed);
        check(&case)
            .map(|lags| (case.lost.is_some(), lags))
            .map_err(|err| describe(&case, (script_seed, inter_seed), &err))
    }

    /// `MIRANTE_HARNESS_CASE=<script seed>,<interleaving seed> cargo test reproduce_case --
    /// --nocapture` prints one run with the engine's counters after every event.
    #[test]
    fn reproduce_case() {
        let Some(spec) = std::env::var("MIRANTE_HARNESS_CASE").ok() else {
            return;
        };
        let seeds: Vec<u64> = spec.split(',').map(|x| x.trim().parse().expect("seed")).collect();
        let kinds = script(seeds[0]);
        let case = build(&kinds, seeds[0].wrapping_mul(1_000_003) ^ seeds[1]);
        let verdict = check(&case).err().unwrap_or_else(|| "ok".into());
        eprintln!("{}", describe(&case, (seeds[0], seeds[1]), &verdict));
    }

    #[test]
    fn interleavings_keep_the_invariants() {
        // MIRANTE_HARNESS_SCALE=<n> multiplies the scripts swept (seeds stay deterministic).
        let scale: u64 = std::env::var("MIRANTE_HARNESS_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
        let mut failures = Vec::new();
        let (mut lost_runs, mut lagged_runs) = (0, 0);
        for s in 0..SCRIPTS * scale {
            for i in 0..INTERLEAVINGS {
                match run_case(s, i) {
                    Ok((lost, lags)) => {
                        lost_runs += usize::from(lost);
                        lagged_runs += usize::from(lags > 0);
                    }
                    Err(msg) => failures.push(msg),
                }
            }
        }
        eprintln!(
            "{} runs; {lost_runs} with a lost Stop, {lagged_runs} of them lagged one turn",
            SCRIPTS * scale * INTERLEAVINGS
        );
        if !failures.is_empty() {
            let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
            for f in &failures {
                let key = f.split(" after event").next().unwrap_or("").split(':').next().unwrap_or("").to_string();
                *kinds.entry(key).or_default() += 1;
            }
            if std::env::var_os("MIRANTE_HARNESS_ALL").is_some() {
                for f in &failures {
                    eprintln!("{}\n", f.lines().take(4).collect::<Vec<_>>().join("\n"));
                }
            }
            panic!(
                "{} of {} runs broke an invariant {kinds:?}; first:\n{}",
                failures.len(),
                SCRIPTS * scale * INTERLEAVINGS,
                failures[0]
            );
        }
    }
}
