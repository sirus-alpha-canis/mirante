//! Session lifecycle: start / resume (one live session per task), boot and trust, stop,
//! and every input that moves a session's status: hook events (Claude, via `hook.event`)
//! and driver events (transcript lines, Codex status, approvals, exit).
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mirante_core::delivery::{classify_message, Delivery};
use mirante_core::hook::HookEvent;
use mirante_core::model::{AgentKind, Lifecycle, PermissionStatus, Status};
use mirante_core::redact::redact;
use mirante_core::status::{is_sidechain, Input};
use mirante_ipc::types::{
    AttachParams, BootState, HookEnvelope, HookParams, ResizeParams, SendMode, StartResult, StartSession, TaskView,
    TrustAnswer,
};
use serde_json::{json, Value};

use super::{queue, DialogKey, EventRx, Hub, HubError, Live, Result, State};
use crate::driver::{Attachment, Driver, DriverError, DriverEvent, Sink, SpawnRequest};

/// How many hook `event_id`s the hub remembers for dedupe.
pub(crate) const HOOK_DEDUPE_WINDOW: usize = 1024;

/// A bounded set of recently seen hook event ids (oldest forgotten first).
#[derive(Debug, Default)]
pub(crate) struct SeenHooks {
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl SeenHooks {
    /// False when `id` was already seen (inside the window).
    pub(crate) fn insert(&mut self, id: &str) -> bool {
        if self.set.contains(id) {
            return false;
        }
        self.set.insert(id.to_string());
        self.order.push_back(id.to_string());
        while self.order.len() > HOOK_DEDUPE_WINDOW {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.order.len()
    }
}

/// How often the daemon does its database housekeeping.
const MAINTENANCE_EVERY: Duration = Duration::from_secs(24 * 3600);

/// Why a launch is refused once [`Hub::shutdown`] began.
pub(crate) const SHUTTING_DOWN: &str = "the daemon is shutting down";
/// Longest wait of a shutdown for agents being spawned.
const SPAWN_WAIT: Duration = Duration::from_secs(15);

/// One launch whose agent is being spawned (see `Hub::spawning`), until dropped.
struct Spawning<'a>(&'a std::sync::atomic::AtomicUsize);

impl<'a> Spawning<'a> {
    fn new(n: &'a std::sync::atomic::AtomicUsize) -> Spawning<'a> {
        n.fetch_add(1, Ordering::SeqCst);
        Spawning(n)
    }
}

impl Drop for Spawning<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// (task id, session id, driver) of a live session.
type LiveRef = (String, String, Option<Arc<dyn Driver>>);

/// Most compacted transcript lines kept per session for steer/queue classification.
const TRANSCRIPT_WINDOW: usize = 500;

/// The parts of a main-chain transcript line that steer/queue classification reads
/// (`mirante_core::delivery`): queue operations, prompts and turn ends. Assistant content is
/// dropped to bound memory. Sidechain lines are skipped.
pub(crate) fn compact(line: &Value) -> Option<Value> {
    if is_sidechain(line) {
        return None;
    }
    match line["type"].as_str()? {
        "queue-operation" | "user" | "system" => Some(line.clone()),
        "assistant" => {
            Some(json!({ "type": "assistant", "message": { "stop_reason": line["message"]["stop_reason"] } }))
        }
        _ => None,
    }
}

/// A Claude tool call that ran (`PostToolUse` / `PostToolUseFailure`) as a feed step:
/// `{type: "tool", tool, summary, ok, agent}`. `summary` is what the tool worked on (command,
/// path, pattern, URL, description; at most 200 chars, credentials [`redact`]ed); a side
/// agent's prompt is never one. `tool` and `agent` (a side agent's type, else null) are at
/// most 64 chars; `agent_id` names which of Claude's own agents ran it (its lane), null for
/// the main agent. Claude has no other step source: Codex steps come from its driver.
pub(crate) fn tool_step(ev: &HookEvent) -> Option<Value> {
    let ok = match ev.hook_event_name.as_str() {
        "PostToolUse" => true,
        "PostToolUseFailure" => false,
        _ => return None,
    };
    let cap = |s: &str, n: usize| s.chars().take(n).collect::<String>();
    let tool = cap(ev.tool_name.as_deref()?, 64);
    let summary = ["command", "file_path", "path", "pattern", "url", "description"]
        .iter()
        .find_map(|k| ev.tool_input.as_ref().and_then(|i| i[*k].as_str()).filter(|s| !s.is_empty()))
        .unwrap_or("");
    let summary = cap(&redact(summary), 200);
    let agent = ev.agent_type.as_deref().map(|a| cap(a, 64));
    let agent_id = ev.agent_id.as_deref().filter(|a| super::subagents::valid_agent_id(a));
    Some(json!({ "type": "tool", "tool": tool, "summary": summary, "ok": ok, "agent": agent, "agent_id": agent_id }))
}

/// What a Claude permission mode lets the agent approve on its own: `commands` (auto,
/// bypassPermissions), `edits` (acceptEdits), or nothing (default / manual, plan, dontAsk,
/// and modes this daemon does not know).
pub(crate) fn self_approves(mode: &str) -> Option<&'static str> {
    match mode {
        "auto" | "bypassPermissions" => Some("commands"),
        "acceptEdits" => Some("edits"),
        _ => None,
    }
}

/// The main agent's text in one transcript line (`assistant`, not a side agent), redacted
/// and capped, as an `agent_message` feed step. `final` when the line ends the turn (kept up
/// to `AGENT_MESSAGE_CAP`; text on the way up to `AGENT_MESSAGE_PARTIAL_CAP`).
pub(crate) fn agent_message(line: &Value) -> Option<Value> {
    if line["type"] != "assistant" || is_sidechain(line) {
        return None;
    }
    let text: Vec<&str> = line["message"]["content"]
        .as_array()?
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .filter(|t| !t.trim().is_empty())
        .collect();
    if text.is_empty() {
        return None;
    }
    let fin = line["message"]["stop_reason"] == "end_turn";
    let cap = if fin { crate::driver::AGENT_MESSAGE_CAP } else { crate::driver::AGENT_MESSAGE_PARTIAL_CAP };
    let text: String = redact(&text.join("\n\n")).chars().take(cap).collect();
    Some(json!({ "type": "agent_message", "text": text, "final": fin }))
}

impl State {
    /// A Claude session reported its permission mode (`source`: "hook" or "transcript"):
    /// remembered on the live session, and a `permission_mode` event when it changed.
    pub(crate) fn observe_mode(&mut self, task_id: &str, session_id: &str, mode: &str, source: &str) -> Result<()> {
        let mode: String = mode.chars().take(64).collect();
        let Some(l) = self.live.get_mut(task_id).filter(|l| l.session_id == session_id) else {
            return Ok(());
        };
        if l.permission_mode.as_deref() == Some(mode.as_str()) {
            return Ok(());
        }
        let from = l.permission_mode.replace(mode.clone());
        let payload = json!({
            "session_id": session_id,
            "from": from,
            "mode": mode,
            "self_approves": self_approves(&mode),
            "source": source,
        });
        self.event(Some(task_id), "permission_mode", payload)
    }

    /// A resumed Claude session reports its mode with its first hook only. Until then, the
    /// task's last known mode is carried over when it approves on its own (`auto`,
    /// `bypassPermissions`, `acceptEdits`), so its warning never drops across a resume; a
    /// manual mode is not (a wrong "manual" would hide a warning). The first hook corrects it.
    pub(crate) fn carry_mode_over(&mut self, task_id: &str, session_id: &str) -> Result<()> {
        let events = self.store.task_events(task_id)?;
        let last = events.iter().rev().find(|e| e.kind == "permission_mode").and_then(|e| e.payload["mode"].as_str());
        match last {
            Some(mode) if self_approves(mode).is_some() => {
                let mode = mode.to_string();
                self.observe_mode(task_id, session_id, &mode, "resume")
            }
            _ => Ok(()),
        }
    }

    /// Delivers the next queued message when the session is ready and idle.
    pub(crate) fn kick(&mut self, task_id: &str) -> Result<()> {
        let status = self.store.get_task(task_id)?.status;
        let Some(l) = self.live.get_mut(task_id) else {
            return Ok(());
        };
        if l.boot != BootState::Ready || status != Status::Idle || l.queue.is_empty() || l.delivering() {
            return Ok(());
        }
        if self.mem.paused {
            // Memory is tight: queued messages wait (the guard kicks again when it eases).
            return Ok(());
        }
        let Some(driver) = l.driver.clone() else {
            return Ok(());
        };
        // A thread's block owed to the agent rides on the next message that may carry it.
        let carries = l.queue.front().is_some_and(|q| q.context && !super::threads::is_command(&q.text));
        let owed = if carries { self.thread_owed(task_id)? } else { None };
        let l = self.live.get_mut(task_id).expect("checked above");
        // Popped and handed to the driver under the hub lock: from here the message is being
        // delivered, and `message.edit` / `remove` / `reorder` refuse it (CONFLICT).
        let next = l.queue.pop_front().expect("not empty");
        l.requeue_at = 0;
        let wire = match &owed {
            Some(o) => super::threads::with_block(&o.block, &next.text, next.operator),
            None => next.wire(),
        };
        match driver.send(&wire) {
            Ok(()) => {
                if let Some(o) = &owed {
                    self.thread_handed(task_id, o)?;
                }
                let l = self.live.get_mut(task_id).expect("still live under the lock");
                l.delivery_in_flight = Some(Instant::now());
                l.left_queue.insert(next.id.clone());
                let payload =
                    json!({ "mode": SendMode::Queue, "text": super::messages::logged(&next.text), "id": next.id });
                l.last_sent = Some(next);
                self.event(Some(task_id), "message_sent", payload)
            }
            Err(e) => {
                if let Some(l) = self.live.get_mut(task_id) {
                    l.queue.push_front(next);
                    l.requeue_at = 1;
                }
                self.event(Some(task_id), "message_failed", json!({ "error": e.to_string() }))
            }
        }
    }

    /// For each Claude message typed mid-turn, records whether the transcript shows it
    /// steered (absorbed at a tool boundary) or queued (delivered after the turn).
    pub(crate) fn classify_steers(&mut self, task_id: &str) -> Result<()> {
        let Some(l) = self.live.get_mut(task_id) else {
            return Ok(());
        };
        let mut verdicts = Vec::new();
        let lines = &l.transcript;
        l.steers.retain(|text| match classify_message(lines, text) {
            Delivery::Pending => true,
            d => {
                verdicts.push((text.clone(), d));
                false
            }
        });
        for (text, delivery) in verdicts {
            // The person's words, not the label the agent saw.
            let text = super::messages::logged(super::threads::plain_words(&text));
            self.event(Some(task_id), "message_delivery", json!({ "text": text, "delivery": delivery }))?;
        }
        Ok(())
    }

    /// Records why Mirante is about to stop this live session (see `Live::ending`).
    pub(crate) fn mark_ending(&mut self, task_id: &str, session_id: &str, cause: &'static str) {
        if let Some(l) = self.live.get_mut(task_id).filter(|l| l.session_id == session_id) {
            l.ending = Some(cause);
        }
    }

    /// Ends the live session `session_id` of `task_id` (no-op if it is not the live one):
    /// closes its turn and DB session, expires its pending permission requests (the dialog
    /// died with the process), reports undelivered queued messages, and sets `Ended`.
    pub(crate) fn finish_session(&mut self, task_id: &str, session_id: &str, cause: &str) -> Result<()> {
        if self.live.get(task_id).is_none_or(|l| l.session_id != session_id) {
            return Ok(());
        }
        let live = self.live.remove(task_id).expect("checked above");
        self.store.end_session(session_id)?;
        for p in self.store.pending_permissions(task_id)? {
            if p.session_id == session_id {
                self.store.answer_permission(&p.id, PermissionStatus::Denied)?;
                self.event(Some(task_id), "permission_expired", json!({ "permission_id": p.id, "cause": cause }))?;
            }
        }
        if !live.queue.is_empty() {
            let texts: Vec<&str> = live.queue.iter().map(|q| q.text.as_str()).collect();
            self.event(Some(task_id), "queue_dropped", json!({ "messages": texts }))?;
        }
        self.set_status(task_id, Status::Ended, cause)
    }

    /// Feeds one input to a Claude session's `StatusEngine` and applies the result.
    pub(crate) fn feed_claude(&mut self, task_id: &str, input: &Input) -> Result<()> {
        let Some(live) = self.live.get_mut(task_id) else {
            return Ok(());
        };
        let t = live.engine.apply(input);
        let turn_open = live.engine.turn_open();
        self.after_input(task_id, t.map(|t| (t.from, t.to, t.cause)), turn_open)
    }

