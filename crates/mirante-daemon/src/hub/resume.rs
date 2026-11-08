//! Bringing sessions back after the daemon restarts.
//!
//! Agent processes do not outlive the daemon (see `recover`): a `mirante daemon restart`, a
//! reboot or a crash ends every live session. With auto-resume on (the default; the
//! person's `daemon.settings`, persisted in `<home>/settings.json`, or
//! `MIRANTE_AUTO_RESUME=off` turn it off), each session the daemon itself ended is resumed
//! on its native conversation (`claude --resume`, Codex `thread/resume`) once the daemon is
//! up, one at a time, and only while the memory guard is not pausing new sessions (they
//! wait until it eases). Never resumed: a task the person stopped, a hibernated one (it
//! stays hibernated until woken), an archived one, one already live again, and one whose
//! conversation never got a native id (nothing to resume). Each resume is a
//! `session.auto_resumed` event; one that fails is `session.auto_resume_failed`.
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mirante_core::model::{Event, Lifecycle};
use mirante_ipc::types::{DaemonSettings, SettingsParams, StartSession};
use serde_json::json;

use super::{Hub, HubError, Result, State};

/// End causes of a session the daemon ended on its own (shutdown, or a crash found at
/// start): those are the ones brought back.
pub const DAEMON_CAUSES: [&str; 2] = ["mirante:daemon_shutdown", "mirante:daemon_restart"];

/// How often a waiting resume looks at the memory guard again.
pub(crate) const POLL: Duration = Duration::from_millis(200);
/// Longest wait for the memory guard's first sample before resuming anyway.
pub(crate) const FIRST_SAMPLE_WAIT: Duration = Duration::from_secs(10);

/// Did the task's last session end because the daemon went away? The latest `status` move
/// to `ended` says, unless a session was launched after it.
pub fn ended_by_daemon(events: &[Event]) -> bool {
    for e in events.iter().rev() {
        if e.kind == "session_launch" {
            return false;
        }
        if e.kind == "status" && e.payload["to"] == "ended" {
            return e.payload["cause"].as_str().is_some_and(|c| DAEMON_CAUSES.contains(&c));
        }
    }
    false
}

/// `MIRANTE_AUTO_RESUME`: "0", "off", "false" or "no" turn auto-resume off for this daemon.
pub fn env_says_off(v: Option<&str>) -> bool {
    matches!(v.map(|s| s.trim().to_ascii_lowercase()).as_deref(), Some("0" | "off" | "false" | "no"))
}

/// The settings file's `auto_resume` (on when the file is missing or unreadable).
pub fn load_auto_resume(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v["auto_resume"].as_bool())
        .unwrap_or(true)
}

fn save_auto_resume(path: &Path, on: bool) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(&tmp)?;
    f.write_all(serde_json::to_string_pretty(&json!({ "auto_resume": on }))?.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

impl State {
    /// Why `task_id` may not be auto-resumed now (`None`: it may).
    fn resume_refusal(&self, task_id: &str) -> Result<Option<&'static str>> {
        let task = self.store.get_task(task_id)?;
        if !matches!(task.lifecycle, Lifecycle::Active | Lifecycle::Created) {
            return Ok(Some("archived"));
        }
        if self.live.contains_key(task_id) {
            return Ok(Some("already live"));
        }
        if self.mem.hibernated.contains(task_id) {
            return Ok(Some("hibernated"));
        }
        let native = self.store.latest_conversation(task_id)?.and_then(|c| c.native_session_id);
        if native.is_none() {
            return Ok(Some("no native conversation to resume"));
        }
        Ok(None)
    }
}

impl Hub {
    /// Auto-resume is on (settings file and `MIRANTE_AUTO_RESUME`, read at start; then the
    /// person's `daemon.settings`).
    pub fn auto_resume_enabled(&self) -> bool {
        self.auto_resume.load(Ordering::SeqCst)
    }

    /// Sets auto-resume for this daemon only (not persisted): `MIRANTE_AUTO_RESUME=off`.
    pub fn set_auto_resume(&self, on: bool) {
        self.auto_resume.store(on, Ordering::SeqCst);
    }

    /// Reads `<home>/settings.json` (auto-resume stays on when it says nothing).
    pub fn load_settings(&self) {
        self.set_auto_resume(load_auto_resume(&self.paths.settings()));
    }

