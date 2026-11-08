//! The editable queue: list, edit, remove and reorder the messages a live session holds
//! until its turn ends (`message.send` with `queue` adds them; the pump in `session`
//! delivers them).
//!
//! Race safety comes from the hub lock: the pump pops a message and hands it to the driver
//! under the same lock these calls take, so a message is either still queued (and the
//! change applies to what is delivered) or gone (`CONFLICT`: delivered or being delivered).
//! A delivery that is aborted hands the message back under its old id, editable again.
//!
//! The queue lives in memory with its session. Every change is a `queue_changed` event in
//! the store's event log (SQLite), which is also what clients refetch on and what crash
//! recovery replays to report the messages a restart dropped, in their final order.
use std::collections::HashSet;

use mirante_core::model::AgentKind;
use mirante_ipc::types::{EditQueued, QueuedMessage, ReorderQueue};
use serde_json::json;

use super::{Hub, HubError, Result, State};

/// One queued message (see [`QueuedMessage`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Queued {
    pub(crate) id: String,
    /// The sender's own words (never labelled).
    pub(crate) text: String,
    /// A person sent it: it reaches the agent behind the operator label ([`Queued::wire`]).
    pub(crate) operator: bool,
}

impl Queued {
    /// A message nobody vouches for as the person's (a task's first prompt, an agent's).
    pub(crate) fn new(text: impl Into<String>) -> Queued {
        Queued { id: mirante_core::model::new_id(), text: text.into(), operator: false }
    }

    /// A message from the person at the keyboard.
    pub(crate) fn from_operator(text: impl Into<String>, operator: bool) -> Queued {
        Queued { operator, ..Queued::new(text) }
    }

    /// What the agent is handed: the text, behind the operator label when a person sent it.
    pub(crate) fn wire(&self) -> String {
        wire(&self.text, self.operator)
    }

    fn view(&self) -> QueuedMessage {
        QueuedMessage { id: self.id.clone(), text: self.text.clone() }
    }
}

/// `text` as handed to the agent: labelled as the human operator's when `operator`
/// ([`mirante_core::delivery::label_operator`], never a slash command).
pub(crate) fn wire(text: &str, operator: bool) -> String {
    if operator {
        mirante_core::delivery::label_operator(text)
    } else {
        text.to_string()
    }
}

impl State {
    /// The live task whose queue holds message `id`, or why there is none: `CONFLICT` when
    /// the message left the queue for delivery, `NOT_FOUND` otherwise.
    fn queue_of(&self, id: &str) -> Result<String> {
        if let Some((task, _)) = self.live.iter().find(|(_, l)| l.queue.iter().any(|q| q.id == id)) {
            return Ok(task.clone());
        }
        if self.live.values().any(|l| l.left_queue.contains(id)) {
            return Err(HubError::Conflict(format!(
                "message {id} is no longer queued: it was delivered (or is being delivered)"
            )));
        }
        Err(HubError::NotFound(format!("no queued message {id}")))
    }

    fn queue_view(&self, task_id: &str) -> Vec<QueuedMessage> {
        self.live.get(task_id).map_or_else(Vec::new, |l| l.queue.iter().map(Queued::view).collect())
    }
}

impl Hub {
    /// The task's queued messages, next first (empty without a live session).
    pub fn list_queued(&self, task: &str) -> Result<Vec<QueuedMessage>> {
        let st = self.lock();
        let task = st.resolve(task)?;
        Ok(st.queue_view(&task.id))
    }

    /// Rewrites a message that is still queued. Returns the queue.
    pub fn edit_queued(&self, e: EditQueued) -> Result<Vec<QueuedMessage>> {
        if e.text.trim().is_empty() {
            return Err(HubError::Invalid("empty message".into()));
        }
        let mut st = self.lock();
        let task_id = st.queue_of(&e.id)?;
        let l = st.live.get_mut(&task_id).expect("queue_of found it");
        if l.agent == AgentKind::Claude {
            // The same check as `message.send`: the typist would refuse it at the turn end.
            crate::driver::paste::typed_message(&e.text).map_err(HubError::Invalid)?;
        }
        let q = l.queue.iter_mut().find(|q| q.id == e.id).expect("queue_of found it");
        q.text = e.text.clone();
        st.event(Some(&task_id), "queue_changed", json!({ "op": "edited", "id": e.id, "text": e.text }))?;
        st.publish();
        Ok(st.queue_view(&task_id))
    }

