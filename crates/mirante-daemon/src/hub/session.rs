//! Session lifecycle: start / resume (one live session per task), boot and trust, stop,
//! and every input that moves a session's status: hook events (Claude, via `hook.event`)
//! and driver events (transcript lines, Codex status, approvals, exit).
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mirante_core::delivery::{classify_message, Delivery};
use mirante_core::hook::HookEvent;
use mirante_core::model::{AgentKind, Lifecycle, PermissionStatus, Status};
use mirante_core::status::{is_sidechain, Input};
use mirante_ipc::types::{
    AttachParams, BootState, HookEnvelope, HookParams, SendMode, StartResult, StartSession, TaskView, TrustAnswer,
};
use serde_json::{json, Value};

use super::{DialogKey, EventRx, Hub, HubError, Live, Result, State};
use crate::driver::{Attachment, Driver, DriverEvent, Sink, SpawnRequest};

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

impl State {
    /// Delivers the next queued message when the session is ready and idle.
    pub(crate) fn kick(&mut self, task_id: &str) -> Result<()> {
        let status = self.store.get_task(task_id)?.status;
        let Some(l) = self.live.get_mut(task_id) else {
            return Ok(());
        };
        if l.boot != BootState::Ready || status != Status::Idle || l.queue.is_empty() || l.delivering() {
            return Ok(());
        }
        let Some(driver) = l.driver.clone() else {
            return Ok(());
        };
        let text = l.queue.pop_front().expect("not empty");
        match driver.send(&text) {
            Ok(()) => {
                l.delivery_in_flight = Some(Instant::now());
                self.event(Some(task_id), "message_sent", json!({ "mode": SendMode::Queue, "text": text }))
            }
            Err(e) => {
                if let Some(l) = self.live.get_mut(task_id) {
                    l.queue.push_front(text);
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
            self.event(Some(task_id), "message_delivery", json!({ "text": text, "delivery": delivery }))?;
        }
        Ok(())
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
            self.event(Some(task_id), "queue_dropped", json!({ "messages": live.queue }))?;
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
    /// `mirante:approval_*`, already persisted) or because a tool ran (`hook:PostToolUse`,
    /// which settles only its own card) settles nothing else.
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
                || cause == "hook:PostToolUse";
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

    /// A `PostToolUse` of the live Claude session: the tool behind the oldest matching card
    /// ran, so the user allowed it (in the attached terminal).
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
            Some(p) => self.settle_card(task_id, &p.id, PermissionStatus::AllowedOnce, "hook:PostToolUse"),
            None => Ok(()),
        }
    }

    fn settle_card(&mut self, task_id: &str, id: &str, answer: PermissionStatus, cause: &str) -> Result<()> {
        self.store.answer_permission(id, answer)?;
        if let Some(l) = self.live.get_mut(task_id) {
            l.perm_keys.remove(id);
            l.dialogs.remove(id);
        }
        self.event(
            Some(task_id),
            "permission_settled",
            json!({ "permission_id": id, "answer": answer, "cause": cause }),
        )
    }

    fn handle(&mut self, task_id: &str, session_id: &str, event: DriverEvent) -> Result<()> {
        match event {
            DriverEvent::Transcript(line) => {
                if let Some(l) = self.live.get_mut(task_id) {
                    if let Some(c) = compact(&line) {
                        l.transcript.push(c);
                        let excess = l.transcript.len().saturating_sub(TRANSCRIPT_WINDOW);
                        l.transcript.drain(..excess);
                    }
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
            DriverEvent::NativeId(id) => self.set_native_id(task_id, session_id, &id),
            DriverEvent::Boot(state) => {
                if let Some(l) = self.live.get_mut(task_id) {
                    l.boot = state;
                }
                self.event(Some(task_id), "boot", json!({ "session_id": session_id, "state": state }))?;
                self.kick(task_id)
            }
            DriverEvent::Step(v) => self.event(Some(task_id), "step", v),
            DriverEvent::Exited { code } => {
                self.event(Some(task_id), "process_exited", json!({ "session_id": session_id, "code": code }))?;
                self.finish_session(task_id, session_id, "mirante:process_exited")
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

impl Hub {
    /// Starts (or, with `resume`, resumes) the task's agent. Fails with a conflict while a
    /// session is live (the per-task lock). Waits until the agent leaves `Booting`.
    pub fn start_session(&self, req: StartSession, resume: bool) -> Result<StartResult> {
        let (task, session_id, native) = {
            let mut st = self.lock();
            let task = st.resolve(&req.task)?;
            if task.lifecycle == Lifecycle::Archived {
                return Err(HubError::Conflict(format!("task {} is archived", task.branch)));
            }
            if st.live.contains_key(&task.id) {
                return Err(HubError::Conflict(format!("task {} already has a live session", task.branch)));
            }
            if st.archiving.contains(&task.id) {
                return Err(HubError::Conflict(format!("task {} is being archived", task.branch)));
            }
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
                st.event(
                    Some(&task.id),
                    "session_launch",
                    json!({ "session_id": session.id, "resume": native, "resume_requested": resume }),
                )
            });
            if let Err(e) = launched {
                // Nothing is live yet: release the per-task lock and fail.
                if let Err(undo) = st.store.end_session(&session.id) {
                    st.log_error(Some(&task.id), "session.start rollback", &undo);
                }
                st.publish();
                return Err(e);
            }
            let mut live = Live::new(&session.id, task.agent, native.clone());
            if let Some(p) = req.prompt.filter(|p| !p.trim().is_empty()) {
                live.queue.push_back(p);
            }
            st.live.insert(task.id.clone(), live);
            st.publish();
            (task, session.id, native)
        };
        let sink = Sink::new(&session_id, self.events.clone());
        let req = SpawnRequest {
            task: task.clone(),
            session_id: session_id.clone(),
            resume: native.clone(),
            paths: self.paths.clone(),
        };
        match self.factory.spawn(req, sink) {
            Ok(driver) => {
                let mut st = self.lock();
                let attached = match st.live.get_mut(&task.id).filter(|l| l.session_id == session_id) {
                    Some(l) => {
                        l.driver = Some(driver.clone());
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
                st.finish_session(&task.id, &session_id, "mirante:spawn_failed")?;
                st.publish();
                return Err(e.into());
            }
        }
        self.wait_boot(&task.id, &session_id, native.is_some())
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
            let out = (task.id.clone(), l.session_id.clone(), driver, l.native_id.is_some());
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
        if let Some(d) = driver {
            d.stop();
        }
        let mut st = self.lock();
        st.finish_session(&task_id, &session_id, "mirante:stopped")?;
        st.publish();
        let t = st.store.get_task(&task_id)?;
        st.view(t)
    }

    pub fn interrupt(&self, reference: &str) -> Result<()> {
        let (task_id, _, driver) = self.live_driver(reference)?;
        driver.ok_or_else(|| HubError::Conflict("agent still starting".into()))?.interrupt()?;
        let mut st = self.lock();
        st.event(Some(&task_id), "interrupt_sent", json!({}))?;
        st.publish();
        Ok(())
    }

    pub fn attach(&self, p: AttachParams) -> Result<(Arc<dyn Driver>, Attachment)> {
        let (task_id, _, driver) = self.live_driver(&p.task)?;
        let driver = driver.ok_or_else(|| HubError::Conflict("agent still starting".into()))?;
        let a = driver.attach(p.rows.max(10), p.cols.max(40))?;
        let mut st = self.lock();
        st.event(Some(&task_id), "attached", json!({ "attachment": a.id }))?;
        st.publish();
        Ok((driver, a))
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

    /// Stops every live session (daemon shutdown).
    pub fn shutdown(&self) {
        let live: Vec<LiveRef> = {
            let st = self.lock();
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
    }

    /// One Claude hook payload without an event id (see [`Hub::apply_hook_envelope`]).
    pub fn apply_hook(&self, p: HookParams) -> Result<()> {
        self.apply_hook_envelope(HookEnvelope { params: p, event_id: None, received_ts_ms: None, truncated: None })
    }

    /// One Claude hook payload as `mirante-hook` sends it (`hook.event`). An `event_id`
    /// already seen is dropped (the hook may deliver an event and also spool it). Records
    /// the native session id and the transcript path (starting the tailer), persists
    /// permission requests, and feeds the status engine. A payload for a task without a
    /// live Claude session, or (once the native session id is known) from another native
    /// session, is only logged.
    pub fn apply_hook_envelope(&self, e: HookEnvelope) -> Result<()> {
        let mut st = self.lock();
        if let Some(id) = &e.event_id {
            if !st.seen_hooks.insert(id) {
                return Ok(());
            }
        }
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
        if ev.hook_event_name != "SessionStart" {
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
        if ev.hook_event_name == "PermissionRequest" {
            st.open_card(&p.task, &session_id, &ev)?;
        }
        let tool_ran = ev.hook_event_name == "PostToolUse";
        st.feed_claude(&p.task, &Input::Hook(ev.clone()))?;
        if tool_ran {
            st.settle_tool_ran(&p.task, &session_id, &ev)?;
        }
        st.publish();
        Ok(())
    }

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

    /// Applies every queued driver event now (tests; the daemon uses the pump thread).
    pub fn drain(&self, rx: &EventRx) {
        while let Ok((session_id, event)) = rx.try_recv() {
            self.handle_driver_event(&session_id, event);
        }
    }

    pub fn spawn_pump(self: &Arc<Self>, rx: EventRx) -> std::thread::JoinHandle<()> {
        let hub = Arc::clone(self);
        std::thread::spawn(move || {
            for (session_id, event) in rx {
                hub.handle_driver_event(&session_id, event);
            }
        })
    }
}

impl State {
    /// Persists the card for a Claude `PermissionRequest` and remembers which tool call it
    /// is about. A repeat of a dialog that already has a pending card adds none.
    fn open_card(&mut self, task_id: &str, session_id: &str, ev: &HookEvent) -> Result<()> {
        let Some(l) = self.live.get(task_id) else {
            return Ok(());
        };
        let key = DialogKey {
            agent: ev.agent_id.clone(),
            tool_use_id: l.engine.tool_use_id_for(ev),
            tool_name: ev.tool_name.clone(),
        };
        if key.tool_use_id.is_some() && l.dialogs.values().any(|k| *k == key) {
            return Ok(());
        }
        let tool = ev.tool_name.clone().unwrap_or_else(|| "unknown".into());
        let card = self.store.create_permission_request(session_id, &tool, &ev.permission_detail())?;
        if let Some(l) = self.live.get_mut(task_id) {
            l.dialogs.insert(card.id, key);
        }
        Ok(())
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

    /// A hook as `mirante-hook` sends it: with an event id.
    pub fn hook_env(&self, task_id: &str, payload: Value, event_id: &str) {
        self.hub
            .apply_hook_envelope(mirante_ipc::types::HookEnvelope {
                params: HookParams { task: task_id.into(), payload: payload.to_string(), ts_ms: 0 },
                event_id: Some(event_id.into()),
                received_ts_ms: None,
                truncated: None,
            })
            .unwrap();
    }

    pub fn calls(&self) -> Vec<String> {
        self.factory.driver.calls()
    }

    pub fn pending(&self, task_id: &str) -> Vec<mirante_core::model::PermissionRequest> {
        self.hub.lock().store.pending_permissions(task_id).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::Kit;
    use super::*;
    use mirante_core::model::PermissionStatus;
    use serde_json::json;

    fn status(k: &Kit, task: &str) -> Status {
        k.hub.get_task(task).unwrap().task.status
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
    }

    fn tool(name: &str, id: &str) -> Value {
        json!({"hook_event_name": name, "tool_name": "Bash", "tool_use_id": id})
    }

    fn perm(id: &str, cmd: &str) -> Value {
        json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_use_id": id,
            "tool_input": {"command": cmd}})
    }

    /// Started Claude task with two dialogs open (t1 "older", then t2 "newer").
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
        k.hub.lock().archiving.insert(t.id.clone());
        let r = k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, false);
        assert!(matches!(r, Err(HubError::Conflict(_))), "{:?}", r.err());
        assert!(matches!(k.hub.archive(&t.id), Err(HubError::Conflict(_))));
        k.hub.lock().archiving.clear();
        k.hub.archive(&t.id).unwrap();
        assert!(k.hub.lock().archiving.is_empty());
    }

    #[test]
    fn compact_keeps_what_classification_reads() {
        assert!(compact(&json!({"type": "user", "isSidechain": true})).is_none());
        assert!(compact(&json!({"type": "progress"})).is_none());
        let a =
            compact(&json!({"type": "assistant", "message": {"stop_reason": "end_turn", "content": "big"}})).unwrap();
        assert_eq!(a, json!({"type": "assistant", "message": {"stop_reason": "end_turn"}}));
    }
}
