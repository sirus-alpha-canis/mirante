//! Daemon start: agent processes do not outlive the daemon (their PTY / pipes close with
//! it), so every session still live in the store is ended, its pending permission requests
//! expire, messages it had queued but never sent are reported (`queue_dropped`), and the
//! task becomes `Ended`, ready for `mirante resume`.
//!
//! A task whose last session was hibernated is marked hibernated again (the mark is
//! replayed from its `memory.hibernated` event), so `mirante wake` still resumes it.
//!
//! An archive the crash interrupted (`Archiving`) is finished: the worktree is removed if
//! still there (git refuses a dirty one: the task then goes back to `Active`, with an
//! `archive_abandoned` event) and the task becomes `Archived`. Checkouts under the worktree
//! root that no task owns (a crash between `git worktree add` and the task row) are
//! reported as `worktree_orphan` events, never deleted.
//!
//! Then hook payloads
//! spooled while the daemon was down are ingested: a `SessionStart` among them still
//! records the native session id, so a session that started while the daemon was down
//! can be resumed.
//!
//! The spool is claimed first (renamed to `hook-spool.<boot_id>.jsonl`, so a hook that
//! spools meanwhile starts a fresh file instead of losing its line), then read. Lines are
//! [`HookEnvelope`]s: unparseable ones are skipped and logged, ones already seen (same
//! `event_id`) or older than the task's latest conversation are skipped. A store error is
//! logged as `daemon_error` and ingest goes on: recovery never stops the daemon starting.
use std::path::{Path, PathBuf};

use mirante_core::hook::HookEvent;
use mirante_core::model::{Lifecycle, PermissionStatus, Status, Task};
use mirante_ipc::types::HookEnvelope;
use serde_json::{json, Value};

use crate::hub::{Hub, Result, State};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Recovery {
    pub sessions_ended: usize,
    pub permissions_expired: usize,
    /// Non-empty spool lines read.
    pub spooled_hooks: usize,
    /// Lines whose `event_id` was already applied.
    pub spool_duplicates: usize,
    /// Lines older than the task's latest conversation.
    pub spool_stale: usize,
    pub spool_unparseable: usize,
    /// Sessions ended here whose queued messages never went out (one `queue_dropped` each).
    pub queued_dropped: usize,
    /// `Archiving` tasks whose archive recovery completed.
    pub archives_finished: usize,
    /// `Archiving` tasks put back to `Active` (their worktree could not be removed).
    pub archives_abandoned: usize,
    /// Checkouts under the worktree root that no task owns (reported, never deleted).
    pub orphan_worktrees: Vec<String>,
    /// Tasks whose last session the daemon itself ended (shutdown, or found live after a
    /// crash): what auto-resume brings back (see `hub::resume`).
    pub resume: Vec<String>,
}

/// What one spool line did.
enum Line {
    Applied,
    Duplicate,
    Stale,
    Unparseable,
}