    /// Persists a status change and the turn records, then runs what a change implies:
    /// leaving `NeedsYou` because the turn ended (or was interrupted) settles the requests
    /// still pending; reaching `Idle` classifies Claude steers and delivers the next queued
    /// message. Leaving it because Mirante answered (`mirante:permission_*` /
    /// `mirante:approval_*`, already persisted) or because a tool ran (`hook:PostToolUse` /
    /// `hook:PostToolUseFailure`, which settle only their own card) settles nothing else.
    pub(crate) fn after_input(
        &mut self,
        task_id: &str,
        transition: Option<(Status, Status, String)>,
        turn_open: bool,
    ) -> Result<()> {
        if let Some((_, to, cause)) = &transition {
            self.set_status(task_id, *to, cause)?;
        }
        self.sync_turn(task_id, turn_open)?;
        if let Some((from, to, cause)) = transition {
            // Mirante's own answer (Claude: `mirante:permission_*`; Codex driver:
            // `mirante:approval_answered`) is already persisted; a tool run settles its card.
            let targeted = cause.starts_with("mirante:permission_")
                || cause.starts_with("mirante:approval_")
                || cause == "hook:PostToolUse"
                || cause == "hook:PostToolUseFailure";
            if from == Status::NeedsYou && to != Status::NeedsYou && !targeted {
                self.settle_permissions(task_id, &cause)?;
            }
            if to == Status::Idle {
                self.classify_steers(task_id)?;
                self.kick(task_id)?;
            }
        }
        Ok(())
    }

    fn sync_turn(&mut self, task_id: &str, open: bool) -> Result<()> {
        let State { store, live, .. } = self;
        let Some(l) = live.get_mut(task_id) else {
            return Ok(());
        };
        match (open, &l.turn_id) {
            (true, None) => l.turn_id = Some(store.start_turn(&l.session_id)?.id),
            (false, Some(id)) => {
                store.end_turn(id)?;
                l.turn_id = None;
            }
            _ => {}
        }
        Ok(())
    }

    /// The turn ended (or was interrupted) with requests still pending: the agent moved on
    /// without running their tools, so each one is settled as denied.
    fn settle_permissions(&mut self, task_id: &str, cause: &str) -> Result<()> {
        let Some(session_id) = self.live.get(task_id).map(|l| l.session_id.clone()) else {
            return Ok(());
        };
        for p in self.store.pending_permissions(task_id)? {
            if p.session_id == session_id {
                self.settle_card(task_id, &p.id, PermissionStatus::Denied, cause)?;
            }
        }
        Ok(())
    }

    /// A `PostToolUse` / `PostToolUseFailure` of the live Claude session: the tool behind the
    /// oldest matching card ran (successfully or not), so the user allowed it (in the
    /// attached terminal).
    fn settle_tool_ran(&mut self, task_id: &str, session_id: &str, ev: &HookEvent) -> Result<()> {
        let Some(l) = self.live.get(task_id) else {
            return Ok(());
        };
        let card = self
            .store
            .pending_permissions(task_id)?
            .into_iter()
            .filter(|p| p.session_id == session_id)
            .find(|p| l.dialogs.get(&p.id).is_some_and(|k| k.answered_by(ev)));
        match card {
            Some(p) => {
                let cause = format!("hook:{}", ev.hook_event_name);
                self.settle_card(task_id, &p.id, PermissionStatus::AllowedOnce, &cause)
            }
            None => Ok(()),
        }
    }

    fn settle_card(&mut self, task_id: &str, id: &str, answer: PermissionStatus, cause: &str) -> Result<()> {
        self.store.answer_permission(id, answer)?;
        if let Some(l) = self.live.get_mut(task_id) {
            l.perm_keys.remove(id);
            l.dialogs.remove(id);
            l.card_gen.fetch_add(1, Ordering::SeqCst);
        }
        self.event(
            Some(task_id),
            "permission_settled",
            json!({ "permission_id": id, "answer": answer, "cause": cause }),
        )
    }

    fn handle(&mut self, task_id: &str, session_id: &str, event: DriverEvent) -> Result<()> {
        #[cfg(test)]
        if let DriverEvent::Step(v) = &event {
            // Test-only: a bug reached while handling an event.
            assert!(v.get("test_panic").is_none(), "test panic handling {v}");
        }
        match event {
            DriverEvent::Transcript(line) => {
                if let Some(l) = self.live.get_mut(task_id) {
                    if l.ledger.observe(&line) {
                        l.card_gen.fetch_add(1, Ordering::SeqCst);
                    }
                    if let Some(c) = compact(&line) {
                        l.transcript.push(c);
                        let excess = l.transcript.len().saturating_sub(TRANSCRIPT_WINDOW);
                        l.transcript.drain(..excess);
                    }
                }
                if line["type"] == "permission-mode" && !is_sidechain(&line) {
                    if let Some(mode) = line["permissionMode"].as_str() {
                        self.observe_mode(task_id, session_id, mode, "transcript")?;
                    }
                }
                if let Some(step) = agent_message(&line) {
                    self.event(Some(task_id), "step", step)?;
                }
                self.feed_claude(task_id, &Input::Transcript(line))
            }
            DriverEvent::TranscriptOffset { path, offset } => {
                Ok(self.store.set_transcript(session_id, &path, offset)?)
            }
            DriverEvent::Status { to, cause, turn_open, detail } => {
                let from = self.store.get_task(task_id)?.status;
                if let Some(l) = self.live.get_mut(task_id) {
                    l.detail = detail;
                }
                let t = (from != to).then_some((from, to, cause));
                self.after_input(task_id, t, turn_open)
            }
            DriverEvent::PermissionRequested { key, tool, detail } => {
                let p = self.store.create_permission_request(session_id, &tool, &detail)?;
                if let Some(l) = self.live.get_mut(task_id) {
                    l.perm_keys.insert(p.id, key);
                }
                Ok(())
            }
            DriverEvent::PermissionResolved { key } => {
                let Some(l) = self.live.get(task_id) else {
                    return Ok(());
                };
                let card = l.perm_keys.iter().find(|(_, k)| **k == key).map(|(id, _)| id.clone());
                let Some(id) = card else {
                    // Already answered through Mirante (its key was removed then).
                    return Ok(());
                };
                let pending = self.store.pending_permissions(task_id)?.iter().any(|p| p.id == id);
                if pending {
                    self.settle_card(task_id, &id, PermissionStatus::Denied, "codex:request_resolved")?;
                } else if let Some(l) = self.live.get_mut(task_id) {
                    l.perm_keys.remove(&id);
                }
                Ok(())
            }
            DriverEvent::DeliveryAborted { text: wire, reason } => {
                // The driver hands back what it was given (labelled when a person sent it):
                // the queue holds the sender's own words and who sent them.
                let text = super::threads::plain_words(&wire).to_string();
                let operator = super::threads::labelled(&wire);
                // It carried the thread's block: the block is owed again.
                if mirante_core::delivery::strip_operator_label(&wire).starts_with("<mirante-thread") {
                    self.thread_handed_back(task_id);
                }
                let mut id = None;
                if let Some(l) = self.live.get_mut(task_id) {
                    let plain = |s: &String| super::threads::plain_words(s) == text;
                    if let Some(i) = l.steers.iter().rposition(plain) {
                        l.steers.remove(i);
                    }
                    // A queued message comes back under its id (editable again); a steer
                    // joins the queue as a new message.
                    let back = match l.last_sent.take() {
                        Some(q) if q.text == text => {
                            l.left_queue.remove(&q.id);
                            q
                        }
                        other => {
                            l.last_sent = other;
                            queue::Queued::from_operator(text.clone(), operator, operator)
                        }
                    };
                    id = Some(back.id.clone());
                    let at = l.requeue_at.min(l.queue.len());
                    l.queue.insert(at, back);
                    l.requeue_at = at + 1;
                    l.delivery_in_flight = None;
                }
                let mut payload = json!({ "text": super::messages::logged(&text), "reason": reason });
                if let Some(id) = id {
                    payload["id"] = json!(id);
                }
                self.event(Some(task_id), "message_requeued", payload)
            }
            // Typing is unblocked again: deliver what was requeued.
            DriverEvent::DialogClosed => self.kick(task_id),
            DriverEvent::ResumeFailed { reason } => {
                let payload = json!({
                    "session_id": session_id,
                    "reason": reason,
                    "hint": "`mirante resume <task> --fresh` starts a new conversation",
                });
                self.event(Some(task_id), "resume_failed", payload)
            }
            DriverEvent::Usage(summary) => {
                if let Some(l) = self.live.get_mut(task_id) {
                    l.usage = Some(summary);
                }
                Ok(())
            }
            DriverEvent::Sandbox(summary) => {
                let changed =
                    self.live.get_mut(task_id).map(|l| l.sandbox.replace(summary.clone()) != Some(summary.clone()));
                if changed == Some(true) {
                    // Audited: the sandbox the agent really runs with (network, writable roots).
                    self.event(Some(task_id), "sandbox", json!({ "session_id": session_id, "effective": summary }))?;
                }
                Ok(())
            }
            DriverEvent::NativeId(id) => self.set_native_id(task_id, session_id, &id),
            DriverEvent::Boot(state) => {
                if let Some(l) = self.live.get_mut(task_id) {
                    l.boot = state;
                }
                self.event(Some(task_id), "boot", json!({ "session_id": session_id, "state": state }))?;
                self.kick(task_id)
            }
            // Audited under its own kind: Mirante killed processes (Codex left them running).
            DriverEvent::Step(v) if v["type"] == "command_killed" => self.event(Some(task_id), "command_killed", v),
            // The session runs without its skills: their injected usage is taken back.
            DriverEvent::Step(v) if v["type"] == "skills_failed" => {
                let error = v["error"].as_str().unwrap_or_default().to_string();
                self.event(Some(task_id), "step", v)?;
                Ok(self.store.skill_session_failed(session_id, task_id, &error)?)
            }
            DriverEvent::Step(mut v) => {
                // A message step (Codex `message_confirmed` / `send_unconfirmed`) carries what
                // the agent was handed: the feed shows the person's words, not the label.
                // Masked like every message text the log keeps.
                if let Some(text) = v["text"].as_str() {
                    let plain = super::messages::logged(super::threads::plain_words(text));
                    if plain != text {
                        v["text"] = json!(plain);
                    }
                }
                self.event(Some(task_id), "step", v)
            }
            DriverEvent::Exited { code } => {
                self.event(Some(task_id), "process_exited", json!({ "session_id": session_id, "code": code }))?;
                let cause = self.live.get(task_id).and_then(|l| l.ending).unwrap_or("mirante:process_exited");
                self.finish_session(task_id, session_id, cause)
            }
        }
    }

    fn set_native_id(&mut self, task_id: &str, session_id: &str, native: &str) -> Result<()> {
        let Some(l) = self.live.get_mut(task_id) else {
            return Ok(());
        };
        if l.native_id.as_deref() == Some(native) {
            return Ok(());
        }
        l.native_id = Some(native.to_string());
        let conversation_id = self.store.get_session(session_id)?.conversation_id;
        Ok(self.store.set_native_session_id(&conversation_id, native)?)
    }
}

/// What [`Hub::launch`] did.
#[derive(Debug)]
pub(crate) enum Launch {
    Started(StartResult),
    /// An automatic resume that may not happen (now): why.
    Refused(&'static str),
}

impl Hub {
    /// Starts (or, with `resume`, resumes) the task's agent. Fails with a conflict while a
    /// session is live (the per-task lock). Waits until the agent leaves `Booting`.
    pub fn start_session(&self, req: StartSession, resume: bool) -> Result<StartResult> {
        if let Some(p) = req.prompt.as_deref().filter(|p| !p.trim().is_empty()) {
            super::messages::check_message(p)?;
        }
        match self.launch(req, resume, false)? {
            Launch::Started(r) => Ok(r),
            Launch::Refused(why) => Err(HubError::Conflict(why.to_string())),
        }
    }

