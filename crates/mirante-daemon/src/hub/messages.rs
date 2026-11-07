//! Talking to a running agent: steer now or queue for the turn end, and permission
//! answers (the queue pump and the steer verdict live in `session`).
use mirante_core::model::{AgentKind, Lifecycle, PermissionRequest, PermissionStatus, Status};
use mirante_core::status::Input;
use mirante_ipc::types::{AnswerPermission, BootState, PermissionView, SendMessage, SendMode, SendResult};
use serde_json::json;

use super::{Hub, HubError, Result, State};

impl State {
    /// A pending request by full id or unique id suffix (at least 4 characters).
    fn find_permission(&self, id: &str) -> Result<PermissionRequest> {
        let id = id.trim();
        let mut found = Vec::new();
        for t in self.store.list_tasks(false)? {
            for p in self.store.pending_permissions(&t.id)? {
                if p.id == id || (id.len() >= 4 && p.id.ends_with(id)) {
                    found.push(p);
                }
            }
        }
        match found.len() {
            1 => Ok(found.remove(0)),
            0 => Err(HubError::NotFound(format!("no pending permission request {id}"))),
            _ => Err(HubError::Conflict(format!("permission id {id} is ambiguous"))),
        }
    }
}

impl State {
    /// An already answered request named by `id` (full id or unique suffix): returned as
    /// stored when its answer is `answer`, a conflict otherwise. `not_found` is the error
    /// when nothing matches.
    fn answered_permission(&self, id: &str, answer: PermissionStatus, not_found: String) -> Result<PermissionRequest> {
        let mut found: Vec<PermissionRequest> = self
            .store
            .permissions_matching(id.trim())?
            .into_iter()
            .filter(|p| p.status != PermissionStatus::Pending)
            .collect();
        match found.len() {
            0 => Err(HubError::NotFound(not_found)),
            1 => {
                let p = found.remove(0);
                if p.status == answer {
                    Ok(p)
                } else {
                    Err(HubError::Conflict(format!(
                        "permission request {} was already answered: {}",
                        p.id,
                        p.status.as_str()
                    )))
                }
            }
            _ => Err(HubError::Conflict(format!("permission id {id} is ambiguous"))),
        }
    }
}

impl Hub {
    /// Steer: hand the message to the agent now (starts a turn if idle). Refused while a
    /// dialog is open, because typed text would answer it. Queue: hold it until the running
    /// turn ends (delivered at once when the agent is already idle with an empty queue and
    /// no earlier delivery is still starting its turn). Before the agent is ready, both
    /// modes queue.
    pub fn send(&self, m: SendMessage) -> Result<SendResult> {
        if m.text.trim().is_empty() {
            return Err(HubError::Invalid("empty message".into()));
        }
        let mut st = self.lock();
        let task = st.resolve(&m.task)?;
        if task.agent == AgentKind::Claude {
            // Refused now rather than queued: the typist would refuse it at every turn end
            // (a `!` bash-mode line, a multi-line slash command).
            crate::driver::paste::typed_message(&m.text).map_err(HubError::Invalid)?;
        }
        let l = st.live.get_mut(&task.id).ok_or_else(|| {
            HubError::Conflict(format!("task {} has no live session: mirante resume {}", task.branch, task.branch))
        })?;
        let ready = l.boot == BootState::Ready && l.driver.is_some();
        let now = match m.mode {
            SendMode::Steer => {
                let dialog = l.driver.as_ref().is_some_and(|d| d.dialog_open());
                if dialog || (l.agent == AgentKind::Claude && task.status == Status::NeedsYou) {
                    return Err(HubError::NeedsYou(format!(
                        "task {} waits on a dialog: answer it first (mirante perms / mirante attach)",
                        task.branch
                    )));
                }
                ready
            }
            SendMode::Queue => ready && task.status == Status::Idle && l.queue.is_empty() && !l.delivering(),
        };
        if !now {
            l.queue.push_back(m.text.clone());
            let position = l.queue.len();
            st.event(Some(&task.id), "message_queued", json!({ "text": m.text, "position": position }))?;
            // An idle, ready agent with a backlog (an earlier delivery failed) takes the
            // next message now.
            st.kick(&task.id)?;
            let queued = st.live.get(&task.id).map_or(0, |l| l.queue.len());
            st.publish();
            return Ok(SendResult { delivered: SendMode::Queue, queued });
        }
        l.driver.clone().expect("ready implies a driver").send(&m.text)?;
        if l.agent == AgentKind::Claude && task.status == Status::Working {
            l.steers.push(m.text.clone());
        }
        if task.status == Status::Idle {
            l.delivery_in_flight = Some(std::time::Instant::now());
        }
        let queued = l.queue.len();
        st.event(Some(&task.id), "message_sent", json!({ "mode": m.mode, "text": m.text }))?;
        st.publish();
        Ok(SendResult { delivered: SendMode::Steer, queued })
    }