impl Hub {
    pub fn recover(&self) -> Result<Recovery> {
        let mut report = Recovery::default();
        let mut st = self.lock();
        for task in st.store.list_tasks(false)? {
            if let Err(e) = end_stale_session(&mut st, &task.id, &mut report) {
                st.log_error(Some(&task.id), "recovery", &e);
            }
            match was_hibernated(&st, &task.id) {
                Ok(true) => {
                    st.mem.hibernated.insert(task.id.clone());
                }
                Ok(false) => {}
                Err(e) => st.log_error(Some(&task.id), "recovery: hibernation mark", &e),
            }
            let active = matches!(task.lifecycle, Lifecycle::Active | Lifecycle::Created);
            match st.store.task_events(&task.id) {
                Ok(events) if active && crate::hub::resume::ended_by_daemon(&events) => {
                    report.resume.push(task.id.clone())
                }
                Ok(_) => {}
                Err(e) => st.log_error(Some(&task.id), "recovery: auto-resume", &e),
            }
            if task.lifecycle == Lifecycle::Archiving {
                if let Err(e) = self.finish_archive(&mut st, &task, &mut report) {
                    st.log_error(Some(&task.id), "recovery: interrupted archive", &e);
                }
            }
        }
        self.report_orphans(&mut st, &mut report)?;
        for file in self.claim_spools(&mut st) {
            let text = match std::fs::read_to_string(&file) {
                Ok(t) => t,
                Err(e) => {
                    st.log_error(None, &format!("reading {}", file.display()), &e);
                    continue;
                }
            };
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                report.spooled_hooks += 1;
                match ingest(&mut st, line) {
                    Ok(Line::Applied) => {}
                    Ok(Line::Duplicate) => report.spool_duplicates += 1,
                    Ok(Line::Stale) => report.spool_stale += 1,
                    Ok(Line::Unparseable) => report.spool_unparseable += 1,
                    Err(e) => st.log_error(None, "spool ingest", &e),
                }
            }
            // Keep the last ingested spool for inspection; drop the claimed file.
            let done = self.paths.spool().with_extension("jsonl.ingested");
            if let Err(e) = std::fs::rename(&file, &done) {
                st.log_error(None, &format!("retiring {}", file.display()), &e);
                let _ = std::fs::remove_file(&file);
            }
        }
        st.publish();
        Ok(report)
    }

    /// Finishes an archive a crash interrupted (git work under the hub lock is fine here:
    /// no client is served before recovery ends).
    fn finish_archive(&self, st: &mut State, task: &Task, report: &mut Recovery) -> Result<()> {
        let path = Path::new(&task.worktree_path);
        // A missing directory only leaves git metadata to prune; that failing (the repo
        // itself moved) does not bring the worktree back, so the archive still completes.
        match self.worktrees.remove(Path::new(&task.repo_path), path) {
            Err(e) if path.exists() => {
                st.store.transition_lifecycle(&task.id, Lifecycle::Active)?;
                st.event(Some(&task.id), "archive_abandoned", json!({ "error": e.to_string() }))?;
                report.archives_abandoned += 1;
            }
            _ => {
                st.store.transition_lifecycle(&task.id, Lifecycle::Archived)?;
                report.archives_finished += 1;
            }
        }
        Ok(())
    }

    /// Checkouts under the worktree root that no live (non-archived) task owns.
    fn report_orphans(&self, st: &mut State, report: &mut Recovery) -> Result<()> {
        let owned: Vec<PathBuf> = st
            .store
            .list_tasks(false)?
            .into_iter()
            .map(|t| std::fs::canonicalize(&t.worktree_path).unwrap_or_else(|_| PathBuf::from(&t.worktree_path)))
            .collect();
        let mut orphans: Vec<String> = self
            .worktrees
            .checkouts()
            .into_iter()
            .filter(|p| !owned.contains(p))
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        orphans.sort();
        for path in &orphans {
            st.event(None, "worktree_orphan", json!({ "path": path }))?;
        }
        report.orphan_worktrees = orphans;
        Ok(())
    }

    /// Renames the live spool to `hook-spool.<boot_id>.jsonl` and returns every claimed
    /// spool file (this boot's and any an earlier daemon claimed but did not finish), oldest
    /// claim first.
    fn claim_spools(&self, st: &mut State) -> Vec<PathBuf> {
        let spool = self.paths.spool();
        let mine = claimed_name(&spool, st.store.boot_id());
        if spool.exists() {
            if let Err(e) = std::fs::rename(&spool, &mine) {
                st.log_error(None, "claiming the hook spool", &e);
            }
        }
        let Some(dir) = spool.parent() else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p != &spool && is_claimed(p))
            .map(|p| (std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), p))
            .collect();
        files.sort();
        files.into_iter().map(|(_, p)| p).collect()
    }
}

fn claimed_name(spool: &Path, boot_id: &str) -> PathBuf {
    spool.with_file_name(format!("hook-spool.{boot_id}.jsonl"))
}

/// `hook-spool.<something>.jsonl` (a claimed spool), not the live spool or a retired one.
fn is_claimed(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("hook-spool."))
        .and_then(|rest| rest.strip_suffix(".jsonl"))
        .is_some_and(|mid| !mid.is_empty())
}