    /// [`Hub::start_session`]; `auto`: an automatic resume after a restart, so under the
    /// same lock as the launch the task must still be one to resume (see
    /// `resume::resume_refusal`) and memory must not be tight (`Refused(MEMORY_TIGHT)`: the
    /// caller waits and retries). Its `session_launch` says `auto_resume`.
    pub(crate) fn launch(&self, req: StartSession, resume: bool, auto: bool) -> Result<Launch> {
        let (task, session_id, native, sink, conversation_id, spawning) = {
            let mut st = self.lock();
            // Read under the lock `shutdown` sets it under: a launch either is refused here
            // or is counted before shutdown looks.
            if self.shutting_down.load(Ordering::SeqCst) {
                return if auto {
                    Ok(Launch::Refused(SHUTTING_DOWN))
                } else {
                    Err(HubError::Conflict(format!("{SHUTTING_DOWN}: start it again once it is back")))
                };
            }
            let task = st.resolve(&req.task)?;
            if auto {
                if st.mem.paused {
                    return Ok(Launch::Refused(super::resume::MEMORY_TIGHT));
                }
                if let Some(why) = st.resume_refusal(&task.id)? {
                    return Ok(Launch::Refused(why));
                }
            }
            if task.lifecycle == Lifecycle::Archived {
                return Err(HubError::Conflict(format!("task {} is archived", task.branch)));
            }
            // Until the worktree is gone no session may start (it would run in a directory
            // being deleted).
            if task.lifecycle == Lifecycle::Archiving {
                return Err(HubError::Conflict(format!("task {} is being archived", task.branch)));
            }
            if st.live.contains_key(&task.id) {
                return Err(HubError::Conflict(format!("task {} already has a live session", task.branch)));
            }
            if st.mem.paused {
                return Err(HubError::Conflict(
                    "memory is tight and the guard is set to pause new sessions: they wait until it eases \
                     (mirante watch / mirante doctor; the app's Sessions setting \
                     \"Pause new sessions under memory pressure\" turns it off)"
                        .into(),
                ));
            }
            st.mem.hibernated.remove(&task.id);
            // Resume continues the latest conversation; a plain start opens a fresh one
            // unless the latest never got a native id (nothing to lose).
            let latest = st.store.latest_conversation(&task.id)?;
            let conversation = match latest {
                Some(c) if resume || c.native_session_id.is_none() => c,
                _ => st.store.create_conversation(&task.id)?,
            };
            let native = if resume { conversation.native_session_id.clone() } else { None };
            let session = st.store.start_session(&task.id, &conversation.id)?;
            let launched = st.set_status(&task.id, Status::Idle, "mirante:session_started").and_then(|()| {
                let mut payload = json!({ "session_id": session.id, "resume": native, "resume_requested": resume });
                if auto {
                    payload["auto_resume"] = json!(true);
                }
                st.event(Some(&task.id), "session_launch", payload)
            });
            if let Err(e) = launched {
                // Nothing is live yet: release the per-task lock and fail.
                if let Err(undo) = st.store.end_session(&session.id) {
                    st.log_error(Some(&task.id), "session.start rollback", &undo);
                }
                st.publish();
                return Err(e);
            }
            let sink = Sink::new(&session.id, self.events.clone());
            let mut live = Live::new(&session.id, task.agent, native.clone());
            live.hold = sink.hold_flag();
            // A thread's block is owed to a fresh conversation, and again after a resume.
            if st.store.thread_state(&task.id).ok().flatten().is_some() {
                live.thread_due = Some(if native.is_some() { "resume" } else { "start" });
            }
            if let Some(p) = req.prompt.filter(|p| !p.trim().is_empty()) {
                live.queue.push_back(queue::Queued::new(p));
            }
            st.live.insert(task.id.clone(), live);
            if native.is_some() && task.agent == AgentKind::Claude {
                if let Err(e) = st.carry_mode_over(&task.id, &session.id) {
                    st.log_error(Some(&task.id), "permission mode at resume", &e);
                }
            }
            st.publish();
            (task, session.id, native, sink, conversation.id, Spawning::new(&self.spawning))
        };
        self.restore_workspace_files(&task);
        let mcp = self.memory_server_for(&task);
        let skills_mcp = self.skills_server_for(&task);
        let orchestrator = self.orchestrator_server_for(&task);
        let orchestrated = orchestrator.is_some();
        // A resumed Codex thread has its skills in its history (its first turn carried
        // them): it gets nothing again, only the withdrawal of those retired or changed
        // since. A resumed Claude session gets its plugin and always-on text again.
        let (skills, withdrawn) = match (task.agent, &native) {
            (AgentKind::Codex, Some(_)) => (None, self.codex_withdrawals(&task, &session_id, &conversation_id)),
            _ => (self.skills_for(&task, &session_id), Vec::new()),
        };
        let req = SpawnRequest {
            task: task.clone(),
            session_id: session_id.clone(),
            resume: native.clone(),
            paths: self.paths.clone(),
            mcp,
            skills_mcp,
            orchestrator,
            skills,
            withdrawn,
            read_only: self.read_only_repos(&task),
        };
        match self.factory.spawn(req, sink) {
            Ok(driver) => {
                let mut st = self.lock();
                let attached = match st.live.get_mut(&task.id).filter(|l| l.session_id == session_id) {
                    Some(l) => {
                        l.driver = Some(driver.clone());
                        l.orchestrator = orchestrated;
                        if l.saw_session_start {
                            // The hook arrived while the agent was being spawned.
                            driver.session_started();
                        }
                        true
                    }
                    None => false,
                };
                if attached {
                    st.kick(&task.id)?;
                    st.publish();
                } else {
                    // Stopped while the agent was spawning: nobody owns this process.
                    drop(st);
                    driver.stop();
                }
            }
            Err(e) => {
                let mut st = self.lock();
                if let DriverError::NativeGone(reason) = &e {
                    let payload = json!({
                        "session_id": session_id,
                        "reason": reason,
                        "hint": "`mirante resume <task> --fresh` starts a new conversation",
                    });
                    st.event(Some(&task.id), "resume_failed", payload)?;
                }
                st.finish_session(&task.id, &session_id, "mirante:spawn_failed")?;
                st.publish();
                return Err(e.into());
            }
        }
        // Owned by the live session now (or stopped): shutdown need not wait for its boot.
        drop(spawning);
        self.wait_boot(&task.id, &session_id, native.is_some()).map(Launch::Started)
    }