    /// Pending permission requests of all non-archived tasks, oldest first per task.
    pub fn permissions(&self) -> Result<Vec<PermissionView>> {
        let st = self.lock();
        let mut out = Vec::new();
        for t in st.store.list_tasks(false)? {
            if t.lifecycle == Lifecycle::Archived {
                continue;
            }
            for p in st.store.pending_permissions(&t.id)? {
                out.push(PermissionView { request: p, task_title: t.title.clone(), branch: t.branch.clone() });
            }
        }
        Ok(out)
    }

    /// Answers a pending request once: the driver acts first (Claude: types the exact
    /// one-time option; Codex: answers the JSON-RPC request); only then is the answer
    /// persisted. Claude dialogs are answered in order, oldest first. Once the driver has
    /// answered, the answer stands: a failure to record it is logged, not returned.
    /// Repeating the answer a request already has returns its stored row without acting
    /// again (a client retry); a different answer to it is a conflict.
    pub fn answer_permission(&self, a: AnswerPermission) -> Result<PermissionRequest> {
        let mut st = self.lock();
        let status = if a.allow { PermissionStatus::AllowedOnce } else { PermissionStatus::Denied };
        let p = match st.find_permission(&a.id) {
            Ok(p) => p,
            // Not pending: a retry of an answer that already stands returns the stored row
            // (idempotent); any other answer to an answered card is a conflict.
            Err(HubError::NotFound(m)) => return st.answered_permission(&a.id, status, m),
            Err(e) => return Err(e),
        };
        let l = st
            .live
            .get(&p.task_id)
            .filter(|l| l.session_id == p.session_id)
            .ok_or_else(|| HubError::Conflict("the session that asked has ended".into()))?;
        let driver = l.driver.clone().ok_or_else(|| HubError::Conflict("agent still starting".into()))?;
        let agent = l.agent;
        let key = match agent {
            AgentKind::Claude => {
                let oldest =
                    st.store.pending_permissions(&p.task_id)?.into_iter().find(|q| q.session_id == p.session_id);
                if oldest.as_ref().is_some_and(|o| o.id != p.id) {
                    return Err(HubError::Conflict(format!(
                        "answer the older request {} first (Claude shows one dialog at a time)",
                        oldest.map(|o| o.id).unwrap_or_default()
                    )));
                }
                p.detail.clone()
            }
            AgentKind::Codex => l.perm_keys.get(&p.id).cloned().ok_or_else(|| {
                HubError::Conflict(format!("permission request {} has no pending approval in the agent", p.id))
            })?,
        };
        driver.answer_permission(&key, a.allow)?;
        let answered = match st.store.answer_permission(&p.id, status) {
            Ok(answered) => answered,
            Err(e) => {
                st.log_error(Some(&p.task_id), "permission answered by the agent but not recorded", &e);
                PermissionRequest { status, answered_at: Some(mirante_core::model::now_ms()), ..p.clone() }
            }
        };
        if let Some(l) = st.live.get_mut(&p.task_id) {
            l.perm_keys.remove(&p.id);
            l.dialogs.remove(&p.id);
        }
        if agent == AgentKind::Claude {
            if let Err(e) = st.feed_claude(&p.task_id, &Input::PermissionAnswered { allowed: a.allow }) {
                st.log_error(Some(&p.task_id), "permission answer status update", &e);
            }
        }
        st.publish();
        Ok(answered)
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::Kit;
    use super::*;
    use crate::driver::DriverEvent;
    use serde_json::Value;
    use std::sync::atomic::Ordering;

    fn send(k: &Kit, task: &str, text: &str, mode: SendMode) -> Result<SendResult> {
        k.hub.send(SendMessage { task: task.into(), text: text.into(), mode })
    }

    #[test]
    fn steer_goes_out_now_and_queue_waits_for_the_turn_end() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "work"}));
        assert_eq!(send(&k, &t.id, "use tabs", SendMode::Steer).unwrap().delivered, SendMode::Steer);
        let r = send(&k, &t.id, "then run tests", SendMode::Queue).unwrap();
        assert_eq!((r.delivered, r.queued), (SendMode::Queue, 1));
        assert_eq!(k.calls(), vec!["send:use tabs".to_string()]);
        k.hook(&t.id, json!({"hook_event_name": "Stop"}));
        assert_eq!(k.calls(), vec!["send:use tabs".to_string(), "send:then run tests".to_string()]);
        assert_eq!(k.hub.get_task(&t.id).unwrap().queued, 0);
    }

    #[test]
    fn claude_messages_the_typist_would_refuse_are_refused_up_front() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "work"}));
        for text in ["!rm -rf build", "/compact\nnow"] {
            for mode in [SendMode::Steer, SendMode::Queue] {
                assert!(matches!(send(&k, &t.id, text, mode), Err(HubError::Invalid(_))), "{text:?}");
            }
        }
        assert_eq!(k.hub.get_task(&t.id).unwrap().queued, 0);
        assert!(send(&k, &t.id, "/compact", SendMode::Queue).is_ok());
    }

    #[test]
    fn queue_on_an_idle_agent_is_delivered_at_once() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        assert_eq!(send(&k, &t.id, "go", SendMode::Queue).unwrap().delivered, SendMode::Steer);
        assert_eq!(k.calls(), vec!["send:go".to_string()]);
    }

    #[test]
    fn steer_is_refused_while_a_dialog_is_open_but_queue_is_accepted() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        k.hook(&t.id, json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash"}));
        assert!(matches!(send(&k, &t.id, "1", SendMode::Steer), Err(HubError::NeedsYou(_))));
        assert_eq!(send(&k, &t.id, "later", SendMode::Queue).unwrap().delivered, SendMode::Queue);
        let c = k.started(AgentKind::Codex, "c").task;
        k.factory.driver.dialog.store(true, Ordering::SeqCst);
        assert!(matches!(send(&k, &c.id, "x", SendMode::Steer), Err(HubError::NeedsYou(_))));
        assert!(k.calls().is_empty());
    }

    #[test]
    fn messages_before_ready_are_queued_and_without_a_session_refused() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "a").task;
        assert!(matches!(send(&k, &t.id, "x", SendMode::Steer), Err(HubError::Conflict(_))));
        k.hub.start_session(mirante_ipc::types::StartSession { task: t.id.clone(), prompt: None }, false).unwrap();
        assert_eq!(send(&k, &t.id, "early", SendMode::Steer).unwrap().delivered, SendMode::Queue);
        assert!(matches!(send(&k, &t.id, " ", SendMode::Steer), Err(HubError::Invalid(_))));
        k.emit(DriverEvent::Boot(BootState::Ready));
        assert_eq!(k.calls(), vec!["send:early".to_string()]);
    }

    #[test]
    fn steer_verdict_comes_from_the_transcript_at_the_turn_end() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        let line = |v: Value| k.emit(DriverEvent::Transcript(v));
        line(json!({"type": "user", "origin": {"kind": "human"}, "message": {"content": "five steps"}}));
        send(&k, &t.id, "stop, say BANANA", SendMode::Steer).unwrap();
        line(json!({"type": "queue-operation", "operation": "enqueue", "content": "stop, say BANANA"}));
        line(json!({"type": "queue-operation", "operation": "remove", "reason": "absorbed_mid_turn"}));
        line(
            json!({"type": "assistant", "message": {"stop_reason": "end_turn", "content": [{"type": "text", "text": "BANANA"}]}}),
        );
        line(json!({"type": "system", "subtype": "turn_duration"}));
        let ev = k.hub.lock().store.task_events(&t.id).unwrap();
        let d = ev.iter().find(|e| e.kind == "message_delivery").expect("verdict");
        assert_eq!(d.payload["delivery"], json!("steered"));
    }

    #[test]
    fn claude_permission_answer_types_the_choice_then_persists_once() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        k.hook(
            &t.id,
            json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_input": {"command": "touch x"}}),
        );
        let p = k.hub.permissions().unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].branch, "a");
        let id = p[0].request.id.clone();
        let a = k.hub.answer_permission(AnswerPermission { id: id[id.len() - 6..].into(), allow: true }).unwrap();
        assert_eq!(a.status, PermissionStatus::AllowedOnce);
        assert_eq!(k.calls(), vec!["answer:touch x:true".to_string()]);
        assert_eq!(k.hub.get_task(&t.id).unwrap().task.status, Status::Working);
        assert!(matches!(k.hub.answer_permission(AnswerPermission { id, allow: false }), Err(HubError::Conflict(_))));
    }

    #[test]
    fn repeating_the_same_answer_returns_the_stored_row() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        k.hook(&t.id, json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash"}));
        let id = k.hub.permissions().unwrap()[0].request.id.clone();
        let first = k.hub.answer_permission(AnswerPermission { id: id.clone(), allow: true }).unwrap();
        // A retry (the client lost the reply), by full id or by suffix: same row, nothing typed again.
        let again = k.hub.answer_permission(AnswerPermission { id: id.clone(), allow: true }).unwrap();
        assert_eq!(again, first);
        let by_suffix = k.hub.answer_permission(AnswerPermission { id: id[id.len() - 6..].into(), allow: true });
        assert_eq!(by_suffix.unwrap(), first);
        assert_eq!(k.calls().iter().filter(|c| c.starts_with("answer:")).count(), 1);
        // A different answer to the answered card stays a conflict.
        let other = k.hub.answer_permission(AnswerPermission { id: id.clone(), allow: false });
        assert!(matches!(other, Err(HubError::Conflict(_))), "{other:?}");
        // The idempotent path also works once the session is gone.
        k.hub.stop(&t.id).unwrap();
        assert_eq!(k.hub.answer_permission(AnswerPermission { id, allow: true }).unwrap(), first);
        assert!(matches!(
            k.hub.answer_permission(AnswerPermission { id: "no-such-permission".into(), allow: true }),
            Err(HubError::NotFound(_))
        ));
    }

    #[test]
    fn a_failed_driver_answer_persists_nothing() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        k.hook(&t.id, json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash"}));
        k.factory.driver.fail_answers.store(true, Ordering::SeqCst);
        let id = k.hub.permissions().unwrap()[0].request.id.clone();
        assert!(matches!(
            k.hub.answer_permission(AnswerPermission { id, allow: true }),
            Err(HubError::Driver(crate::driver::DriverError::NoDialog(_)))
        ));
        assert_eq!(k.hub.permissions().unwrap().len(), 1);
    }

    #[test]
    fn codex_approval_is_answered_with_its_request_key() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        k.emit(DriverEvent::PermissionRequested {
            key: "7".into(),
            tool: "command".into(),
            detail: "rm -rf build".into(),
        });
        let id = k.hub.permissions().unwrap()[0].request.id.clone();
        k.hub.answer_permission(AnswerPermission { id, allow: false }).unwrap();
        assert_eq!(k.calls(), vec!["answer:7:false".to_string()]);
        assert!(k.hub.lock().live[&t.id].perm_keys.is_empty());
    }

    #[test]
    fn answering_the_older_of_two_dialogs_keeps_the_newer_one_pending_and_listed() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        for (id, cmd) in [("t1", "older"), ("t2", "newer")] {
            k.hook(&t.id, json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_use_id": id}));
            k.hook(
                &t.id,
                json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_use_id": id,
                    "tool_input": {"command": cmd}}),
            );
        }
        let p = k.hub.permissions().unwrap();
        k.hub.answer_permission(AnswerPermission { id: p[0].request.id.clone(), allow: true }).unwrap();
        let left = k.hub.permissions().unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].request.id, p[1].request.id);
        let v = k.hub.get_task(&t.id).unwrap();
        assert_eq!((v.task.status, v.needs_you), (Status::NeedsYou, 1));
    }

    #[test]
    fn queue_on_an_idle_agent_with_a_backlog_kicks_the_backlog() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        k.hub.lock().live.get_mut(&t.id).unwrap().queue.push_back("old".into());
        let r = send(&k, &t.id, "new", SendMode::Queue).unwrap();
        assert_eq!(r.delivered, SendMode::Queue);
        assert_eq!(k.calls(), vec!["send:old".to_string()]);
        assert_eq!(k.hub.get_task(&t.id).unwrap().queued, 1);
    }

    #[test]
    fn a_second_queued_message_waits_until_the_first_one_started_a_turn() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        assert_eq!(send(&k, &t.id, "a", SendMode::Queue).unwrap().delivered, SendMode::Steer);
        assert_eq!(send(&k, &t.id, "b", SendMode::Queue).unwrap().delivered, SendMode::Queue);
        assert_eq!(k.calls(), vec!["send:a".to_string()]);
        let status = |to, turn_open| DriverEvent::Status { to, cause: "codex:x".into(), turn_open, detail: None };
        k.emit(status(Status::Working, true));
        assert_eq!(k.calls(), vec!["send:a".to_string()]);
        k.emit(status(Status::Idle, false));
        assert_eq!(k.calls(), vec!["send:a".to_string(), "send:b".to_string()]);
    }

    #[test]
    fn codex_answer_without_a_request_key_is_a_conflict() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        k.emit(DriverEvent::PermissionRequested { key: "7".into(), tool: "command".into(), detail: "ls".into() });
        k.hub.lock().live.get_mut(&t.id).unwrap().perm_keys.clear();
        let id = k.hub.permissions().unwrap()[0].request.id.clone();
        assert!(matches!(k.hub.answer_permission(AnswerPermission { id, allow: true }), Err(HubError::Conflict(_))));
        assert!(k.calls().is_empty());
    }

    #[test]
    fn older_claude_dialog_must_be_answered_first() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        k.hook(&t.id, json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash"}));
        k.hook(&t.id, json!({"hook_event_name": "PermissionRequest", "tool_name": "Edit", "agent_id": "a-1"}));
        let p = k.hub.permissions().unwrap();
        let newer = p[1].request.id.clone();
        assert!(matches!(
            k.hub.answer_permission(AnswerPermission { id: newer, allow: true }),
            Err(HubError::Conflict(_))
        ));
    }
}