    /// `daemon.settings`: the settings, after applying the ones given (persisted).
    pub fn settings(&self, p: SettingsParams) -> Result<DaemonSettings> {
        if let Some(on) = p.auto_resume {
            save_auto_resume(&self.paths.settings(), on)
                .map_err(|e| HubError::Conflict(format!("could not save the settings: {e}")))?;
            self.set_auto_resume(on);
            let mut st = self.lock();
            st.event(None, "settings_changed", json!({ "auto_resume": on }))?;
            st.publish();
        }
        Ok(DaemonSettings { auto_resume: self.auto_resume_enabled() })
    }

    /// Resumes `tasks` one at a time (see the module doc). `wait_sample`: the memory guard
    /// runs, so its first sample is awaited (at most [`FIRST_SAMPLE_WAIT`]) before the first
    /// resume. Returns the tasks resumed.
    pub fn run_auto_resume(&self, tasks: &[String], wait_sample: bool) -> Vec<String> {
        let started = Instant::now();
        let mut resumed = Vec::new();
        for task_id in tasks {
            // Memory first: a resume waits while new sessions are paused.
            loop {
                if !self.auto_resume_enabled() {
                    return resumed;
                }
                let st = self.lock();
                let sampled = !wait_sample || st.mem.system.is_some() || started.elapsed() >= FIRST_SAMPLE_WAIT;
                if sampled && !st.mem.paused {
                    break;
                }
                drop(st);
                std::thread::sleep(POLL);
            }
            {
                let mut st = self.lock();
                match st.resume_refusal(task_id) {
                    Ok(None) => {}
                    Ok(Some(why)) => {
                        let _ = st.event(Some(task_id), "session.auto_resume_skipped", json!({ "reason": why }));
                        st.publish();
                        continue;
                    }
                    Err(e) => {
                        st.log_error(Some(task_id), "auto-resume", &e);
                        st.publish();
                        continue;
                    }
                }
            }
            let r = self.start_session(StartSession { task: task_id.clone(), prompt: None }, true);
            let mut st = self.lock();
            match r {
                Ok(res) => {
                    let session = st.live.get(task_id).map(|l| l.session_id.clone());
                    let payload = json!({ "session_id": session, "resumed": res.resumed, "boot": res.boot });
                    let _ = st.event(Some(task_id), "session.auto_resumed", payload);
                    resumed.push(task_id.clone());
                }
                Err(e) => {
                    let _ = st.event(Some(task_id), "session.auto_resume_failed", json!({ "error": e.to_string() }));
                }
            }
            st.publish();
        }
        resumed
    }

