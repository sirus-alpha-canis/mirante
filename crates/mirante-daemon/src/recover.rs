//! Daemon start: agent processes do not outlive the daemon (their PTY / pipes close with
//! it), so every session still live in the store is ended, its pending permission requests
//! expire, and the task becomes `Ended`, ready for `mirante resume`. Then hook payloads
//! spooled while the daemon was down are ingested: a `SessionStart` among them still
//! records the native session id, so a session that started while the daemon was down
//! can be resumed.
use mirante_core::hook::HookEvent;
use mirante_core::model::{PermissionStatus, Status};
use mirante_ipc::types::HookParams;
use serde_json::json;

use crate::hub::{Hub, Result};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Recovery {
    pub sessions_ended: usize,
    pub permissions_expired: usize,
    pub spooled_hooks: usize,
}

impl Hub {
    pub fn recover(&self) -> Result<Recovery> {
        let mut report = Recovery::default();
        let mut st = self.lock();
        for task in st.store.list_tasks(false)? {
            let live = st.store.live_session(&task.id)?;
            if let Some(s) = &live {
                st.store.end_session(&s.id)?;
                report.sessions_ended += 1;
                for p in st.store.pending_permissions(&task.id)? {
                    if p.session_id == s.id {
                        st.store.answer_permission(&p.id, PermissionStatus::Denied)?;
                        st.event(
                            Some(&task.id),
                            "permission_expired",
                            json!({ "permission_id": p.id, "cause": "mirante:daemon_restart" }),
                        )?;
                        report.permissions_expired += 1;
                    }
                }
            }
            let status = st.store.get_task(&task.id)?.status;
            if live.is_some() || matches!(status, Status::Working | Status::NeedsYou) {
                st.set_status(&task.id, Status::Ended, "mirante:daemon_restart")?;
            }
        }
        let spool = self.paths.spool();
        if let Ok(text) = std::fs::read_to_string(&spool) {
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                report.spooled_hooks += 1;
                let Ok(p) = serde_json::from_str::<HookParams>(line) else {
                    st.event(None, "hook_spool_unparseable", json!({}))?;
                    continue;
                };
                let Ok(ev) = HookEvent::parse(&p.payload) else {
                    st.event(Some(&p.task), "hook_spool_unparseable", json!({}))?;
                    continue;
                };
                st.event(Some(&p.task), "hook_spooled", json!({ "hook": ev.hook_event_name, "ts_ms": p.ts_ms }))?;
                if ev.hook_event_name != "SessionStart" {
                    continue;
                }
                let (Some(native), Ok(Some(conv))) = (ev.session_id.as_deref(), st.store.latest_conversation(&p.task))
                else {
                    continue;
                };
                if conv.native_session_id.as_deref() != Some(native) {
                    st.store.set_native_session_id(&conv.id, native)?;
                }
            }
            std::fs::rename(&spool, spool.with_extension("jsonl.ingested")).map_err(mirante_core::CoreError::from)?;
        }
        st.publish();
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::DriverEvent;
    use crate::hub::testkit::Kit;
    use mirante_core::model::AgentKind;
    use mirante_core::store::Store;
    use mirante_ipc::types::StartSession;

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

    fn spool_line(paths: &mirante_ipc::Paths, task: &str, payload: &str) {
        use std::io::Write;
        let p = HookParams { task: task.into(), payload: payload.into(), ts_ms: 1 };
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(paths.spool()).unwrap();
        writeln!(f, "{}", serde_json::to_string(&p).unwrap()).unwrap();
    }
}