    /// Polls (without holding the lock) until the session leaves `Booting`, ends, or the
    /// boot timeout passes.
    fn wait_boot(&self, task_id: &str, session_id: &str, resumed: bool) -> Result<StartResult> {
        let deadline = Instant::now() + self.boot_timeout;
        loop {
            let (boot, driver) = {
                let st = self.lock();
                match st.live.get(task_id).filter(|l| l.session_id == session_id) {
                    Some(l) => (l.boot, l.driver.clone()),
                    None => (BootState::Exited, None),
                }
            };
            if boot != BootState::Booting || Instant::now() >= deadline {
                let screen = driver.map(|d| d.screen_tail()).unwrap_or_default();
                return Ok(StartResult { task: task_id.to_string(), boot, resumed, screen });
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The user's answer to Claude's workspace-trust dialog (never answered by Mirante on
    /// its own). Accept: boot continues. Decline: the session ends.
    pub fn trust(&self, answer: TrustAnswer) -> Result<StartResult> {
        let (task_id, session_id, driver, resumed) = {
            let mut st = self.lock();
            let task = st.resolve(&answer.task)?;
            let l = st.live.get_mut(&task.id).ok_or_else(|| HubError::Conflict("no live session".into()))?;
            if l.boot != BootState::TrustPrompt {
                return Err(HubError::Conflict("no trust prompt is open".into()));
            }
            let driver = l.driver.clone().ok_or_else(|| HubError::Conflict("agent still starting".into()))?;
            // Before the driver acts: its watcher may report Ready right after.
            l.boot = BootState::Booting;
            let out = (task.id.clone(), l.session_id.clone(), driver, l.resumed);
            st.event(Some(&task.id), "trust_answered", json!({ "accept": answer.accept }))?;
            st.publish();
            out
        };
        if let Err(e) = driver.trust(answer.accept) {
            let mut st = self.lock();
            if let Some(l) = st.live.get_mut(&task_id) {
                l.boot = BootState::TrustPrompt;
            }
            return Err(e.into());
        }
        self.wait_boot(&task_id, &session_id, resumed)
    }

    /// Ends the live session (graceful, then kill). The conversation stays resumable.
    pub fn stop(&self, reference: &str) -> Result<TaskView> {
        let (task_id, session_id, driver) = self.live_driver(reference)?;
        self.lock().mark_ending(&task_id, &session_id, "mirante:stopped");
        if let Some(d) = driver {
            d.stop();
        }
        let mut st = self.lock();
        st.finish_session(&task_id, &session_id, "mirante:stopped")?;
        st.publish();
        let t = st.store.get_task(&task_id)?;
        st.view(t)
    }

    /// Claude: refused while the session has a pending permission (Esc would answer its
    /// dialog); the driver also refuses while a dialog is on screen.
    pub fn interrupt(&self, reference: &str) -> Result<()> {
        let (task_id, session_id, driver) = self.live_driver(reference)?;
        {
            let st = self.lock();
            let claude = st.live.get(&task_id).is_some_and(|l| l.agent == AgentKind::Claude);
            if claude && st.store.pending_permissions(&task_id)?.iter().any(|p| p.session_id == session_id) {
                return Err(HubError::Driver(DriverError::NotNow(crate::driver::claude::INTERRUPT_REFUSED.into())));
            }
        }
        driver.ok_or_else(|| HubError::Conflict("agent still starting".into()))?.interrupt()?;
        let mut st = self.lock();
        st.event(Some(&task_id), "interrupt_sent", json!({}))?;
        st.publish();
        Ok(())
    }

    pub fn attach(&self, p: AttachParams) -> Result<(Arc<dyn Driver>, Attachment)> {
        let (task_id, session_id, driver) = self.live_driver(&p.task)?;
        let driver = driver.ok_or_else(|| HubError::Conflict("agent still starting".into()))?;
        // 0 x 0: keep the PTY's size (a client re-attaching); else at least 10 x 40.
        let (rows, cols) = if p.rows == 0 || p.cols == 0 { (0, 0) } else { (p.rows.max(10), p.cols.max(40)) };
        let a = driver.attach(rows, cols)?;
        let mut st = self.lock();
        // Audit the session's first attach only: a view that comes back re-attaches, and an
        // event per re-attach would be noise in the log and a refresh in every client.
        let first = st
            .live
            .get_mut(&task_id)
            .filter(|l| l.session_id == session_id)
            .is_some_and(|l| !std::mem::replace(&mut l.attach_recorded, true));
        if first {
            st.event(Some(&task_id), "attached", json!({ "attachment": a.id }))?;
            st.publish();
        }
        Ok((driver, a))
    }

    /// The attached terminal changed size: forwarded to the live driver.
    pub fn resize(&self, p: ResizeParams) -> Result<()> {
        let (_, _, driver) = self.live_driver(&p.task)?;
        let driver = driver.ok_or_else(|| HubError::Conflict("agent still starting".into()))?;
        Ok(driver.resize(p.rows.max(10), p.cols.max(40))?)
    }

    fn live_driver(&self, reference: &str) -> Result<LiveRef> {
        let st = self.lock();
        let task = st.resolve(reference)?;
        let l = st
            .live
            .get(&task.id)
            .ok_or_else(|| HubError::Conflict(format!("task {} has no live session", task.branch)))?;
        Ok((task.id.clone(), l.session_id.clone(), l.driver.clone()))
    }

    /// The daemon is going down ([`Hub::shutdown`] began).
    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    /// Stops every live session (daemon shutdown). From the moment it begins, launches are
    /// refused; an agent being spawned is waited for (at most [`SPAWN_WAIT`]) so it is
    /// stopped with the rest instead of outliving the daemon.
    pub fn shutdown(&self) {
        {
            let _st = self.lock();
            self.shutting_down.store(true, Ordering::SeqCst);
        }
        let deadline = Instant::now() + SPAWN_WAIT;
        while self.spawning.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let live: Vec<LiveRef> = {
            let mut st = self.lock();
            for l in st.live.values_mut() {
                l.ending = Some("mirante:daemon_shutdown");
            }
            st.live.iter().map(|(t, l)| (t.clone(), l.session_id.clone(), l.driver.clone())).collect()
        };
        for (_, _, d) in &live {
            if let Some(d) = d {
                d.stop();
            }
        }
        let mut st = self.lock();
        for (task_id, session_id, _) in live {
            let _ = st.finish_session(&task_id, &session_id, "mirante:daemon_shutdown");
        }
        st.publish();
        if let Err(e) = st.store.maintain() {
            st.log_error(None, "database maintenance at shutdown", &e);
        }
    }

    /// One Claude hook payload without an event id (see [`Hub::apply_hook_envelope`]).
    pub fn apply_hook(&self, p: HookParams) -> Result<()> {
        self.apply_hook_envelope(HookEnvelope {
            params: p,
            event_id: None,
            received_ts_ms: None,
            truncated: None,
            session: None,
        })
    }

    /// One Claude hook payload as `mirante-hook` sends it (`hook.event`). An `event_id`
    /// already seen is dropped (the hook may deliver an event and also spool it). Records
    /// the native session id and the transcript path (starting the tailer), persists
    /// permission requests, and feeds the status engine. A payload for a task without a
    /// live Claude session, stamped with another Mirante session (`hook_stale`), or (once
    /// the native session id is known) from another native session, is only logged.
    /// `hook.event` from a socket caller (see [`Hub::apply_hook_as`]).
    pub fn apply_hook_from(&self, caller: &super::dna::Caller, e: HookEnvelope) -> Result<()> {
        self.apply_hook_as(&self.identify(caller), e)
    }

    /// `hook.event` for `who`: a person may deliver any task's hook; a Mirante-launched
    /// agent (its `mirante-hook`) only its own task's, so a forged `Stop` or permission
    /// request never reaches another task. An agent whose task is unknown, and a caller that
    /// could not be checked, are refused.
    pub fn apply_hook_as(&self, who: &super::dna::Identity, e: HookEnvelope) -> Result<()> {
        use super::dna::Identity;
        let own = |why: &str| {
            HubError::Forbidden(format!("{why}: a Mirante-launched agent delivers its own task's hooks only"))
        };
        match who {
            Identity::Person => {}
            Identity::Agent { task: Some(t) } if *t == e.params.task => {}
            Identity::Agent { task: Some(_) } => return Err(own("another task was named")),
            Identity::Agent { task: None } => return Err(own("this agent's task is unknown")),
            Identity::Unverified(why) => return Err(HubError::Forbidden(why.clone())),
        }
        self.apply_hook_envelope(e)
    }

    pub fn apply_hook_envelope(&self, e: HookEnvelope) -> Result<()> {
        let mut st = self.lock();
        let r = st.apply_hook_locked(e);
        if r.is_err() {
            // Whatever was applied before the error may have opened or settled a card: the
            // hold flags must follow (publish syncs them and sends what was persisted).
            st.publish();
        }
        r
    }
}

impl State {
    fn apply_hook_locked(&mut self, e: HookEnvelope) -> Result<()> {
        let st = self;
        if let Some(id) = &e.event_id {
            if !st.seen_hooks.insert(id) {
                return Ok(());
            }
        }
        let stamped = e.session;
        let p = e.params;
        let ev = match HookEvent::parse(&p.payload) {
            Ok(ev) => ev,
            Err(e) => {
                st.event(Some(&p.task), "hook_unparseable", json!({ "error": e.to_string() }))?;
                st.publish();
                return Err(HubError::Invalid(format!("hook payload: {e}")));
            }
        };
        let live = st.live.get(&p.task).filter(|l| l.agent == AgentKind::Claude);
        let Some(live) = live else {
            st.event(Some(&p.task), "hook_orphan", json!({ "hook": ev.hook_event_name }))?;
            st.publish();
            return Ok(());
        };
        let session_id = live.session_id.clone();
        let native = live.native_id.clone();
        let card_gen = live.card_gen.clone();
        // Generation fence: a hook stamped with an earlier Mirante session of this task (its
        // Claude process outlived the session, or the hook was slow) must not touch the new
        // one. Unstamped hooks (an older hook binary) are still applied.
        if let Some(stale) = stamped.as_ref().filter(|s| **s != session_id) {
            let payload = json!({ "hook": ev.hook_event_name, "session": stale, "live_session": session_id });
            st.event(Some(&p.task), "hook_stale", payload)?;
            st.publish();
            return Ok(());
        }
        // Any hook of this session may mean another dialog: an answer in flight for an
        // earlier view of the session types nothing.
        card_gen.fetch_add(1, Ordering::SeqCst);
        if ev.hook_event_name == "SessionStart" {
            if let Some(d) = live.driver.clone() {
                d.session_started();
            }
            if let Some(l) = st.live.get_mut(&p.task) {
                l.saw_session_start = true;
            }
        }
        // Stamped with this very Mirante session: Claude's own session id may change under
        // it (`/clear`, a resume that forks), so the hook's id is adopted, never dropped.
        if stamped.is_some() {
            if let Some(sid) = &ev.session_id {
                st.set_native_id(&p.task, &session_id, sid)?;
            }
        } else if ev.hook_event_name != "SessionStart" {
            if let (Some(known), Some(sid)) = (native.as_deref(), ev.session_id.as_deref()) {
                if known != sid {
                    let payload = json!({ "hook": ev.hook_event_name, "session_id": sid, "native_id": known });
                    st.event(Some(&p.task), "hook_orphan", payload)?;
                    st.publish();
                    return Ok(());
                }
            }
        }
        if ev.hook_event_name == "SessionStart" {
            if let Some(sid) = &ev.session_id {
                st.set_native_id(&p.task, &session_id, sid)?;
            }
        }
        if let Some(path) = ev.transcript_path.clone() {
            st.follow_transcript(&p.task, &session_id, &path)?;
        }
        if let Some(mode) = ev.permission_mode.as_deref().filter(|m| !m.is_empty()) {
            st.observe_mode(&p.task, &session_id, mode, "hook")?;
        }
        let card =
            if ev.hook_event_name == "PermissionRequest" { st.open_card(&p.task, &session_id, &ev)? } else { None };
        // Claude's own agents: read-only lanes (cosmetic: a failure must not fail the hook).
        if let Err(e) = st.observe_subagent(&p.task, &ev, &p.payload, card.as_deref()) {
            eprintln!("mirante-daemon: task {}: subagent lane: {e}", p.task);
        }
        // A tool that failed still ran: its dialog was allowed.
        let tool_ran = matches!(ev.hook_event_name.as_str(), "PostToolUse" | "PostToolUseFailure");
        st.feed_claude(&p.task, &Input::Hook(ev.clone()))?;
        if tool_ran {
            st.settle_tool_ran(&p.task, &session_id, &ev)?;
        }
        if ev.hook_event_name == "PreToolUse" {
            st.skill_hook(&p.task, &session_id, &ev);
        }
        if let Some(step) = tool_step(&ev) {
            // A feed step is cosmetic: failing to record it must not fail the hook.
            if let Err(e) = st.event(Some(&p.task), "step", step) {
                eprintln!("mirante-daemon: task {}: step event: {e}", p.task);
            }
        }
        st.publish();
        Ok(())
    }
}

impl Hub {
    /// Applies one driver event. Events of a session that is no longer live are ignored.
    pub fn handle_driver_event(&self, session_id: &str, event: DriverEvent) {
        let mut st = self.lock();
        let Some(task_id) = st.task_of_session(session_id) else {
            return;
        };
        if let Err(e) = st.handle(&task_id, session_id, event) {
            eprintln!("mirante-daemon: task {task_id}: {e}");
            let _ = st.event(Some(&task_id), "daemon_error", json!({ "error": e.to_string() }));
        }
        st.publish();
    }

    /// [`Hub::handle_driver_event`] that survives a panic: the panic is logged (stderr and a
    /// `daemon_error` event) and the next event is handled as usual. One bad event must not
    /// stop every session's status updates. The hub mutex may be left poisoned; [`Hub::lock`]
    /// recovers it.
    pub fn handle_driver_event_guarded(&self, session_id: &str, event: DriverEvent) {
        let kind = format!("{event:?}").chars().take(80).collect::<String>();
        let run = std::panic::AssertUnwindSafe(|| self.handle_driver_event(session_id, event));
        if let Err(panic) = std::panic::catch_unwind(run) {
            let msg = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown panic".into());
            let mut st = self.lock();
            let task = st.task_of_session(session_id);
            eprintln!("mirante-daemon: driver event panicked (session {session_id}, {kind}): {msg}");
            let payload = json!({
                "context": "driver event panicked",
                "error": msg,
                "session_id": session_id,
                "event": kind,
            });
            let _ = st.event(task.as_deref(), "daemon_error", payload);
            st.publish();
        }
    }

    /// Applies every queued driver event now (tests; the daemon uses the pump thread).
    pub fn drain(&self, rx: &EventRx) {
        while let Ok((session_id, event)) = rx.try_recv() {
            self.handle_driver_event_guarded(&session_id, event);
        }
    }

    /// Database housekeeping once a day while the daemon runs (`Store::maintain`; the
    /// shutdown does it too).
    pub fn spawn_maintenance(self: &Arc<Self>) {
        let hub = Arc::downgrade(self);
        std::thread::spawn(move || loop {
            std::thread::sleep(MAINTENANCE_EVERY);
            let Some(hub) = hub.upgrade() else { return };
            let mut st = hub.lock();
            if let Err(e) = st.store.maintain() {
                st.log_error(None, "daily database maintenance", &e);
            }
        });
    }

    /// The thread that applies driver events. It only ends if the channel closes (never:
    /// the hub keeps a sender) or a panic escapes the guard; the daemon watches the handle
    /// and exits loudly then.
    pub fn spawn_pump(self: &Arc<Self>, rx: EventRx) -> std::thread::JoinHandle<()> {
        let hub = Arc::clone(self);
        std::thread::spawn(move || {
            for (session_id, event) in rx {
                hub.handle_driver_event_guarded(&session_id, event);
            }
        })
    }
}

impl State {
    /// Persists the card for a Claude `PermissionRequest` and remembers which tool call it
    /// is about. A repeat of a dialog that already has a pending card adds none. One agent
    /// (the main one, or a side agent by `agent_id`) never has two dialogs open: its older
    /// pending cards are settled as superseded (`permission_superseded`, recorded denied),
    /// so Mirante never answers a dialog that is no longer the one on screen.
    fn open_card(&mut self, task_id: &str, session_id: &str, ev: &HookEvent) -> Result<Option<String>> {
        let Some(l) = self.live.get(task_id) else {
            return Ok(None);
        };
        let description = ev
            .tool_input
            .as_ref()
            .and_then(|i| i["description"].as_str())
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string);
        let key = DialogKey {
            agent: ev.agent_id.clone(),
            tool_use_id: l.engine.tool_use_id_for(ev),
            tool_name: ev.tool_name.clone(),
            description,
            detail: ev.permission_detail(),
            hook_lost: mirante_ipc::hookloss::fingerprint(&self.paths),
        };
        if key.tool_use_id.is_some() && l.dialogs.values().any(|k| k.same_dialog(&key)) {
            return Ok(None);
        }
        let older: Vec<String> = self
            .store
            .pending_permissions(task_id)?
            .into_iter()
            .filter(|p| p.session_id == session_id && l.dialogs.get(&p.id).is_some_and(|k| k.agent == key.agent))
            .map(|p| p.id)
            .collect();
        let tool = ev.tool_name.clone().unwrap_or_else(|| "unknown".into());
        let card = self.store.create_permission_request(session_id, &tool, &key.detail)?;
        for id in older {
            self.store.answer_permission(&id, PermissionStatus::Denied)?;
            if let Some(l) = self.live.get_mut(task_id) {
                l.dialogs.remove(&id);
            }
            let payload = json!({ "permission_id": id, "by": card.id, "agent_id": key.agent });
            self.event(Some(task_id), "permission_superseded", payload)?;
        }
        let id = card.id.clone();
        if let Some(l) = self.live.get_mut(task_id) {
            l.dialogs.insert(card.id, key);
            l.card_gen.fetch_add(1, Ordering::SeqCst);
        }
        Ok(Some(id))
    }

    /// Starts tailing a transcript the first time a hook names it. A file that already
    /// exists (a resumed session appends to the same file) is tailed from its end, so old
    /// turns are not replayed into the fresh engine; the persisted offset wins when the
    /// session already tailed this path.
    fn follow_transcript(&mut self, task_id: &str, session_id: &str, path: &str) -> Result<()> {
        let Some(l) = self.live.get_mut(task_id) else {
            return Ok(());
        };
        if l.transcript_path.as_deref() == Some(path) {
            return Ok(());
        }
        l.transcript_path = Some(path.to_string());
        let driver = l.driver.clone();
        let session = self.store.get_session(session_id)?;
        let offset = if session.transcript_path.as_deref() == Some(path) {
            session.transcript_offset
        } else {
            std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
        };
        self.store.set_transcript(session_id, path, offset)?;
        if let Some(d) = driver {
            d.tail_transcript(std::path::Path::new(path), offset);
        }
        Ok(())
    }
}

#[cfg(test)]
impl super::testkit::Kit {
    /// Created, started and booted (`Ready`), with no prompt queued.
    pub fn started(&self, agent: AgentKind, branch: &str) -> TaskView {
        let t = self.create(agent, branch);
        self.hub.start_session(StartSession { task: t.task.id.clone(), prompt: None }, false).unwrap();
        self.emit(DriverEvent::Boot(BootState::Ready));
        self.hub.get_task(&t.task.id).unwrap()
    }

    pub fn drain(&self) {
        self.hub.drain(&self.rx);
    }

    pub fn emit(&self, event: DriverEvent) {
        self.factory.emit(event);
        self.drain();
    }

    pub fn hook(&self, task_id: &str, payload: Value) {
        self.hub
            .apply_hook(mirante_ipc::types::HookParams { task: task_id.into(), payload: payload.to_string(), ts_ms: 0 })
            .unwrap();
    }

    /// A hook stamped with the Mirante session it ran under (`MIRANTE_SESSION_ID`).
    pub fn hook_session(&self, task_id: &str, payload: Value, session: &str) {
        self.hub
            .apply_hook_envelope(mirante_ipc::types::HookEnvelope {
                params: HookParams { task: task_id.into(), payload: payload.to_string(), ts_ms: 0 },
                event_id: None,
                received_ts_ms: None,
                truncated: None,
                session: Some(session.into()),
            })
            .unwrap();
    }

    /// A hook as `mirante-hook` sends it: with an event id.
    pub fn hook_env(&self, task_id: &str, payload: Value, event_id: &str) {
        self.hub
            .apply_hook_envelope(mirante_ipc::types::HookEnvelope {
                params: HookParams { task: task_id.into(), payload: payload.to_string(), ts_ms: 0 },
                event_id: Some(event_id.into()),
                received_ts_ms: None,
                truncated: None,
                session: None,
            })
            .unwrap();
    }

    pub fn calls(&self) -> Vec<String> {
        self.factory.driver.calls()
    }

    /// Turns Claude inline answers on (off by default).
    pub fn inline(&self) {
        self.hub.set_claude_inline_answers(true);
    }

    /// The transcript shows the main agent's tool call `id` (the answer evidence).
    pub fn tool_use(&self, id: &str) {
        let line = serde_json::json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": id, "name": "Bash", "input": {}}]}});
        self.emit(DriverEvent::Transcript(line));
    }

    pub fn pending(&self, task_id: &str) -> Vec<mirante_core::model::PermissionRequest> {
        self.hub.lock().store.pending_permissions(task_id).unwrap()
    }
}

impl Hub {
    /// A workspace task's session starts with Mirante's own files of its folder as Mirante
    /// wrote them (`workspace::restore_files`): what an earlier session changed there is
    /// written again (`workspace_files_restored`). A failure is logged; the session starts.
    fn restore_workspace_files(&self, task: &mirante_core::model::Task) {
        let Some(ws) = task.workspace_path.as_deref() else {
            return;
        };
        let body = match self.lock().store.workspace_body(&task.id) {
            Ok(Some(b)) => b,
            Ok(None) => return,
            Err(e) => {
                self.lock().log_error(Some(&task.id), "reading the workspace files", &e);
                return;
            }
        };
        let done = mirante_core::workspace::restore_files(&self.worktrees, std::path::Path::new(ws), &body);
        let mut st = self.lock();
        match done {
            Ok(fixed) if fixed.is_empty() => {}
            Ok(fixed) => {
                if let Err(e) = st.event(Some(&task.id), "workspace_files_restored", json!({ "files": fixed })) {
                    st.log_error(Some(&task.id), "workspace files", &e);
                }
            }
            Err(e) => st.log_error(Some(&task.id), "restoring the workspace files", &e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::Kit;
    use super::*;
    use mirante_core::model::PermissionStatus;
    use mirante_ipc::types::SendMessage;
    use serde_json::json;

    fn status(k: &Kit, task: &str) -> Status {
        k.hub.get_task(task).unwrap().task.status
    }

    #[test]
    fn spawn_request_carries_mcp_only_for_enabled_repos() {
        use super::super::dna::Caller;
        let k = Kit::new();
        let bin = k.dir.path().join("bin/mirante-mcp");
        *k.factory.mcp_bin.lock().unwrap() = Some(bin.clone());
        let t = k.create(AgentKind::Claude, "mem").task;
        let cycle = || {
            k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, false).unwrap();
            k.hub.stop(&t.id).unwrap();
            k.factory.mcps.lock().unwrap().last().cloned().expect("spawned")
        };
        assert_eq!(cycle(), None, "nothing before memory is enabled");
        k.hub.dna_enable(&k.repo_ref(), &Caller::Person).unwrap();
        assert_eq!(cycle(), Some(bin.clone()));
        k.hub.dna_disable(&k.repo_ref(), &Caller::Person).unwrap();
        assert_eq!(cycle(), None, "disable stops injection for the next session");
        assert!(!k.kinds(&t.id).contains(&"dna.unavailable".to_string()));
        // Enabled but the binary is missing: the session still starts, without memory, and
        // says why.
        k.hub.dna_enable(&k.repo_ref(), &Caller::Person).unwrap();
        *k.factory.mcp_bin.lock().unwrap() = None;
        assert_eq!(cycle(), None);
        let events = k.hub.lock().store.task_events(&t.id).unwrap();
        let why = events.iter().find(|e| e.kind == "dna.unavailable").expect("dna.unavailable");
        assert!(why.payload["reason"].as_str().unwrap().contains("mirante-mcp"), "{}", why.payload);
    }

    #[test]
    fn a_tool_step_is_redacted_capped_and_never_shows_a_prompt() {
        let hook = |v: Value| serde_json::from_value::<HookEvent>(v).unwrap();
        let step = tool_step(&hook(json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "B".repeat(100),
            "agent_type": "A".repeat(100),
            "tool_input": {"command": format!("TOKEN=abc {}", "z".repeat(300))},
        })))
        .unwrap();
        assert_eq!(step["tool"].as_str().unwrap().chars().count(), 64);
        assert_eq!(step["agent"].as_str().unwrap().chars().count(), 64);
        let summary = step["summary"].as_str().unwrap();
        assert!(summary.starts_with("TOKEN=«redacted» zz") && summary.chars().count() == 200, "{summary}");
        let task = tool_step(&hook(json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Task",
            "tool_input": {"prompt": "the whole secret plan"},
        })))
        .unwrap();
        assert_eq!(task["summary"], "");
    }

    #[test]
    fn a_claude_tool_that_ran_becomes_a_feed_step() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "steps").task;
        let bash = json!({"command": "cargo test"});
        k.hook(&t.id, json!({"hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_input": bash}));
        let edit = json!({"file_path": "/w/src/lib.rs"});
        let failed = json!({"hook_event_name": "PostToolUseFailure", "tool_name": "Edit", "tool_input": edit,
            "agent_type": "Explore", "agent_id": "a7386d060a40b63d6"});
        k.hook(&t.id, failed);
        k.hook(
            &t.id,
            json!({"hook_event_name": "PreToolUse", "tool_name": "Read", "tool_input": {"file_path": "/w/a"}}),
        );
        let events = k.hub.lock().store.task_events(&t.id).unwrap();
        let steps: Vec<Value> = events.into_iter().filter(|e| e.kind == "step").map(|e| e.payload).collect();
        assert_eq!(
            steps,
            vec![
                json!({"type": "tool", "tool": "Bash", "summary": "cargo test", "ok": true, "agent": null, "agent_id": null}),
                json!({"type": "tool", "tool": "Edit", "summary": "/w/src/lib.rs", "ok": false, "agent": "Explore",
                    "agent_id": "a7386d060a40b63d6"}),
            ]
        );
    }

