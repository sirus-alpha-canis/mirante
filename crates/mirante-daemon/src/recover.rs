//! Daemon start: agent processes do not outlive the daemon (their PTY / pipes close with
//! it), so every session still live in the store is ended, its pending permission requests
//! expire, and the task becomes `Ended`, ready for `mirante resume`. Then hook payloads
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
use mirante_core::model::{PermissionStatus, Status};
use mirante_ipc::types::HookEnvelope;
use serde_json::json;

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
        }
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

fn end_stale_session(st: &mut State, task_id: &str, report: &mut Recovery) -> Result<()> {
    let live = st.store.live_session(task_id)?;
    if let Some(s) = &live {
        st.store.end_session(&s.id)?;
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
    use mirante_core::model::AgentKind;
    use mirante_core::store::Store;
    use mirante_ipc::types::{HookEnvelope, HookParams, StartSession};

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