/// Texts queued (`message_queued`) during session `session_id` of `task_id` and never sent,
/// in queue order. The queue lives in memory: a crash loses it, so it is rebuilt from the
/// event log: by message id (`message_sent`, `message_requeued`, and the user's
/// `queue_changed` edits, removals and reorders), or by text for events written before
/// queued messages had ids.
fn unsent_queue(st: &State, task_id: &str, session_id: &str) -> Result<Vec<String>> {
    let events = st.store.task_events(task_id)?;
    let started =
        events.iter().position(|e| e.kind == "session_started" && e.payload["session_id"].as_str() == Some(session_id));
    let Some(start) = started else {
        return Ok(Vec::new());
    };
    let text = |p: &Value| p["text"].as_str().map(str::to_string);
    let id = |p: &Value| p["id"].as_str().map(str::to_string);
    // (id, text); `None` ids come from older daemons.
    let mut queue: Vec<(Option<String>, String)> = Vec::new();
    let at =
        |queue: &[(Option<String>, String)], want: &str| queue.iter().position(|(i, _)| i.as_deref() == Some(want));
    for e in &events[start + 1..] {
        let p = &e.payload;
        match e.kind.as_str() {
            "message_queued" => queue.extend(text(p).map(|t| (id(p), t))),
            "message_sent" => {
                let found = match id(p) {
                    Some(want) => at(&queue, &want),
                    None => text(p).and_then(|t| queue.iter().position(|(i, q)| i.is_none() && *q == t)),
                };
                if let Some(i) = found {
                    queue.remove(i);
                }
            }
            "message_requeued" => {
                if let (Some(want), Some(t)) = (id(p), text(p)) {
                    if at(&queue, &want).is_none() {
                        queue.insert(0, (Some(want), t));
                    }
                }
            }
            "queue_changed" => match p["op"].as_str() {
                Some("edited") => {
                    if let (Some(i), Some(t)) = (id(p).and_then(|w| at(&queue, &w)), text(p)) {
                        queue[i].1 = t;
                    }
                }
                Some("removed") => {
                    if let Some(i) = id(p).and_then(|w| at(&queue, &w)) {
                        queue.remove(i);
                    }
                }
                Some("reordered") => {
                    let ids: Vec<&str> = p["ids"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                    let rank = |q: &(Option<String>, String)| {
                        q.0.as_deref().and_then(|i| ids.iter().position(|x| *x == i)).unwrap_or(usize::MAX)
                    };
                    queue.sort_by_key(rank);
                }
                _ => {}
            },
            _ => {}
        }
    }
    Ok(queue.into_iter().map(|(_, t)| t).collect())
}

/// The task's last session ended in a hibernation (`memory.hibernated` /
/// `memory.auto_hibernated` after its latest `session_launch`): the mark lives in memory,
/// so a restart rebuilds it from the trail and `mirante wake` keeps working.
fn was_hibernated(st: &State, task_id: &str) -> Result<bool> {
    let events = st.store.task_events(task_id)?;
    let last = events
        .iter()
        .rev()
        .find(|e| matches!(e.kind.as_str(), "session_launch" | "memory.hibernated" | "memory.auto_hibernated"));
    Ok(last.is_some_and(|e| e.kind != "session_launch"))
}

fn end_stale_session(st: &mut State, task_id: &str, report: &mut Recovery) -> Result<()> {
    let live = st.store.live_session(task_id)?;
    if let Some(s) = &live {
        let unsent = unsent_queue(st, task_id, &s.id)?;
        st.store.end_session(&s.id)?;
        if !unsent.is_empty() {
            st.event(
                Some(task_id),
                "queue_dropped",
                json!({ "messages": unsent, "session_id": s.id, "cause": "mirante:daemon_restart" }),
            )?;
            report.queued_dropped += 1;
        }
        report.sessions_ended += 1;
        for p in st.store.pending_permissions(task_id)? {
            if p.session_id == s.id {
                st.store.answer_permission(&p.id, PermissionStatus::Denied)?;
                st.event(
                    Some(task_id),
                    "permission_expired",
                    json!({ "permission_id": p.id, "cause": "mirante:daemon_restart" }),
                )?;
                report.permissions_expired += 1;
            }
        }
    }
    let status = st.store.get_task(task_id)?.status;
    if live.is_some() || matches!(status, Status::Working | Status::NeedsYou) {
        st.set_status(task_id, Status::Ended, "mirante:daemon_restart")?;
    }
    Ok(())
}

fn ingest(st: &mut State, line: &str) -> Result<Line> {
    let Ok(env) = serde_json::from_str::<HookEnvelope>(line) else {
        st.event(None, "hook_spool_unparseable", json!({}))?;
        return Ok(Line::Unparseable);
    };
    if let Some(id) = &env.event_id {
        if !st.seen_hooks.insert(id) {
            return Ok(Line::Duplicate);
        }
    }
    let p = env.params;
    let Ok(ev) = HookEvent::parse(&p.payload) else {
        st.event(Some(&p.task), "hook_spool_unparseable", json!({}))?;
        return Ok(Line::Unparseable);
    };
    let conv = st.store.latest_conversation(&p.task)?;
    if conv.as_ref().is_some_and(|c| p.ts_ms < c.created_at) {
        // From an earlier conversation: applying it would overwrite newer state.
        return Ok(Line::Stale);
    }
    st.event(Some(&p.task), "hook_spooled", json!({ "hook": ev.hook_event_name, "ts_ms": p.ts_ms }))?;
    if ev.hook_event_name != "SessionStart" {
        return Ok(Line::Applied);
    }
    if let (Some(native), Some(conv)) = (ev.session_id.as_deref(), conv) {
        if conv.native_session_id.as_deref() != Some(native) {
            st.store.set_native_session_id(&conv.id, native)?;
        }
    }
    Ok(Line::Applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::DriverEvent;
    use crate::hub::testkit::Kit;
    use mirante_core::model::{AgentKind, Lifecycle};
    use mirante_core::store::Store;
    use mirante_core::worktree::WorktreeManager;
    use mirante_ipc::types::{EditQueued, HookEnvelope, HookParams, ReorderQueue, SendMessage, SendMode, StartSession};

    #[test]
    fn restart_ends_live_sessions_expires_permissions_and_keeps_resume_possible() {
        let k = Kit::with_store(|p| Store::open(&p.database()).unwrap());
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "SessionStart", "session_id": "nat-1", "source": "startup"}));
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        k.hook(&t.id, json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash"}));
        let idle = k.create(AgentKind::Codex, "never-started").task;
        let paths = k.hub.paths().clone();
        // Simulate a crash: a second hub over the same database, nothing shut down.
        let (mut hub2, _rx) =
            Hub::new(Store::open(&paths.database()).unwrap(), paths, Box::new(k.factory.clone())).unwrap();
        hub2.set_boot_timeout(std::time::Duration::ZERO);
        let r = hub2.recover().unwrap();
        assert_eq!((r.sessions_ended, r.permissions_expired), (1, 1));
        let v = hub2.get_task(&t.id).unwrap();
        assert_eq!((v.task.status, v.live, v.needs_you), (Status::Ended, false, 0));
        assert_eq!(hub2.get_task(&idle.id).unwrap().task.status, Status::Idle);
        let res = hub2.start_session(StartSession { task: t.id.clone(), prompt: None }, true).unwrap();
        assert!(res.resumed);
        assert_eq!(k.factory.resumes.lock().unwrap().last().cloned().flatten().as_deref(), Some("nat-1"));
    }

    #[test]
    fn spooled_session_start_records_the_native_id_and_the_spool_is_consumed() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.emit(DriverEvent::Exited { code: None });
        let paths = k.hub.paths().clone();
        for payload in [r#"{"hook_event_name":"SessionStart","session_id":"nat-7","source":"startup"}"#, "garbage"] {
            spool_line(&paths, &t.id, payload);
        }
        let r = k.hub.recover().unwrap();
        assert_eq!(r.spooled_hooks, 2);
        let conv = k.hub.lock().store.latest_conversation(&t.id).unwrap().unwrap();
        assert_eq!(conv.native_session_id.as_deref(), Some("nat-7"));
        assert!(!paths.spool().exists());
        assert!(k.kinds(&t.id).contains(&"hook_spooled".to_string()));
        assert_eq!(k.hub.recover().unwrap().spooled_hooks, 0);
    }

    #[test]
    fn spool_ingest_skips_garbage_stale_and_duplicate_lines_and_never_aborts() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.emit(DriverEvent::Exited { code: None });
        let paths = k.hub.paths().clone();
        let created = k.hub.lock().store.latest_conversation(&t.id).unwrap().unwrap().created_at;
        let start =
            |sid: &str| format!(r#"{{"hook_event_name":"SessionStart","session_id":"{sid}","source":"startup"}}"#);
        let now = mirante_core::model::now_ms();
        spool_env(&paths, &t.id, &start("nat-7"), now, Some("e1"));
        spool_env(&paths, &t.id, &start("nat-7"), now, Some("e1")); // duplicate
        spool_env(&paths, &t.id, &start("nat-old"), created - 60_000, Some("e0")); // older than the conversation
        append(&paths.spool(), "garbage");
        append(&paths.spool(), r#"{"task": 5}"#);
        spool_env(&paths, "no-such-task", &start("nat-x"), now, Some("e2"));
        let r = k.hub.recover().unwrap();
        assert_eq!(r.spooled_hooks, 6);
        assert_eq!((r.spool_duplicates, r.spool_stale, r.spool_unparseable), (1, 1, 2));
        let conv = k.hub.lock().store.latest_conversation(&t.id).unwrap().unwrap();
        assert_eq!(conv.native_session_id.as_deref(), Some("nat-7"));
        assert_eq!(k.kinds(&t.id).iter().filter(|e| *e == "hook_spooled").count(), 1);
        assert!(!paths.spool().exists());
        assert!(claimed(&paths).is_empty(), "{:?}", claimed(&paths));
    }

    #[test]
    fn a_spool_claimed_by_an_earlier_boot_is_still_ingested() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.emit(DriverEvent::Exited { code: None });
        let paths = k.hub.paths().clone();
        // A previous daemon claimed the spool, then died before reading it.
        let line = HookEnvelope {
            params: HookParams {
                task: t.id.clone(),
                payload: r#"{"hook_event_name":"SessionStart","session_id":"nat-9","source":"startup"}"#.into(),
                ts_ms: mirante_core::model::now_ms(),
            },
            event_id: Some("e9".into()),
            received_ts_ms: None,
            truncated: None,
            session: None,
        };
        let old = paths.spool().with_file_name("hook-spool.oldboot.jsonl");
        append(&old, &serde_json::to_string(&line).unwrap());
        k.hub.recover().unwrap();
        let conv = k.hub.lock().store.latest_conversation(&t.id).unwrap().unwrap();
        assert_eq!(conv.native_session_id.as_deref(), Some("nat-9"));
        assert!(claimed(&paths).is_empty());
    }

    #[test]
    fn spooled_event_ids_are_remembered_against_live_redelivery() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        let paths = k.hub.paths().clone();
        let ups = r#"{"hook_event_name":"UserPromptSubmit","prompt":"x"}"#;
        spool_env(&paths, &t.id, ups, mirante_core::model::now_ms(), Some("dup-1"));
        k.hub.recover().unwrap();
        // The same event later arrives live (the hook spooled it after an ack timeout and
        // also delivered it): it was already seen.
        let restarted = k.hub.get_task(&t.id).unwrap().task.status;
        assert_eq!(restarted, Status::Ended);
        assert!(k.hub.lock().seen_hooks.contains("dup-1"));
    }

    /// A second hub over the same database, nothing shut down (a crash), recovered.
    fn crash_and_recover(k: &Kit) -> (Hub, Recovery) {
        let paths = k.hub.paths().clone();
        let (mut hub2, _rx) =
            Hub::new(Store::open(&paths.database()).unwrap(), paths, Box::new(k.factory.clone())).unwrap();
        hub2.set_boot_timeout(std::time::Duration::ZERO);
        let r = hub2.recover().unwrap();
        (hub2, r)
    }

    fn file_kit() -> Kit {
        Kit::with_store(|p| Store::open(&p.database()).unwrap())
    }

    fn task_kinds(hub: &Hub, task: &str) -> Vec<mirante_core::model::Event> {
        hub.lock().store.task_events(task).unwrap()
    }

    #[test]
    fn a_hibernated_task_stays_hibernated_across_a_restart_and_wakes() {
        let k = file_kit();
        let t = k.started(AgentKind::Claude, "a").task.id;
        k.emit(DriverEvent::NativeId("nat-9".into()));
        k.hub.hibernate(&t).unwrap();
        let woke = k.started(AgentKind::Codex, "b").task.id;
        k.hub.hibernate(&woke).unwrap();
        k.hub.wake(&woke).unwrap();
        k.hub.shutdown();
        let (hub2, _) = crash_and_recover(&k);
        let v = hub2.get_task(&t).unwrap();
        assert!(v.hibernated && !v.live, "{v:?}");
        assert!(!hub2.get_task(&woke).unwrap().hibernated, "woken, then ended by the restart");
        let r = hub2.wake(&t).unwrap();
        assert!(r.resumed);
        assert_eq!(k.factory.resumes.lock().unwrap().last().cloned().flatten().as_deref(), Some("nat-9"));
        assert!(!hub2.get_task(&t).unwrap().hibernated);
    }

    #[test]
    fn messages_queued_but_never_sent_are_reported_dropped_after_a_crash() {
        let k = file_kit();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "work"}));
        for text in ["first", "second"] {
            let m = SendMessage { task: t.id.clone(), text: text.into(), mode: SendMode::Queue };
            assert_eq!(k.hub.send(m).unwrap().delivered, SendMode::Queue);
        }
        // The turn ends: "first" goes out, "second" waits for the next turn end.
        k.hook(&t.id, json!({"hook_event_name": "Stop"}));
        assert_eq!(k.hub.get_task(&t.id).unwrap().queued, 1);
        let (hub2, r) = crash_and_recover(&k);
        assert_eq!(r.queued_dropped, 1);
        let ev = task_kinds(&hub2, &t.id);
        let dropped: Vec<&mirante_core::model::Event> = ev.iter().filter(|e| e.kind == "queue_dropped").collect();
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].payload["messages"], json!(["second"]));
        assert_eq!(dropped[0].payload["cause"], json!("mirante:daemon_restart"));
        // Nothing is reported twice by a second restart.
        let (_hub3, r3) = crash_and_recover(&k);
        assert_eq!(r3.queued_dropped, 0);
    }

    #[test]
    fn a_crash_reports_the_queue_as_edited_removed_and_reordered() {
        let k = file_kit();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "work"}));
        for text in ["one", "two", "three", "two"] {
            let m = SendMessage { task: t.id.clone(), text: text.into(), mode: SendMode::Queue };
            k.hub.send(m).unwrap();
        }
        let ids: Vec<String> = k.hub.list_queued(&t.id).unwrap().into_iter().map(|q| q.id).collect();
        k.hub.edit_queued(EditQueued { id: ids[0].clone(), text: "one, edited".into() }).unwrap();
        // The first "two" goes; the second (same text) stays.
        k.hub.remove_queued(&ids[1]).unwrap();
        let order = vec![ids[3].clone(), ids[2].clone(), ids[0].clone()];
        k.hub.reorder_queued(ReorderQueue { task_id: t.id.clone(), ids: order }).unwrap();
        // The turn ends: the new front ("two") goes out.
        k.hook(&t.id, json!({"hook_event_name": "Stop"}));
        assert_eq!(k.calls().last().map(String::as_str), Some("send:two"));
        let (hub2, r) = crash_and_recover(&k);
        assert_eq!(r.queued_dropped, 1);
        let ev = task_kinds(&hub2, &t.id);
        let dropped = ev.iter().find(|e| e.kind == "queue_dropped").unwrap();
        assert_eq!(dropped.payload["messages"], json!(["three", "one, edited"]));
    }

    #[test]
    fn an_interrupted_archive_of_a_clean_worktree_is_finished() {
        let k = file_kit();
        let t = k.create(AgentKind::Claude, "a").task;
        // Crash after Archiving was persisted, before the worktree was removed.
        k.hub.lock().store.transition_lifecycle(&t.id, Lifecycle::Archiving).unwrap();
        let (hub2, r) = crash_and_recover(&k);
        assert_eq!((r.archives_finished, r.archives_abandoned), (1, 0));
        assert_eq!(hub2.get_task(&t.id).unwrap().task.lifecycle, Lifecycle::Archived);
        assert!(!std::path::Path::new(&t.worktree_path).exists());
    }

    #[test]
    fn an_interrupted_archive_whose_worktree_is_gone_is_finished() {
        let k = file_kit();
        let t = k.create(AgentKind::Claude, "a").task;
        k.hub.lock().store.transition_lifecycle(&t.id, Lifecycle::Archiving).unwrap();
        std::fs::remove_dir_all(&t.worktree_path).unwrap();
        let (hub2, r) = crash_and_recover(&k);
        assert_eq!(r.archives_finished, 1);
        assert_eq!(hub2.get_task(&t.id).unwrap().task.lifecycle, Lifecycle::Archived);
    }

    #[test]
    fn an_interrupted_archive_of_a_dirty_worktree_goes_back_to_active() {
        let k = file_kit();
        let t = k.create(AgentKind::Claude, "a").task;
        k.hub.lock().store.transition_lifecycle(&t.id, Lifecycle::Archiving).unwrap();
        std::fs::write(std::path::Path::new(&t.worktree_path).join("wip.txt"), "unsaved\n").unwrap();
        let (hub2, r) = crash_and_recover(&k);
        assert_eq!((r.archives_finished, r.archives_abandoned), (0, 1));
        assert_eq!(hub2.get_task(&t.id).unwrap().task.lifecycle, Lifecycle::Active);
        assert!(std::path::Path::new(&t.worktree_path).join("wip.txt").exists());
        assert!(task_kinds(&hub2, &t.id).iter().any(|e| e.kind == "archive_abandoned"));
    }

    #[test]
    fn orphan_managed_worktrees_are_reported_not_deleted() {
        let k = file_kit();
        let kept = k.create(AgentKind::Claude, "kept").task;
        // A worktree made for a task whose row never got written (crash in task.create).
        let orphan = WorktreeManager::new(&k.hub.paths().worktrees()).create(&k.repo, "orphan", None).unwrap();
        let (hub2, r) = crash_and_recover(&k);
        let canon = std::fs::canonicalize(&orphan.path).unwrap().to_string_lossy().into_owned();
        assert_eq!(r.orphan_worktrees, vec![canon.clone()]);
        assert!(orphan.path.exists());
        assert!(std::path::Path::new(&kept.worktree_path).exists());
        let st = hub2.lock();
        let ev = st.store.events_since(0, 10_000).unwrap();
        let e = ev.iter().find(|e| e.kind == "worktree_orphan").expect("reported");
        assert_eq!(e.payload["path"], json!(canon));
    }

    /// Claimed-but-not-ingested spool files left in the home.
    fn claimed(paths: &mirante_ipc::Paths) -> Vec<String> {
        let home = paths.spool().parent().unwrap().to_path_buf();
        std::fs::read_dir(home)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("hook-spool.") && n.ends_with(".jsonl"))
            .collect()
    }

    fn append(path: &std::path::Path, line: &str) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        writeln!(f, "{line}").unwrap();
    }

    fn spool_env(paths: &mirante_ipc::Paths, task: &str, payload: &str, ts_ms: i64, event_id: Option<&str>) {
        let e = HookEnvelope {
            params: HookParams { task: task.into(), payload: payload.into(), ts_ms },
            event_id: event_id.map(String::from),
            received_ts_ms: None,
            truncated: None,
            session: None,
        };
        append(&paths.spool(), &serde_json::to_string(&e).unwrap());
    }

    fn spool_line(paths: &mirante_ipc::Paths, task: &str, payload: &str) {
        let p = HookParams { task: task.into(), payload: payload.into(), ts_ms: mirante_core::model::now_ms() };
        append(&paths.spool(), &serde_json::to_string(&p).unwrap());
    }
}