    /// Takes a message out of the queue (it is never delivered). Returns the queue.
    pub fn remove_queued(&self, id: &str) -> Result<Vec<QueuedMessage>> {
        let mut st = self.lock();
        let task_id = st.queue_of(id)?;
        let l = st.live.get_mut(&task_id).expect("queue_of found it");
        let at = l.queue.iter().position(|q| q.id == id).expect("queue_of found it");
        let gone = l.queue.remove(at).expect("in range");
        if at < l.requeue_at {
            l.requeue_at -= 1;
        }
        st.event(Some(&task_id), "queue_changed", json!({ "op": "removed", "id": gone.id, "text": gone.text }))?;
        st.publish();
        Ok(st.queue_view(&task_id))
    }

    /// Puts the queue in the order `r.ids`, which must name exactly the messages queued now.
    /// Returns the queue.
    pub fn reorder_queued(&self, r: ReorderQueue) -> Result<Vec<QueuedMessage>> {
        let mut st = self.lock();
        let task = st.resolve(&r.task_id)?;
        let Some(l) = st.live.get_mut(&task.id) else {
            return Err(HubError::Conflict(format!("task {} has no live session", task.branch)));
        };
        if let Some(id) = r.ids.iter().find(|id| l.left_queue.contains(*id)) {
            return Err(HubError::Conflict(format!(
                "message {id} is no longer queued: it was delivered (or is being delivered)"
            )));
        }
        let now: HashSet<&str> = l.queue.iter().map(|q| q.id.as_str()).collect();
        let asked: HashSet<&str> = r.ids.iter().map(String::as_str).collect();
        if asked.len() != r.ids.len() {
            return Err(HubError::Invalid("an id is listed twice".into()));
        }
        if now != asked {
            return Err(HubError::Conflict("the queue changed since it was read: reload it and try again".into()));
        }
        let mut old: Vec<Queued> = l.queue.drain(..).collect();
        for id in &r.ids {
            let at = old.iter().position(|q| q.id == *id).expect("same set");
            l.queue.push_back(old.swap_remove(at));
        }
        st.event(Some(&task.id), "queue_changed", json!({ "op": "reordered", "ids": r.ids }))?;
        st.publish();
        Ok(st.queue_view(&task.id))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::Kit;
    use super::*;
    use crate::driver::DriverEvent;
    use mirante_ipc::types::{SendMessage, SendMode};
    use serde_json::Value;
    use std::sync::{Arc, Barrier};

    /// A Claude task in a running turn (queued messages wait for its end).
    fn working(k: &Kit, branch: &str) -> String {
        let t = k.started(AgentKind::Claude, branch).task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "work"}));
        t.id
    }

    fn queue(k: &Kit, task: &str, text: &str) -> String {
        let r = k.hub.send(SendMessage { task: task.into(), text: text.into(), mode: SendMode::Queue }).unwrap();
        assert_eq!(r.delivered, SendMode::Queue);
        k.hub.list_queued(task).unwrap().last().unwrap().id.clone()
    }

    fn texts(k: &Kit, task: &str) -> Vec<String> {
        k.hub.list_queued(task).unwrap().into_iter().map(|q| q.text).collect()
    }

    fn changes(k: &Kit, task: &str) -> Vec<Value> {
        let st = k.hub.lock();
        let ev = st.store.task_events(task).unwrap();
        ev.into_iter().filter(|e| e.kind == "queue_changed").map(|e| e.payload).collect()
    }

    fn turn_end(k: &Kit, task: &str) {
        k.hook(task, json!({"hook_event_name": "Stop"}));
    }

    #[test]
    fn lists_the_queue_in_order_with_stable_ids() {
        let k = Kit::new();
        let t = working(&k, "a");
        let a = queue(&k, &t, "first");
        let b = queue(&k, &t, "second");
        let l = k.hub.list_queued("a").unwrap();
        assert_eq!(
            l,
            vec![QueuedMessage { id: a, text: "first".into() }, QueuedMessage { id: b, text: "second".into() }]
        );
        let idle = k.create(AgentKind::Codex, "b").task;
        assert_eq!(k.hub.list_queued(&idle.id).unwrap(), vec![]);
        assert!(matches!(k.hub.list_queued("nope"), Err(HubError::NotFound(_))));
    }

    #[test]
    fn an_edited_message_is_delivered_as_edited() {
        let k = Kit::new();
        let t = working(&k, "a");
        let id = queue(&k, &t, "run tests");
        let q = k.hub.edit_queued(EditQueued { id: id.clone(), text: "run all tests".into() }).unwrap();
        assert_eq!(q, vec![QueuedMessage { id: id.clone(), text: "run all tests".into() }]);
        assert_eq!(changes(&k, &t), vec![json!({"op": "edited", "id": id, "text": "run all tests"})]);
        turn_end(&k, &t);
        assert_eq!(k.calls().last().map(String::as_str), Some("send:run all tests"));
    }

    #[test]
    fn edits_are_checked_like_sends() {
        let k = Kit::new();
        let t = working(&k, "a");
        let id = queue(&k, &t, "x");
        let edit = |text: &str| k.hub.edit_queued(EditQueued { id: id.clone(), text: text.into() });
        assert!(matches!(edit("  "), Err(HubError::Invalid(_))));
        assert!(matches!(edit("!rm -rf build"), Err(HubError::Invalid(_))));
        assert!(matches!(
            k.hub.edit_queued(EditQueued { id: "nope".into(), text: "y".into() }),
            Err(HubError::NotFound(_))
        ));
        assert_eq!(texts(&k, &t), ["x"]);
        assert!(changes(&k, &t).is_empty());
    }

    #[test]
    fn a_removed_message_is_never_delivered() {
        let k = Kit::new();
        let t = working(&k, "a");
        let a = queue(&k, &t, "a");
        queue(&k, &t, "b");
        assert_eq!(k.hub.remove_queued(&a).unwrap().len(), 1);
        assert_eq!(k.hub.get_task(&t).unwrap().queued, 1);
        assert_eq!(changes(&k, &t), vec![json!({"op": "removed", "id": a, "text": "a"})]);
        turn_end(&k, &t);
        assert!(!k.calls().contains(&"send:a".to_string()));
        assert_eq!(k.calls().last().map(String::as_str), Some("send:b"));
        assert!(matches!(k.hub.remove_queued(&a), Err(HubError::NotFound(_))));
    }

    #[test]
    fn a_reordered_queue_is_delivered_in_the_new_order() {
        let k = Kit::new();
        let t = working(&k, "a");
        let ids: Vec<String> = ["a", "b", "c"].iter().map(|x| queue(&k, &t, x)).collect();
        let order = vec![ids[2].clone(), ids[0].clone(), ids[1].clone()];
        k.hub.reorder_queued(ReorderQueue { task_id: t.clone(), ids: order.clone() }).unwrap();
        assert_eq!(texts(&k, &t), ["c", "a", "b"]);
        assert_eq!(changes(&k, &t), vec![json!({"op": "reordered", "ids": order})]);
        for _ in 0..3 {
            turn_end(&k, &t);
            k.hook(&t, json!({"hook_event_name": "UserPromptSubmit", "prompt": "x"}));
        }
        let sent: Vec<String> = k.calls().into_iter().filter(|c| c.starts_with("send:")).collect();
        assert_eq!(sent, ["send:c", "send:a", "send:b"]);
    }

    #[test]
    fn a_reorder_must_name_exactly_the_queue() {
        let k = Kit::new();
        let t = working(&k, "a");
        let a = queue(&k, &t, "a");
        let b = queue(&k, &t, "b");
        let reorder = |ids: Vec<&String>| {
            k.hub.reorder_queued(ReorderQueue { task_id: t.clone(), ids: ids.into_iter().cloned().collect() })
        };
        assert!(matches!(reorder(vec![&b]), Err(HubError::Conflict(_))), "one missing");
        assert!(matches!(reorder(vec![&b, &a, &"x".to_string()]), Err(HubError::Conflict(_))), "an unknown one");
        assert!(matches!(reorder(vec![&b, &b]), Err(HubError::Invalid(_))));
        assert_eq!(texts(&k, &t), ["a", "b"]);
        let stopped = k.create(AgentKind::Claude, "s").task.id;
        let r = k.hub.reorder_queued(ReorderQueue { task_id: stopped, ids: vec![] });
        assert!(matches!(r, Err(HubError::Conflict(_))));
    }

    #[test]
    fn a_delivered_message_can_no_longer_be_changed() {
        let k = Kit::new();
        let t = working(&k, "a");
        let a = queue(&k, &t, "a");
        let b = queue(&k, &t, "b");
        turn_end(&k, &t);
        assert_eq!(k.calls().last().map(String::as_str), Some("send:a"));
        let edit = k.hub.edit_queued(EditQueued { id: a.clone(), text: "z".into() });
        assert!(matches!(edit, Err(HubError::Conflict(_))), "{edit:?}");
        assert!(matches!(k.hub.remove_queued(&a), Err(HubError::Conflict(_))));
        let r = k.hub.reorder_queued(ReorderQueue { task_id: t.clone(), ids: vec![b.clone(), a.clone()] });
        assert!(matches!(r, Err(HubError::Conflict(_))));
        assert_eq!(texts(&k, &t), ["b"]);
    }

    #[test]
    fn a_handed_back_message_keeps_its_id_and_is_editable_again() {
        let k = Kit::new();
        let t = working(&k, "a");
        let a = queue(&k, &t, "a");
        turn_end(&k, &t);
        assert_eq!(k.calls().last().map(String::as_str), Some("send:a"));
        k.emit(DriverEvent::DeliveryAborted { text: "a".into(), reason: "a dialog appeared".into() });
        assert_eq!(k.hub.list_queued(&t).unwrap(), vec![QueuedMessage { id: a.clone(), text: "a".into() }]);
        k.hub.edit_queued(EditQueued { id: a, text: "a again".into() }).unwrap();
        assert_eq!(texts(&k, &t), ["a again"]);
    }

    #[test]
    fn edits_racing_the_delivery_either_land_in_it_or_are_refused() {
        let k = Kit::new();
        let t = working(&k, "a");
        let mut refused = 0;
        for i in 0..20 {
            let id = queue(&k, &t, &format!("orig {i}"));
            let hub = Arc::clone(&k.hub);
            let go = Arc::new(Barrier::new(2));
            let start = Arc::clone(&go);
            // Edits back to back until the queue lets go of the message.
            let editor = std::thread::spawn(move || {
                start.wait();
                let mut last_ok = None;
                for n in 0.. {
                    let text = format!("edit {n} of {i}");
                    match hub.edit_queued(EditQueued { id: id.clone(), text: text.clone() }) {
                        Ok(_) => last_ok = Some(text),
                        Err(HubError::Conflict(_)) => return (last_ok, true),
                        Err(e) => panic!("unexpected {e:?}"),
                    }
                    if n > 100_000 {
                        return (last_ok, false);
                    }
                    // Leave the lock to the turn end now and then (the mutex is not fair).
                    std::thread::sleep(std::time::Duration::from_micros(20));
                }
                unreachable!()
            });
            go.wait();
            turn_end(&k, &t);
            let (last_ok, conflicted) = editor.join().unwrap();
            assert!(conflicted, "once delivered, an edit is refused");
            refused += 1;
            let expect = format!("send:{}", last_ok.unwrap_or(format!("orig {i}")));
            assert_eq!(k.calls().last(), Some(&expect), "the delivery carries the last accepted edit");
            assert_eq!(k.hub.list_queued(&t).unwrap(), vec![]);
            k.hook(&t, json!({"hook_event_name": "UserPromptSubmit", "prompt": "next"}));
        }
        assert_eq!(refused, 20);
    }

    #[test]
    fn a_remove_racing_the_delivery_either_prevents_it_or_is_refused() {
        let k = Kit::new();
        let t = working(&k, "a");
        for i in 0..20u64 {
            let id = queue(&k, &t, &format!("orig {i}"));
            let sent_before = k.calls().len();
            let hub = Arc::clone(&k.hub);
            let go = Arc::new(Barrier::new(2));
            let start = Arc::clone(&go);
            let remover = std::thread::spawn(move || {
                start.wait();
                // Spread the two over each other's timing.
                std::thread::sleep(std::time::Duration::from_micros(i * 50));
                hub.remove_queued(&id)
            });
            go.wait();
            turn_end(&k, &t);
            let delivered = k.calls()[sent_before..].contains(&format!("send:orig {i}"));
            match remover.join().unwrap() {
                Ok(_) => assert!(!delivered, "removed, so never sent"),
                Err(HubError::Conflict(_)) => assert!(delivered, "refused because it went out"),
                Err(e) => panic!("unexpected {e:?}"),
            }
            assert_eq!(k.hub.list_queued(&t).unwrap(), vec![]);
            k.hook(&t, json!({"hook_event_name": "UserPromptSubmit", "prompt": "next"}));
        }
    }
}