    /// [`Hub::run_auto_resume`] on its own thread (the daemon serves meanwhile).
    pub fn spawn_auto_resume(self: &Arc<Self>, tasks: Vec<String>, wait_sample: bool) {
        if tasks.is_empty() || !self.auto_resume_enabled() {
            return;
        }
        let hub = Arc::clone(self);
        std::thread::spawn(move || {
            hub.run_auto_resume(&tasks, wait_sample);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::Kit;
    use super::*;
    use crate::driver::DriverEvent;
    use mirante_core::model::AgentKind;
    use mirante_core::store::Store;

    /// Started with a native conversation (`nat`).
    fn live_with_native(k: &Kit, branch: &str, nat: &str) -> String {
        let t = k.started(AgentKind::Claude, branch).task.id;
        k.emit(DriverEvent::NativeId(nat.into()));
        t
    }

    /// A second hub over the same database, recovered: the daemon after a restart.
    fn restarted(k: &Kit) -> (Hub, crate::recover::Recovery) {
        let paths = k.hub.paths().clone();
        let (mut hub, _rx) =
            Hub::new(Store::open(&paths.database()).unwrap(), paths, Box::new(k.factory.clone())).unwrap();
        hub.set_boot_timeout(Duration::ZERO);
        let r = hub.recover().unwrap();
        (hub, r)
    }

    fn kinds(hub: &Hub, task: &str) -> Vec<String> {
        hub.lock().store.task_events(task).unwrap().into_iter().map(|e| e.kind).collect()
    }

    fn file_kit() -> Kit {
        Kit::with_store(|p| Store::open(&p.database()).unwrap())
    }

    #[test]
    fn a_daemon_shutdown_brings_back_live_sessions_but_not_stopped_or_hibernated_ones() {
        let k = file_kit();
        let live = live_with_native(&k, "live", "nat-live");
        let stopped = live_with_native(&k, "stopped", "nat-stopped");
        k.hub.stop(&stopped).unwrap();
        let asleep = live_with_native(&k, "asleep", "nat-asleep");
        k.hub.hibernate(&asleep).unwrap();
        let fresh = k.started(AgentKind::Claude, "no-native").task.id;
        let never = k.create(AgentKind::Codex, "never-started").task.id;
        k.hub.shutdown();
        let (hub, r) = restarted(&k);
        let mut want = vec![live.clone(), fresh.clone()];
        want.sort();
        let mut got = r.resume.clone();
        got.sort();
        assert_eq!(got, want, "only sessions the daemon ended");
        assert!(!r.resume.contains(&never) && !r.resume.contains(&stopped) && !r.resume.contains(&asleep));
        let before = k.factory.resumes.lock().unwrap().len();
        assert_eq!(hub.run_auto_resume(&r.resume, false), vec![live.clone()]);
        let resumes = k.factory.resumes.lock().unwrap().clone();
        assert_eq!(resumes[before..], [Some("nat-live".to_string())]);
        assert!(hub.get_task(&live).unwrap().live);
        assert!(kinds(&hub, &live).contains(&"session.auto_resumed".to_string()));
        // No native id: nothing to resume, said so.
        assert!(!hub.get_task(&fresh).unwrap().live);
        assert!(kinds(&hub, &fresh).contains(&"session.auto_resume_skipped".to_string()));
        assert!(hub.get_task(&asleep).unwrap().hibernated);
    }

    #[test]
    fn a_crash_brings_back_the_sessions_that_were_live() {
        let k = file_kit();
        let t = live_with_native(&k, "crashed", "nat-1");
        // No shutdown: a second hub finds the session still live in the store.
        let (hub, r) = restarted(&k);
        assert_eq!(r.resume, vec![t.clone()]);
        assert_eq!(hub.run_auto_resume(&r.resume, false), vec![t.clone()]);
        assert_eq!(k.factory.resumes.lock().unwrap().last().cloned().flatten().as_deref(), Some("nat-1"));
    }

    #[test]
    fn resumes_wait_while_memory_pressure_pauses_new_sessions() {
        let k = file_kit();
        let a = live_with_native(&k, "a", "nat-a");
        let b = live_with_native(&k, "b", "nat-b");
        k.hub.shutdown();
        let (hub, r) = restarted(&k);
        let hub = Arc::new(hub);
        hub.lock().mem.paused = true;
        let worker = {
            let (hub, ids) = (Arc::clone(&hub), r.resume.clone());
            std::thread::spawn(move || hub.run_auto_resume(&ids, true))
        };
        std::thread::sleep(POLL * 3);
        assert!(!hub.get_task(&a).unwrap().live && !hub.get_task(&b).unwrap().live, "paused: nothing resumed");
        hub.lock().mem.paused = false;
        let done = worker.join().unwrap();
        assert_eq!(done.len(), 2);
        assert!(hub.get_task(&a).unwrap().live && hub.get_task(&b).unwrap().live);
    }

    #[test]
    fn the_setting_turns_auto_resume_off_and_persists() {
        let k = file_kit();
        let t = live_with_native(&k, "a", "nat-a");
        assert!(k.hub.auto_resume_enabled());
        let s = k.hub.settings(SettingsParams { auto_resume: Some(false) }).unwrap();
        assert!(!s.auto_resume);
        assert!(!load_auto_resume(&k.hub.paths().settings()));
        k.hub.shutdown();
        let (hub, r) = restarted(&k);
        hub.load_settings();
        assert!(!hub.auto_resume_enabled());
        assert_eq!(hub.run_auto_resume(&r.resume, false), Vec::<String>::new());
        assert!(!hub.get_task(&t).unwrap().live);
        assert!(hub.settings(SettingsParams { auto_resume: Some(true) }).unwrap().auto_resume);
        assert!(load_auto_resume(&hub.paths().settings()));
        assert!(hub.settings(SettingsParams::default()).unwrap().auto_resume);
    }

    #[test]
    fn the_env_switch_and_a_missing_file() {
        for off in ["0", "off", "OFF", "false", " no "] {
            assert!(env_says_off(Some(off)), "{off}");
        }
        for on in [None, Some(""), Some("1"), Some("on")] {
            assert!(!env_says_off(on), "{on:?}");
        }
        assert!(load_auto_resume(Path::new("/definitely/not/here.json")));
    }
}