    fn steps(k: &Kit, task: &str) -> Vec<Value> {
        let events = k.hub.lock().store.task_events(task).unwrap();
        events.into_iter().filter(|e| e.kind == "step").map(|e| e.payload).collect()
    }

    /// What Claude says (its main-chain text) is a feed step, so the cockpit can show the
    /// agent's last answer; side agents' text and tool calls are not.
    #[test]
    fn claude_text_in_the_transcript_becomes_an_agent_message_step() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "answer").task;
        let text = |t: &str, stop: Value| json!({"type": "assistant", "message": {"stop_reason": stop, "content": [{"type": "text", "text": t}]}});
        k.emit(DriverEvent::Transcript(text("Looking at the code.", Value::Null)));
        k.tool_use("tu1");
        let mut side = text("side agent text", Value::Null);
        side["isSidechain"] = json!(true);
        k.emit(DriverEvent::Transcript(side));
        let secret = "token ghp_0123456789abcdefghijklmnopqrstuvwxyzAB";
        k.emit(DriverEvent::Transcript(text(&format!("Done: 3 files changed. {secret}"), json!("end_turn"))));
        k.emit(DriverEvent::Transcript(text(&"x".repeat(9000), json!("end_turn"))));
        // A text on the way (not the turn's end) is kept short: only final answers in full.
        k.emit(DriverEvent::Transcript(text(&"y".repeat(9000), Value::Null)));
        let s = steps(&k, &t.id);
        assert_eq!(s.len(), 4, "{s:?}");
        assert_eq!(s[3]["text"].as_str().unwrap().chars().count(), crate::driver::AGENT_MESSAGE_PARTIAL_CAP);
        assert_eq!(s[0], json!({"type": "agent_message", "text": "Looking at the code.", "final": false}));
        assert_eq!(s[1]["final"], true);
        let said = s[1]["text"].as_str().unwrap();
        assert!(said.starts_with("Done: 3 files changed.") && !said.contains("ghp_0123"), "{said}");
        assert_eq!(s[2]["text"].as_str().unwrap().chars().count(), crate::driver::AGENT_MESSAGE_CAP);
    }

    /// Claude's permission mode, from every hook payload (`permission_mode`) and from the
    /// transcript's `permission-mode` lines: leaving manual mode is an event and shows on the
    /// task; the same mode again is not news.
    #[test]
    fn leaving_manual_mode_is_reported_from_hooks_and_the_transcript() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "mode").task;
        let hook = |mode: &str| json!({"hook_event_name": "UserPromptSubmit", "permission_mode": mode});
        k.hook(&t.id, hook("default"));
        k.hook(&t.id, hook("default"));
        assert_eq!(k.hub.get_task(&t.id).unwrap().permission_mode.as_deref(), Some("default"));
        k.hook(&t.id, hook("auto"));
        k.hook(&t.id, json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "permission_mode": "auto"}));
        assert_eq!(k.hub.get_task(&t.id).unwrap().permission_mode.as_deref(), Some("auto"));
        k.emit(DriverEvent::Transcript(json!({"type": "permission-mode", "permissionMode": "default"})));
        k.emit(DriverEvent::Transcript(json!({"type": "permission-mode", "permissionMode": "acceptEdits"})));
        let modes: Vec<Value> = k
            .hub
            .lock()
            .store
            .task_events(&t.id)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "permission_mode")
            .map(|e| json!([e.payload["from"], e.payload["mode"], e.payload["self_approves"], e.payload["source"]]))
            .collect();
        assert_eq!(
            modes,
            vec![
                json!([null, "default", null, "hook"]),
                json!(["default", "auto", "commands", "hook"]),
                json!(["auto", "default", null, "transcript"]),
                json!(["default", "acceptEdits", "edits", "transcript"]),
            ]
        );
        // A side agent's transcript line is not the session's mode.
        k.emit(DriverEvent::Transcript(
            json!({"type": "permission-mode", "permissionMode": "plan", "isSidechain": true}),
        ));
        assert_eq!(k.hub.get_task(&t.id).unwrap().permission_mode.as_deref(), Some("acceptEdits"));
        // Not live: no mode.
        k.hub.stop(&t.id).unwrap();
        assert_eq!(k.hub.get_task(&t.id).unwrap().permission_mode, None);
    }

    /// Review: a resumed Claude session had no mode until its first hook, so a session
    /// that approved on its own lost its warning across a restart. The last mode that
    /// approves on its own is carried over (`source: "resume"`) until a hook says otherwise;
    /// a manual one is not (nothing to warn about).
    #[test]
    fn a_resumed_session_keeps_a_self_approving_mode_until_a_hook_says_otherwise() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "mode").task;
        k.emit(DriverEvent::NativeId("nat-mode".into()));
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "permission_mode": "auto"}));
        k.hub.stop(&t.id).unwrap();
        k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, true).unwrap();
        assert_eq!(k.hub.get_task(&t.id).unwrap().permission_mode.as_deref(), Some("auto"));
        let last = k.hub.lock().store.task_events(&t.id).unwrap().into_iter().rfind(|e| e.kind == "permission_mode");
        assert_eq!(last.unwrap().payload["source"], "resume");
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "permission_mode": "default"}));
        assert_eq!(k.hub.get_task(&t.id).unwrap().permission_mode.as_deref(), Some("default"));
        // Manual last time: nothing carried over; a fresh start never carries anything.
        k.hub.stop(&t.id).unwrap();
        k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, true).unwrap();
        assert_eq!(k.hub.get_task(&t.id).unwrap().permission_mode, None);
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "permission_mode": "acceptEdits"}));
        k.hub.stop(&t.id).unwrap();
        k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, false).unwrap();
        assert_eq!(k.hub.get_task(&t.id).unwrap().permission_mode, None);
    }

    /// The process's exit is applied while `stop` runs (the pump thread can get there
    /// before the caller re-takes the lock): the end cause must still be the caller's.
    fn exit_during_stop(k: &Kit, task: &str) {
        let hub = std::sync::Arc::clone(&k.hub);
        let session = hub.lock().live.get(task).unwrap().session_id.clone();
        *k.factory.driver.on_stop.lock().unwrap() =
            Some(Box::new(move || hub.handle_driver_event_guarded(&session, DriverEvent::Exited { code: Some(0) })));
    }

    fn end_cause(k: &Kit, task: &str) -> String {
        let events = k.hub.lock().store.task_events(task).unwrap();
        let ended = events.iter().rev().find(|e| e.kind == "status" && e.payload["to"] == json!("ended")).unwrap();
        ended.payload["cause"].as_str().unwrap().to_string()
    }

    #[test]
    fn a_killed_command_is_its_own_event() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "x").task.id;
        k.emit(DriverEvent::Step(json!({ "type": "command_killed", "items": ["c1"], "pids": [42] })));
        let ev = k.hub.lock().store.task_events(&t).unwrap();
        let e = ev.iter().find(|e| e.kind == "command_killed").expect("command_killed");
        assert_eq!(e.payload["pids"], json!([42]));
    }

    #[test]
    fn an_exit_racing_stop_shutdown_or_hibernate_keeps_mirantes_cause() {
        let k = Kit::new();
        let a = k.started(AgentKind::Claude, "a").task.id;
        exit_during_stop(&k, &a);
        k.hub.stop(&a).unwrap();
        assert_eq!(end_cause(&k, &a), "mirante:stopped");
        let b = k.started(AgentKind::Codex, "b").task.id;
        exit_during_stop(&k, &b);
        k.hub.hibernate(&b).unwrap();
        assert_eq!(end_cause(&k, &b), "mirante:hibernated");
        assert!(k.hub.get_task(&b).unwrap().hibernated);
        // An exit nobody asked for is still the process's.
        let d = k.started(AgentKind::Claude, "d").task.id;
        k.emit(DriverEvent::Exited { code: Some(1) });
        assert_eq!(end_cause(&k, &d), "mirante:process_exited");
        let c = k.started(AgentKind::Claude, "c").task.id;
        exit_during_stop(&k, &c);
        k.hub.shutdown();
        assert_eq!(end_cause(&k, &c), "mirante:daemon_shutdown");
    }

    #[test]
    fn start_takes_the_per_task_lock_and_boot_ready_delivers_the_first_prompt() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "a").task;
        let r = k.hub.start_session(StartSession { task: t.id.clone(), prompt: Some("do it".into()) }, false).unwrap();
        assert_eq!(r.boot, BootState::Booting);
        assert!(!r.resumed);
        let again = k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, false);
        assert!(matches!(again, Err(HubError::Conflict(_))));
        assert_eq!(k.hub.get_task(&t.id).unwrap().queued, 1);
        k.emit(DriverEvent::Boot(BootState::Ready));
        assert_eq!(k.calls(), vec!["send:do it".to_string()]);
        let v = k.hub.get_task(&t.id).unwrap();
        assert_eq!((v.live, v.boot, v.queued, v.task.lifecycle), (true, Some(BootState::Ready), 0, Lifecycle::Active));
    }

    #[test]
    fn hooks_drive_status_turns_and_the_native_id() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "SessionStart", "session_id": "nat-1", "source": "startup"}));
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "hi"}));
        assert_eq!(status(&k, &t.id), Status::Working);
        k.hook(&t.id, json!({"hook_event_name": "Stop"}));
        assert_eq!(status(&k, &t.id), Status::Idle);
        let st = k.hub.lock();
        let conv = st.store.latest_conversation(&t.id).unwrap().unwrap();
        assert_eq!(conv.native_session_id.as_deref(), Some("nat-1"));
        let session = st.store.live_session(&t.id).unwrap().unwrap();
        let turns = st.store.turns(&session.id).unwrap();
        assert_eq!(turns.len(), 1);
        assert!(turns[0].ended_at.is_some());
    }

    #[test]
    fn hook_permission_request_is_persisted_and_settled_when_the_tool_runs() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "touch x"}));
        k.hook(
            &t.id,
            json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_input": {"command": "touch x"}}),
        );
        assert_eq!(status(&k, &t.id), Status::NeedsYou);
        let v = k.hub.get_task(&t.id).unwrap();
        assert_eq!(v.needs_you, 1);
        let pending = k.hub.lock().store.pending_permissions(&t.id).unwrap();
        assert_eq!((pending[0].tool.as_str(), pending[0].detail.as_str()), ("Bash", "touch x"));
        // The user answered "Yes" inside `mirante attach`: the tool runs.
        k.hook(&t.id, json!({"hook_event_name": "PostToolUse", "tool_name": "Bash"}));
        assert_eq!(status(&k, &t.id), Status::Working);
        assert!(k.hub.lock().store.pending_permissions(&t.id).unwrap().is_empty());
        assert!(k.kinds(&t.id).contains(&"permission_settled".to_string()));
    }

    #[test]
    fn deny_in_the_terminal_is_settled_by_the_transcript_turn_end() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "touch x"}));
        k.emit(DriverEvent::Transcript(
            json!({"type": "user", "origin": {"kind": "human"}, "message": {"content": "touch x"}}),
        ));
        k.hook(
            &t.id,
            json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_input": {"command": "touch x"}}),
        );
        k.emit(DriverEvent::Transcript(json!({"type": "system", "subtype": "turn_duration"})));
        assert_eq!(status(&k, &t.id), Status::Idle);
        let ev = k.hub.lock().store.task_events(&t.id).unwrap();
        let settled = ev.iter().find(|e| e.kind == "permission_settled").unwrap();
        assert_eq!(settled.payload["answer"], json!(PermissionStatus::Denied));
    }

    #[test]
    fn transcript_path_starts_the_tailer_at_the_end_of_an_existing_file() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        let path = k.dir.path().join("t.jsonl");
        std::fs::write(&path, "{\"old\":1}\n").unwrap();
        let p = path.to_string_lossy().into_owned();
        k.hook(&t.id, json!({"hook_event_name": "SessionStart", "source": "resume", "transcript_path": p}));
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "transcript_path": p}));
        let tails: Vec<String> = k.calls().into_iter().filter(|c| c.starts_with("tail:")).collect();
        assert_eq!(tails, vec![format!("tail:{p}:10")]);
        k.emit(DriverEvent::TranscriptOffset { path: p.clone(), offset: 99 });
        let s = k.hub.lock().store.live_session(&t.id).unwrap().unwrap();
        assert_eq!((s.transcript_path.as_deref(), s.transcript_offset), (Some(p.as_str()), 99));
    }

    #[test]
    fn hooks_for_unknown_or_ended_sessions_are_logged_not_applied() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        assert_eq!(status(&k, &t.id), Status::Idle);
        assert!(k.kinds(&t.id).contains(&"hook_orphan".to_string()));
        let bad = k.hub.apply_hook(HookParams { task: t.id.clone(), payload: "nope".into(), ts_ms: 0 });
        assert!(matches!(bad, Err(HubError::Invalid(_))));
    }

    #[test]
    fn process_exit_ends_the_session_expires_permissions_and_allows_resume() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "SessionStart", "session_id": "nat-9", "source": "startup"}));
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        k.hook(&t.id, json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash"}));
        k.emit(DriverEvent::Exited { code: Some(0) });
        let v = k.hub.get_task(&t.id).unwrap();
        assert_eq!((v.task.status, v.live, v.needs_you), (Status::Ended, false, 0));
        assert!(k.kinds(&t.id).contains(&"permission_expired".to_string()));
        let r = k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, true).unwrap();
        assert!(r.resumed);
        assert_eq!(k.factory.resumes.lock().unwrap().last().cloned().flatten().as_deref(), Some("nat-9"));
        assert_eq!(status(&k, &t.id), Status::Idle);
    }

    #[test]
    fn resume_without_a_native_id_starts_fresh_and_says_so() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.emit(DriverEvent::Exited { code: None });
        let r = k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, true).unwrap();
        assert!(!r.resumed);
    }

    #[test]
    fn spawn_failure_releases_the_lock_and_ends_the_session() {
        let k = Kit::new();
        let t = k.create(AgentKind::Codex, "a").task;
        k.factory.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, false).is_err());
        assert_eq!(status(&k, &t.id), Status::Ended);
        k.factory.fail.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, false).is_ok());
    }

    #[test]
    fn trust_is_only_answered_when_the_prompt_is_open() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "a").task;
        k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, false).unwrap();
        assert!(matches!(k.hub.trust(TrustAnswer { task: t.id.clone(), accept: true }), Err(HubError::Conflict(_))));
        k.emit(DriverEvent::Boot(BootState::TrustPrompt));
        assert_eq!(k.hub.get_task(&t.id).unwrap().needs_you, 1);
        let r = k.hub.trust(TrustAnswer { task: t.id.clone(), accept: true }).unwrap();
        assert_eq!(r.boot, BootState::Booting);
        assert_eq!(k.calls(), vec!["trust:true".to_string()]);
        assert!(k.kinds(&t.id).contains(&"trust_answered".to_string()));
    }

    /// Task 21 live run: Claude is launched with its session id, so the native id is known
    /// before the trust prompt; answering trust on a fresh start must not say "resumed".
    #[test]
    fn trust_on_a_fresh_start_is_not_reported_as_resumed() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "a").task;
        let r = k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, false).unwrap();
        assert!(!r.resumed);
        k.emit(DriverEvent::NativeId("7c4d2e92".into()));
        k.emit(DriverEvent::Boot(BootState::TrustPrompt));
        let r = k.hub.trust(TrustAnswer { task: t.id.clone(), accept: true }).unwrap();
        assert!(!r.resumed, "a fresh conversation was not resumed");
    }

    #[test]
    fn stop_and_shutdown_end_sessions_through_the_driver() {
        let k = Kit::new();
        let a = k.started(AgentKind::Claude, "a").task;
        let b = k.started(AgentKind::Codex, "b").task;
        let v = k.hub.stop("a").unwrap();
        assert_eq!((v.task.status, v.live), (Status::Ended, false));
        assert!(matches!(k.hub.stop(&a.id), Err(HubError::Conflict(_))));
        k.hub.shutdown();
        assert_eq!(status(&k, &b.id), Status::Ended);
        assert_eq!(k.calls().iter().filter(|c| *c == "stop").count(), 2);
        assert_eq!(k.hub.status().live_sessions, 0);
    }

    #[test]
    fn shutdown_leaves_the_database_checkpointed() {
        let k = Kit::with_store(|p| mirante_core::store::Store::open(&p.database()).unwrap());
        k.started(AgentKind::Codex, "c");
        let wal = std::path::PathBuf::from(format!("{}-wal", k.hub.paths().database().display()));
        assert!(std::fs::metadata(&wal).unwrap().len() > 0);
        k.hub.shutdown();
        assert_eq!(std::fs::metadata(&wal).unwrap().len(), 0);
    }

    #[test]
    fn shutdown_waits_for_an_in_flight_launch_then_stops_it_and_refuses_new_ones() {
        let k = Kit::new();
        let t = k.create(AgentKind::Codex, "c").task;
        let (entered_tx, entered) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        *k.factory.spawn_gate.lock().unwrap() = Some((entered_tx, release_rx));
        let hub = k.hub.clone();
        let id = t.id.clone();
        let launch = std::thread::spawn(move || hub.start_session(StartSession { task: id, prompt: None }, false));
        entered.recv().unwrap();
        // The agent is being spawned: shutdown waits for it instead of exiting under it.
        let hub = k.hub.clone();
        let down = std::thread::spawn(move || hub.shutdown());
        std::thread::sleep(Duration::from_millis(150));
        assert!(!down.is_finished(), "shutdown returned while an agent was being spawned");
        // No new session starts while the daemon goes down.
        let other = k.create(AgentKind::Codex, "d").task;
        let r = k.hub.start_session(StartSession { task: other.id.clone(), prompt: None }, false);
        assert!(matches!(&r, Err(HubError::Conflict(m)) if m.contains("shutting down")), "{r:?}");
        release.send(()).unwrap();
        down.join().unwrap();
        let _ = launch.join().unwrap();
        // The agent that was spawning is stopped, not left running without a daemon.
        assert_eq!(k.calls().iter().filter(|c| *c == "stop").count(), 1, "{:?}", k.calls());
        assert_eq!(k.hub.status().live_sessions, 0);
        assert_eq!(status(&k, &t.id), Status::Ended);
    }

    #[test]
    fn codex_status_events_drive_status_turns_and_detail() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        k.emit(DriverEvent::NativeId("thr-1".into()));
        k.emit(DriverEvent::Status {
            to: Status::Working,
            cause: "codex:turn/started".into(),
            turn_open: true,
            detail: None,
        });
        let detail = Some("turn done, 1 command still finishing".to_string());
        k.emit(DriverEvent::Status {
            to: Status::Working,
            cause: "codex:turn/completed".into(),
            turn_open: false,
            detail: detail.clone(),
        });
        let v = k.hub.get_task(&t.id).unwrap();
        assert_eq!((v.task.status, v.detail), (Status::Working, detail));
        k.emit(DriverEvent::Status {
            to: Status::Idle,
            cause: "codex:item/completed".into(),
            turn_open: false,
            detail: None,
        });
        assert_eq!(status(&k, &t.id), Status::Idle);
        let st = k.hub.lock();
        assert_eq!(st.store.latest_conversation(&t.id).unwrap().unwrap().native_session_id.as_deref(), Some("thr-1"));
        let s = st.store.live_session(&t.id).unwrap().unwrap();
        assert_eq!(st.store.turns(&s.id).unwrap().len(), 1);
    }

    #[test]
    fn events_of_an_ended_session_are_ignored() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        let old = k.factory.last_sink();
        k.emit(DriverEvent::Exited { code: Some(1) });
        old.emit(DriverEvent::Status { to: Status::Working, cause: "late".into(), turn_open: true, detail: None });
        k.drain();
        assert_eq!(status(&k, &t.id), Status::Ended);
    }

    #[test]
    fn attach_goes_through_the_live_driver() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        let (_d, a) = k.hub.attach(AttachParams { task: t.id.clone(), rows: 30, cols: 100 }).unwrap();
        assert_eq!(a.initial, b"screen");
        assert_eq!(a.output.recv().unwrap(), b"live");
        assert!(k.calls().contains(&"attach:30x100".to_string()));
        // 0 x 0 (a re-attach) keeps the PTY's size; any other size is clamped as before.
        k.hub.attach(AttachParams { task: t.id.clone(), rows: 0, cols: 0 }).unwrap();
        k.hub.attach(AttachParams { task: t.id.clone(), rows: 1, cols: 1 }).unwrap();
        let calls = k.calls();
        assert!(calls.contains(&"attach:0x0".to_string()) && calls.contains(&"attach:10x40".to_string()), "{calls:?}");
    }

    /// The cockpit re-attaches whenever a terminal view comes back: the audit keeps the
    /// session's first attach, not one event (and one refresh in every client) per view.
    #[test]
    fn only_the_first_attach_of_a_session_is_recorded() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        for _ in 0..3 {
            k.hub.attach(AttachParams { task: t.id.clone(), rows: 30, cols: 100 }).unwrap();
        }
        let attached = k.kinds(&t.id).iter().filter(|k| *k == "attached").count();
        assert_eq!(attached, 1);
    }

    /// Tool call `t2` belongs to a side agent (`a-1`): one agent never has two dialogs open
    /// (a newer one supersedes its older card).
    fn with_agent(mut v: Value, id: &str) -> Value {
        if id == "t2" {
            v["agent_id"] = json!("a-1");
        }
        v
    }

    fn tool(name: &str, id: &str) -> Value {
        with_agent(json!({"hook_event_name": name, "tool_name": "Bash", "tool_use_id": id}), id)
    }

    fn perm(id: &str, cmd: &str) -> Value {
        with_agent(
            json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_use_id": id,
                "tool_input": {"command": cmd}}),
            id,
        )
    }

    /// Started Claude task with two dialogs open (t1 "older" from the main agent, then t2
    /// "newer" from side agent `a-1`).
    fn two_dialogs(k: &Kit) -> String {
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "go"}));
        k.emit(DriverEvent::Transcript(
            json!({"type": "user", "origin": {"kind": "human"}, "message": {"content": "go"}}),
        ));
        k.hook(&t.id, tool("PreToolUse", "t1"));
        k.hook(&t.id, perm("t1", "older"));
        k.hook(&t.id, tool("PreToolUse", "t2"));
        k.hook(&t.id, perm("t2", "newer"));
        assert_eq!(status(k, &t.id), Status::NeedsYou);
        assert_eq!(k.pending(&t.id).len(), 2);
        t.id
    }

    fn settled(k: &Kit, task: &str) -> Vec<(String, Value)> {
        let st = k.hub.lock();
        st.store
            .task_events(task)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "permission_settled")
            .map(|e| (e.payload["permission_id"].as_str().unwrap().to_string(), e.payload["answer"].clone()))
            .collect()
    }

    #[test]
    fn post_tool_use_settles_only_the_matching_card() {
        let k = Kit::new();
        let t = two_dialogs(&k);
        let older = k.pending(&t)[0].id.clone();
        // The user allowed the older dialog inside `mirante attach`: its tool ran.
        k.hook(&t, tool("PostToolUse", "t1"));
        let left = k.pending(&t);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].detail, "newer");
        assert_eq!(status(&k, &t), Status::NeedsYou);
        assert_eq!(settled(&k, &t), vec![(older, json!(PermissionStatus::AllowedOnce))]);
        k.hook(&t, tool("PostToolUse", "t2"));
        assert!(k.pending(&t).is_empty());
        assert_eq!(status(&k, &t), Status::Working);
    }

    #[test]
    fn a_mirante_answer_never_settles_the_other_cards() {
        let k = Kit::new();
        let t = two_dialogs(&k);
        // Whatever the engine says, a status change caused by Mirante's own answer must not
        // settle the cards Mirante did not answer.
        k.hub
            .lock()
            .after_input(&t, Some((Status::NeedsYou, Status::Working, "mirante:permission_allowed".into())), true)
            .unwrap();
        assert_eq!(k.pending(&t).len(), 2);
        assert!(settled(&k, &t).is_empty());
    }

    #[test]
    fn a_codex_status_change_from_mirantes_own_answer_settles_no_other_card() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        for key in ["1", "2"] {
            k.emit(DriverEvent::PermissionRequested { key: key.into(), tool: "command".into(), detail: key.into() });
        }
        let status = |to, cause: &str| DriverEvent::Status { to, cause: cause.into(), turn_open: true, detail: None };
        k.emit(status(Status::NeedsYou, "codex:approval_requested"));
        k.emit(status(Status::Working, "mirante:approval_answered"));
        assert_eq!(k.pending(&t.id).len(), 2);
        // The turn ending is what settles whatever is left.
        k.emit(status(Status::NeedsYou, "codex:approval_requested"));
        k.emit(status(Status::Working, "codex:turn/completed"));
        assert!(k.pending(&t.id).is_empty());
    }

    #[test]
    fn the_turn_end_settles_every_remaining_card_as_denied() {
        let k = Kit::new();
        let t = two_dialogs(&k);
        k.emit(DriverEvent::Transcript(json!({"type": "system", "subtype": "turn_duration"})));
        assert_eq!(status(&k, &t), Status::Idle);
        assert!(k.pending(&t).is_empty());
        let s = settled(&k, &t);
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(|(_, a)| *a == json!(PermissionStatus::Denied)));
    }

    #[test]
    fn a_hook_event_delivered_twice_is_applied_once() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook_env(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "go"}), "e1");
        k.hook_env(&t.id, perm("t1", "ls"), "e2");
        k.hook_env(&t.id, perm("t1", "ls"), "e2");
        assert_eq!(k.pending(&t.id).len(), 1);
        // No event id (an old hook binary): applied as before.
        k.hook(&t.id, json!({"hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_use_id": "t1"}));
        assert_eq!(status(&k, &t.id), Status::Working);
    }

    #[test]
    fn the_hook_dedupe_window_is_bounded() {
        let mut seen = SeenHooks::default();
        assert!(seen.insert("first"));
        assert!(!seen.insert("first"));
        for i in 0..HOOK_DEDUPE_WINDOW {
            assert!(seen.insert(&format!("e{i}")));
        }
        assert_eq!(seen.len(), HOOK_DEDUPE_WINDOW);
        // The oldest id fell out of the window.
        assert!(seen.insert("first"));
    }

    #[test]
    fn hooks_of_another_native_session_are_dropped_once_the_native_id_is_known() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        // Before the native id is known nothing is dropped.
        k.hook(&t.id, json!({"hook_event_name": "SessionStart", "session_id": "nat-1", "source": "startup"}));
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "session_id": "stray", "prompt": "x"}));
        assert_eq!(status(&k, &t.id), Status::Idle);
        assert!(k.kinds(&t.id).contains(&"hook_orphan".to_string()));
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "session_id": "nat-1", "prompt": "x"}));
        assert_eq!(status(&k, &t.id), Status::Working);
        // A SessionStart always passes: `/clear` starts a new native session.
        k.hook(&t.id, json!({"hook_event_name": "SessionStart", "session_id": "nat-2", "source": "clear"}));
        assert_eq!(k.hub.lock().live[&t.id].native_id.as_deref(), Some("nat-2"));
        // Hooks without a session id are kept.
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "y"}));
        assert_eq!(status(&k, &t.id), Status::Working);
    }

    #[test]
    fn stop_during_spawn_stops_the_late_driver() {
        let k = Kit::new();
        let t = k.create(AgentKind::Codex, "a").task;
        let (entered_tx, entered) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        *k.factory.spawn_gate.lock().unwrap() = Some((entered_tx, release_rx));
        let hub = k.hub.clone();
        let id = t.id.clone();
        let starter = std::thread::spawn(move || {
            hub.start_session(StartSession { task: id, prompt: None }, false).map(|r| r.boot)
        });
        entered.recv().unwrap();
        let v = k.hub.stop(&t.id).unwrap();
        assert_eq!((v.task.status, v.live), (Status::Ended, false));
        release.send(()).unwrap();
        let boot = starter.join().unwrap();
        assert!(matches!(boot, Ok(BootState::Exited) | Err(_)), "{boot:?}");
        assert_eq!(k.calls(), vec!["stop".to_string()]);
        assert_eq!(k.hub.status().live_sessions, 0);
        // The per-task lock is free again.
        assert!(k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, false).is_ok());
    }

    #[test]
    fn start_is_refused_while_the_task_is_being_archived() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "a").task;
        k.hub.lock().store.transition_lifecycle(&t.id, Lifecycle::Archiving).unwrap();
        let r = k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, false);
        assert!(matches!(r, Err(HubError::Conflict(_))), "{:?}", r.err());
        assert!(matches!(k.hub.archive(&t.id), Err(HubError::Conflict(_))));
        k.hub.lock().store.transition_lifecycle(&t.id, Lifecycle::Active).unwrap();
        assert_eq!(k.hub.archive(&t.id).unwrap().task.lifecycle, Lifecycle::Archived);
    }

    #[test]
    fn a_late_hook_from_an_earlier_session_of_the_task_is_dropped() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        let old = k.hub.lock().live[&t.id].session_id.clone();
        k.hub.stop(&t.id).unwrap();
        k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, false).unwrap();
        k.emit(DriverEvent::Boot(BootState::Ready));
        let new = k.hub.lock().live[&t.id].session_id.clone();
        assert_ne!(old, new);
        // The old Claude process's hook arrives after the new session started.
        k.hook_session(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "late"}), &old);
        k.hook_session(&t.id, perm("t1", "late"), &old);
        assert_eq!(status(&k, &t.id), Status::Idle);
        assert!(k.pending(&t.id).is_empty());
        let stale: Vec<Value> = k
            .hub
            .lock()
            .store
            .task_events(&t.id)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "hook_stale")
            .map(|e| e.payload)
            .collect();
        assert_eq!(stale.len(), 2);
        assert_eq!(stale[0]["session"], json!(old));
        assert_eq!(stale[0]["live_session"], json!(new));
        // The live session's own hooks pass.
        k.hook_session(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "now"}), &new);
        assert_eq!(status(&k, &t.id), Status::Working);
    }

    #[test]
    fn hooks_without_a_session_stamp_are_still_accepted() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "old hook binary"}));
        assert_eq!(status(&k, &t.id), Status::Working);
        assert!(!k.kinds(&t.id).contains(&"hook_stale".to_string()));
    }

    #[test]
    fn post_tool_use_failure_settles_its_card_as_allowed() {
        let k = Kit::new();
        let t = two_dialogs(&k);
        let older = k.pending(&t)[0].id.clone();
        // The user allowed the older dialog in the terminal; the tool ran and failed.
        k.hook(&t, tool("PostToolUseFailure", "t1"));
        assert_eq!(k.pending(&t).len(), 1, "the other card is not settled");
        assert_eq!(settled(&k, &t), vec![(older, json!(PermissionStatus::AllowedOnce))]);
        let ev = k.hub.lock().store.task_events(&t).unwrap();
        let s = ev.iter().find(|e| e.kind == "permission_settled").unwrap();
        assert_eq!(s.payload["cause"], json!("hook:PostToolUseFailure"));
        k.hook(&t, tool("PostToolUseFailure", "t2"));
        assert!(k.pending(&t).is_empty());
        assert_eq!(status(&k, &t), Status::Working);
        k.hook(&t, json!({"hook_event_name": "StopFailure"}));
        assert_eq!(status(&k, &t), Status::Idle);
    }

    #[test]
    fn a_panic_handling_one_driver_event_is_logged_and_the_next_still_applies() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        k.emit(DriverEvent::Step(json!({ "test_panic": true })));
        k.emit(DriverEvent::Status { to: Status::Working, cause: "codex:x".into(), turn_open: true, detail: None });
        assert_eq!(status(&k, &t.id), Status::Working);
        let ev = k.hub.lock().store.task_events(&t.id).unwrap();
        let err = ev.iter().find(|e| e.kind == "daemon_error").expect("panic logged");
        assert_eq!(err.payload["context"], json!("driver event panicked"));
    }

    #[test]
    fn the_pump_thread_survives_a_panicking_event() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        let super::super::testkit::Kit { hub, rx, factory, dir: _dir, .. } = k;
        let pump = hub.spawn_pump(rx);
        factory.emit(DriverEvent::Step(json!({ "test_panic": true })));
        factory.emit(DriverEvent::Status {
            to: Status::Working,
            cause: "codex:x".into(),
            turn_open: true,
            detail: None,
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while hub.get_task(&t.id).unwrap().task.status != Status::Working {
            assert!(Instant::now() < deadline, "the pump stopped applying events");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!pump.is_finished());
    }

    fn kinds_payloads(k: &Kit, task: &str, kind: &str) -> Vec<Value> {
        let st = k.hub.lock();
        st.store.task_events(task).unwrap().into_iter().filter(|e| e.kind == kind).map(|e| e.payload).collect()
    }

    #[test]
    fn a_resolved_codex_request_settles_only_its_own_card() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        k.emit(DriverEvent::PermissionRequested { key: "0".into(), tool: "command".into(), detail: "ls".into() });
        k.emit(DriverEvent::PermissionRequested { key: "7".into(), tool: "command".into(), detail: "pwd".into() });
        k.emit(DriverEvent::PermissionResolved { key: "0".into() });
        let left: Vec<String> = k.pending(&t.id).into_iter().map(|p| p.detail).collect();
        assert_eq!(left, vec!["pwd".to_string()]);
        let settled = kinds_payloads(&k, &t.id, "permission_settled");
        assert_eq!(settled.len(), 1);
        assert_eq!(
            (settled[0]["answer"].as_str(), settled[0]["cause"].as_str()),
            (Some("denied"), Some("codex:request_resolved"))
        );
        // Unknown or repeated keys change nothing.
        k.emit(DriverEvent::PermissionResolved { key: "0".into() });
        k.emit(DriverEvent::PermissionResolved { key: "99".into() });
        assert_eq!(k.pending(&t.id).len(), 1);
        assert_eq!(k.hub.lock().live[&t.id].perm_keys.len(), 1);
    }

    #[test]
    fn an_aborted_delivery_goes_back_to_the_front_of_the_queue() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "work"}));
        k.hub.send(SendMessage { task: t.id.clone(), text: "steer me".into(), mode: SendMode::Steer }).unwrap();
        k.hub.send(SendMessage { task: t.id.clone(), text: "later".into(), mode: SendMode::Queue }).unwrap();
        k.emit(DriverEvent::DeliveryAborted { text: "steer me".into(), reason: "a dialog appeared".into() });
        {
            let st = k.hub.lock();
            let l = &st.live[&t.id];
            let q: Vec<&str> = l.queue.iter().map(|q| q.text.as_str()).collect();
            assert_eq!(q, ["steer me", "later"]);
            assert!(l.steers.is_empty());
        }
        let ev = kinds_payloads(&k, &t.id, "message_requeued");
        assert_eq!(ev.len(), 1);
        assert_eq!((&ev[0]["text"], &ev[0]["reason"]), (&json!("steer me"), &json!("a dialog appeared")));
        assert!(ev[0]["id"].is_string(), "a requeued message has an id: {ev:?}");
        // At the turn end the requeued message goes out first.
        k.hook(&t.id, json!({"hook_event_name": "Stop"}));
        assert_eq!(k.calls().last().map(String::as_str), Some("send:steer me"));
    }

    #[test]
    fn several_handed_back_messages_are_delivered_in_their_original_order() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "work"}));
        for text in ["one", "two", "three"] {
            k.hub.send(SendMessage { task: t.id.clone(), text: text.into(), mode: SendMode::Steer }).unwrap();
        }
        k.hub.send(SendMessage { task: t.id.clone(), text: "later".into(), mode: SendMode::Queue }).unwrap();
        for text in ["one", "two", "three"] {
            k.emit(DriverEvent::DeliveryAborted { text: text.into(), reason: "hold".into() });
        }
        {
            let st = k.hub.lock();
            let q: Vec<String> = st.live[&t.id].queue.iter().map(|q| q.text.clone()).collect();
            assert_eq!(q, ["one", "two", "three", "later"]);
        }
        for _ in 0..4 {
            k.hook(&t.id, json!({"hook_event_name": "Stop"}));
            k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "x"}));
        }
        let sent: Vec<String> = k.calls().into_iter().filter(|c| c.starts_with("send:")).collect();
        assert_eq!(sent[sent.len() - 4..], ["send:one", "send:two", "send:three", "send:later"]);
    }

    #[test]
    fn a_failed_resume_is_recorded_and_fresh_start_stays_the_users_call() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.emit(DriverEvent::ResumeFailed { reason: "exited after 0.4 s".into() });
        k.emit(DriverEvent::Exited { code: Some(1) });
        let ev = kinds_payloads(&k, &t.id, "resume_failed");
        assert_eq!(ev.len(), 1);
        assert!(ev[0]["hint"].as_str().unwrap().contains("--fresh"));
        // Nothing was started on its own.
        assert_eq!(k.hub.status().live_sessions, 0);
        assert_eq!(k.factory.resumes.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_vanished_native_conversation_fails_the_resume_with_a_typed_error() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        k.emit(DriverEvent::NativeId("thr-gone".into()));
        k.hub.stop(&t.id).unwrap();
        *k.factory.fail_with.lock().unwrap() = Some(DriverError::NativeGone("no rollout found for thr-gone".into()));
        let r = k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, true);
        assert!(matches!(r, Err(HubError::Driver(DriverError::NativeGone(_)))), "{r:?}");
        assert_eq!(kinds_payloads(&k, &t.id, "resume_failed").len(), 1);
        assert_eq!(k.hub.status().live_sessions, 0);
        // The user chooses a fresh start explicitly.
        *k.factory.fail_with.lock().unwrap() = None;
        let fresh = k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, false).unwrap();
        assert!(!fresh.resumed);
        assert_eq!(k.factory.resumes.lock().unwrap().last().cloned(), Some(None));
    }

    #[test]
    fn usage_reports_show_in_the_task_view() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        assert_eq!(k.hub.get_task(&t.id).unwrap().usage, None);
        k.emit(DriverEvent::Usage("5h 42% · 31k tokens".into()));
        assert_eq!(k.hub.get_task(&t.id).unwrap().usage.as_deref(), Some("5h 42% · 31k tokens"));
        assert!(kinds_payloads(&k, &t.id, "usage").is_empty(), "usage is not written to the audit trail");
    }

    #[test]
    fn a_pending_permission_holds_the_drivers_typing() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        let sink = k.factory.last_sink();
        assert!(!sink.held());
        k.inline();
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        k.tool_use("t1");
        k.hook(&t.id, perm("t1", "ls"));
        assert!(sink.held());
        let id = k.pending(&t.id)[0].id.clone();
        k.hub.answer_permission(mirante_ipc::types::AnswerPermission { id, allow: true }).unwrap();
        assert!(!sink.held());
    }

    #[test]
    fn session_start_reaches_the_driver_and_fenced_hooks_update_the_native_id() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        let session = k.hub.lock().live[&t.id].session_id.clone();
        k.hook_session(
            &t.id,
            json!({"hook_event_name": "SessionStart", "session_id": "nat-1", "source": "startup"}),
            &session,
        );
        assert!(k.calls().contains(&"session_started".to_string()));
        // Claude's own id changed under the same Mirante session: adopted, not dropped.
        k.hook_session(
            &t.id,
            json!({"hook_event_name": "UserPromptSubmit", "session_id": "nat-2", "prompt": "x"}),
            &session,
        );
        assert_eq!(status(&k, &t.id), Status::Working);
        assert_eq!(k.hub.lock().live[&t.id].native_id.as_deref(), Some("nat-2"));
        let st = k.hub.lock();
        assert_eq!(st.store.latest_conversation(&t.id).unwrap().unwrap().native_session_id.as_deref(), Some("nat-2"));
    }

    #[test]
    fn compact_keeps_what_classification_reads() {
        assert!(compact(&json!({"type": "user", "isSidechain": true})).is_none());
        assert!(compact(&json!({"type": "progress"})).is_none());
        let a =
            compact(&json!({"type": "assistant", "message": {"stop_reason": "end_turn", "content": "big"}})).unwrap();
        assert_eq!(a, json!({"type": "assistant", "message": {"stop_reason": "end_turn"}}));
    }

    #[test]
    fn a_session_start_seen_while_the_driver_spawns_reaches_it_once_attached() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "a").task;
        let (entered_tx, entered) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        *k.factory.spawn_gate.lock().unwrap() = Some((entered_tx, release_rx));
        let (hub, id) = (k.hub.clone(), t.id.clone());
        let starter = std::thread::spawn(move || hub.start_session(StartSession { task: id, prompt: None }, false));
        entered.recv().unwrap();
        // The hook beats the spawn: no driver to tell yet.
        k.hook(&t.id, json!({"hook_event_name": "SessionStart", "session_id": "nat-1", "source": "startup"}));
        assert!(!k.calls().contains(&"session_started".to_string()));
        release.send(()).unwrap();
        starter.join().unwrap().unwrap();
        assert_eq!(k.calls().iter().filter(|c| *c == "session_started").count(), 1);
    }

    #[test]
    fn a_closed_dialog_redelivers_the_requeued_message() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hub.send(SendMessage { task: t.id.clone(), text: "go".into(), mode: SendMode::Queue }).unwrap();
        assert_eq!(k.calls(), vec!["send:go".to_string()]);
        k.emit(DriverEvent::DeliveryAborted { text: "go".into(), reason: "dialog: a dialog is open".into() });
        assert_eq!(k.calls().len(), 1, "nothing is retried while the dialog is up");
        k.emit(DriverEvent::DialogClosed);
        assert_eq!(k.calls(), vec!["send:go".to_string(), "send:go".to_string()]);
        assert_eq!(k.hub.get_task(&t.id).unwrap().queued, 0);
    }

    #[test]
    fn a_blocked_driver_send_is_a_conflict_that_says_why() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "work"}));
        *k.factory.driver.send_error.lock().unwrap() =
            Some(DriverError::NotNow("hold: a permission request is pending".into()));
        let e = k.hub.send(SendMessage { task: t.id.clone(), text: "x".into(), mode: SendMode::Steer }).unwrap_err();
        let rpc: mirante_ipc::rpc::RpcError = e.into();
        assert_eq!(rpc.code, mirante_ipc::rpc::code::CONFLICT);
        assert!(rpc.message.starts_with("hold"), "{}", rpc.message);
    }

    #[test]
    fn a_hook_that_fails_midway_still_brings_the_hold_flags_up_to_date() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        let sink = k.factory.last_sink();
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        k.hook(&t.id, perm("t1", "ls"));
        assert!(sink.held());
        {
            // The card is settled and the session ended behind the hub's back (no publish):
            // the flag is stale, and the next card cannot be stored.
            let mut st = k.hub.lock();
            let id = st.store.pending_permissions(&t.id).unwrap()[0].id.clone();
            st.store.answer_permission(&id, PermissionStatus::Denied).unwrap();
            let session = st.live[&t.id].session_id.clone();
            st.store.end_session(&session).unwrap();
        }
        assert!(sink.held());
        let r = k.hub.apply_hook(HookParams { task: t.id.clone(), payload: perm("t2", "pwd").to_string(), ts_ms: 0 });
        assert!(r.is_err());
        assert!(!sink.held(), "the error path must sync the hold flags too");
    }

    #[test]
    fn a_native_id_emitted_before_a_failed_spawn_is_not_kept() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "a").task;
        *k.factory.emit_then_fail.lock().unwrap() = Some(DriverEvent::NativeId("nat-never".into()));
        assert!(k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, false).is_err());
        k.drain();
        let conv = k.hub.lock().store.latest_conversation(&t.id).unwrap().unwrap();
        assert_eq!(conv.native_session_id, None);
        *k.factory.emit_then_fail.lock().unwrap() = None;
        let r = k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, true).unwrap();
        assert!(!r.resumed, "nothing to resume: the process never ran");
    }
}
