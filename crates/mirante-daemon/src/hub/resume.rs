//! Bringing sessions back after the daemon restarts.
//!
//! Agent processes do not outlive the daemon (see `recover`): a `mirante daemon restart`, a
//! reboot or a crash ends every live session. With auto-resume on (the default; the
//! person's `daemon.settings`, persisted in `<home>/settings.json`, or
//! `MIRANTE_AUTO_RESUME=off` turn it off), each session the daemon itself ended is resumed
//! on its native conversation (`claude --resume`, Codex `thread/resume`) once the daemon is
//! up, one at a time, [`RESUME_GAP`] apart (the memory guard needs that long to see what one
//! resume costs), and only while the memory guard is not pausing new sessions (they wait
//! until it eases, also when it tightens at the moment of the launch).
//!
//! Only what the immediately previous daemon run ended comes back (its shutdown, or the
//! sessions this run found live after a crash), within [`RESUME_MAX_AGE_MS`]. Never resumed:
//! a task the person stopped, started or stopped again since, a hibernated one (it stays
//! hibernated until woken), an archived one, one already live again, one whose conversation
//! never got a native id (nothing to resume), and one that this daemon had itself resumed
//! after a crash and that crashed with the daemon again (no crash loop). Each resume is a
//! `session.auto_resumed` event; one that fails is `session.auto_resume_failed`; one not
//! brought back is `session.auto_resume_skipped`. Any of the three consumes the candidate: a
//! later start never tries it again.
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mirante_core::model::{Event, Lifecycle};
use mirante_ipc::types::{DaemonSettings, SettingsParams, StartSession};
use serde_json::json;

use super::session::Launch;
use super::{Hub, HubError, Result, State};

/// End causes of a session the daemon ended on its own (shutdown, or a crash found at
/// start): those are the ones brought back.
pub const DAEMON_CAUSES: [&str; 2] = ["mirante:daemon_shutdown", "mirante:daemon_restart"];

/// How often a waiting resume looks at the memory guard again.
pub(crate) const POLL: Duration = Duration::from_millis(200);
/// Longest wait for the memory guard's first sample before resuming anyway.
pub(crate) const FIRST_SAMPLE_WAIT: Duration = Duration::from_secs(10);
/// Between two resumes: the memory guard steps up after `UP_SAMPLES` samples 2 s apart,
/// plus time for the resumed agent to settle.
pub const RESUME_GAP: Duration = Duration::from_secs(8);
/// A session ended longer ago than this is not brought back (12 h).
pub const RESUME_MAX_AGE_MS: i64 = 12 * 3_600_000;
/// Why a launch made by auto-resume waits instead of failing.
pub const MEMORY_TIGHT: &str = "memory is tight";

/// Event kinds that consume an auto-resume candidate.
fn is_marker(kind: &str) -> bool {
    matches!(kind, "session.auto_resumed" | "session.auto_resume_failed" | "session.auto_resume_skipped")
}

/// Where the task's last session ended because the daemon went away: the latest `status`
/// move to `ended` with a daemon cause, unless a session was launched after it or an
/// auto-resume already acted on it (resumed, failed or skipped).
fn daemon_end(events: &[Event]) -> Option<usize> {
    for (i, e) in events.iter().enumerate().rev() {
        if e.kind == "session_launch" || is_marker(&e.kind) {
            return None;
        }
        if e.kind == "status" && e.payload["to"] == "ended" {
            return e.payload["cause"].as_str().is_some_and(|c| DAEMON_CAUSES.contains(&c)).then_some(i);
        }
    }
    None
}

/// Did the task's last session end because the daemon went away (and nothing acted on it
/// since)? See [`daemon_end`].
pub fn ended_by_daemon(events: &[Event]) -> bool {
    daemon_end(events).is_some()
}

/// What a start does with a task (see the module doc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Not a candidate.
    No,
    Resume,
    /// A candidate that is not brought back (said once, with a `session.auto_resume_skipped`).
    Skip(&'static str),
}

/// The verdict for a task with `events`, at the start of the run `current_boot`, whose
/// previous run was `previous_boot`, at `now_ms`.
pub fn verdict(events: &[Event], current_boot: &str, previous_boot: Option<&str>, now_ms: i64) -> Verdict {
    let Some(i) = daemon_end(events) else {
        return Verdict::No;
    };
    let end = &events[i];
    let crash = end.payload["cause"] == "mirante:daemon_restart";
    // A crash is found (and the session ended) by this run's recovery; a shutdown is the
    // previous run's own.
    let by_last_run = if crash { end.boot_id == current_boot } else { previous_boot == Some(end.boot_id.as_str()) };
    if !by_last_run {
        return Verdict::No;
    }
    if now_ms.saturating_sub(end.ts_ms) > RESUME_MAX_AGE_MS {
        return Verdict::Skip("it ended more than 12 h ago");
    }
    let launched_by_auto_resume =
        events[..i].iter().rev().find(|e| e.kind == "session_launch").is_some_and(|e| e.payload["auto_resume"] == true);
    if crash && launched_by_auto_resume {
        return Verdict::Skip("the daemon crashed again after resuming it automatically");
    }
    Verdict::Resume
}

/// `MIRANTE_AUTO_RESUME`: "0", "off", "false" or "no" turn auto-resume off for this daemon.
pub fn env_says_off(v: Option<&str>) -> bool {
    matches!(v.map(|s| s.trim().to_ascii_lowercase()).as_deref(), Some("0" | "off" | "false" | "no"))
}

/// The settings file's `auto_resume` (on when the file is missing or unreadable).
pub fn load_auto_resume(path: &Path) -> bool {
    read_settings(path)["auto_resume"].as_bool().unwrap_or(true)
}

/// The settings file as JSON (an empty object when missing, unreadable or not an object).
fn read_settings(path: &Path) -> serde_json::Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| json!({}))
}

/// Writes `auto_resume` into the settings file, keeping its other keys: through a fresh
/// temporary file (random name, created new, never through a symlink), fsynced, renamed
/// over the file, and the directory fsynced.
fn save_auto_resume(path: &Path, on: bool) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut v = read_settings(path);
    v["auto_resume"] = json!(on);
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let name = format!(".settings.{}.{nonce}.{}.tmp", std::process::id(), N.fetch_add(1, Ordering::SeqCst));
    let tmp = path.with_file_name(name);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    let written = f
        .write_all(serde_json::to_string_pretty(&v)?.as_bytes())
        .and_then(|()| f.sync_all())
        .and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Some(dir) = path.parent() {
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

impl State {
    /// Why `task_id` may not be auto-resumed now (`None`: it may). Checked again under the
    /// same lock as the launch (see [`Hub::launch`]).
    pub(crate) fn resume_refusal(&self, task_id: &str) -> Result<Option<&'static str>> {
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
        if !ended_by_daemon(&self.store.task_events(task_id)?) {
            return Ok(Some("the person started or stopped it since the restart"));
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
    /// person's `daemon.settings`, unless the environment forced it off).
    pub fn auto_resume_enabled(&self) -> bool {
        self.auto_resume.load(Ordering::SeqCst) && !self.auto_resume_forced_off()
    }

    /// Sets auto-resume for this daemon only (not persisted).
    pub fn set_auto_resume(&self, on: bool) {
        self.auto_resume.store(on, Ordering::SeqCst);
    }

    /// `MIRANTE_AUTO_RESUME=off`: off for this daemon whatever the person's setting says
    /// (`daemon.settings` still saves their choice for later runs, and says it is forced).
    pub fn force_auto_resume_off(&self) {
        self.auto_resume_env_off.store(true, Ordering::SeqCst);
    }

    pub fn auto_resume_forced_off(&self) -> bool {
        self.auto_resume_env_off.load(Ordering::SeqCst)
    }

    /// The pause between two resumes (tests shorten it).
    pub fn set_resume_gap(&mut self, gap: Duration) {
        self.resume_gap = gap;
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
        Ok(DaemonSettings { auto_resume: self.auto_resume_enabled(), forced_by_env: self.auto_resume_forced_off() })
    }

    /// Waits until a resume may launch: auto-resume still on, the memory guard sampled
    /// (when it runs) and not pausing new sessions. `false`: auto-resume was turned off.
    fn wait_for_room(&self, started: Instant, wait_sample: bool) -> bool {
        loop {
            if !self.auto_resume_enabled() {
                return false;
            }
            let st = self.lock();
            let sampled = !wait_sample || st.mem.system.is_some() || started.elapsed() >= FIRST_SAMPLE_WAIT;
            if sampled && !st.mem.paused {
                return true;
            }
            drop(st);
            std::thread::sleep(POLL);
        }
    }

    /// Resumes `tasks` one at a time (see the module doc). `wait_sample`: the memory guard
    /// runs, so its first sample is awaited (at most [`FIRST_SAMPLE_WAIT`]) before the first
    /// resume. Returns the tasks resumed.
    pub fn run_auto_resume(&self, tasks: &[String], wait_sample: bool) -> Vec<String> {
        let started = Instant::now();
        let mut resumed: Vec<String> = Vec::new();
        for task_id in tasks {
            if !resumed.is_empty() && !self.resume_gap.is_zero() {
                std::thread::sleep(self.resume_gap);
            }
            loop {
                if !self.wait_for_room(started, wait_sample) {
                    return resumed;
                }
                let r = self.launch(StartSession { task: task_id.clone(), prompt: None }, true, true);
                let mut st = self.lock();
                let (kind, payload) = match r {
                    // Memory tightened between the wait and the launch: wait again.
                    Ok(Launch::Refused(MEMORY_TIGHT)) => continue,
                    Ok(Launch::Refused(why)) => ("session.auto_resume_skipped", json!({ "reason": why })),
                    // Someone else got there first (the person resumed it meanwhile).
                    Err(HubError::Conflict(why)) => ("session.auto_resume_skipped", json!({ "reason": why })),
                    Ok(Launch::Started(res)) => {
                        let session = st.live.get(task_id).map(|l| l.session_id.clone());
                        resumed.push(task_id.clone());
                        (
                            "session.auto_resumed",
                            json!({ "session_id": session, "resumed": res.resumed, "boot": res.boot }),
                        )
                    }
                    Err(e) => ("session.auto_resume_failed", json!({ "error": e.to_string() })),
                };
                if let Err(e) = st.event(Some(task_id), kind, payload) {
                    st.log_error(Some(task_id), "auto-resume", &e);
                }
                st.publish();
                break;
            }
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
        restarted_with(k, true)
    }

    fn restarted_with(k: &Kit, auto_resume: bool) -> (Hub, crate::recover::Recovery) {
        let paths = k.hub.paths().clone();
        let (mut hub, _rx) =
            Hub::new(Store::open(&paths.database()).unwrap(), paths, Box::new(k.factory.clone())).unwrap();
        hub.set_boot_timeout(Duration::ZERO);
        hub.set_resume_gap(Duration::ZERO);
        hub.set_auto_resume(auto_resume);
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

    fn kinds_with(hub: &Hub, task: &str, kind: &str) -> Vec<serde_json::Value> {
        let events = hub.lock().store.task_events(task).unwrap();
        events.into_iter().filter(|e| e.kind == kind).map(|e| e.payload).collect()
    }

    /// Review 1: the person started and stopped the task while it waited in the auto-resume
    /// queue; the daemon must not bring it back on its own.
    #[test]
    fn a_task_the_person_started_and_stopped_meanwhile_is_not_auto_resumed() {
        let k = file_kit();
        let t = live_with_native(&k, "a", "nat-a");
        k.hub.shutdown();
        let (hub, r) = restarted(&k);
        assert_eq!(r.resume, vec![t.clone()]);
        hub.start_session(StartSession { task: t.clone(), prompt: None }, true).unwrap();
        hub.stop(&t).unwrap();
        assert_eq!(hub.run_auto_resume(&r.resume, false), Vec::<String>::new());
        assert!(!hub.get_task(&t).unwrap().live);
        let skipped = kinds_with(&hub, &t, "session.auto_resume_skipped");
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        assert!(skipped[0]["reason"].as_str().unwrap().contains("person"), "{skipped:?}");
    }

    /// The person resumed it while auto-resume was on its way: a skip, never a failure.
    #[test]
    fn a_person_resume_during_auto_resume_is_a_skip() {
        let k = file_kit();
        let t = live_with_native(&k, "a", "nat-a");
        k.hub.shutdown();
        let (hub, r) = restarted(&k);
        hub.start_session(StartSession { task: t.clone(), prompt: None }, true).unwrap();
        assert!(hub.run_auto_resume(&r.resume, false).is_empty());
        assert!(kinds_with(&hub, &t, "session.auto_resume_failed").is_empty());
        assert_eq!(kinds_with(&hub, &t, "session.auto_resume_skipped").len(), 1);
    }

    /// Review 3: memory tight at the moment of the launch is "wait", not a failure.
    #[test]
    fn an_auto_launch_while_memory_is_tight_is_refused_for_a_retry() {
        let k = file_kit();
        let t = live_with_native(&k, "a", "nat-a");
        k.hub.shutdown();
        let (hub, _r) = restarted(&k);
        hub.lock().mem.paused = true;
        let got = hub.launch(StartSession { task: t.clone(), prompt: None }, true, true).unwrap();
        assert!(matches!(got, super::super::session::Launch::Refused(MEMORY_TIGHT)), "{got:?}");
        assert!(!hub.get_task(&t).unwrap().live);
        hub.lock().mem.paused = false;
        let got = hub.launch(StartSession { task: t.clone(), prompt: None }, true, true).unwrap();
        assert!(matches!(got, super::super::session::Launch::Started(_)), "{got:?}");
        let launches = kinds_with(&hub, &t, "session_launch");
        assert_eq!(launches.last().unwrap()["auto_resume"], true, "{launches:?}");
    }

    /// Review 3: the memory guard needs `UP_SAMPLES` samples to react to a resume.
    #[test]
    fn resumes_are_spaced_for_the_memory_guard() {
        assert!(
            RESUME_GAP
                >= crate::memguard::policy::Config::for_ram(1 << 34).interval * crate::memguard::policy::UP_SAMPLES
        );
        let k = file_kit();
        let a = live_with_native(&k, "a", "nat-a");
        let b = live_with_native(&k, "b", "nat-b");
        k.hub.shutdown();
        let (mut hub, r) = restarted(&k);
        hub.set_resume_gap(Duration::from_millis(300));
        let t0 = Instant::now();
        assert_eq!(hub.run_auto_resume(&r.resume, false).len(), 2);
        assert!(t0.elapsed() >= Duration::from_millis(300), "{:?}", t0.elapsed());
        assert!(hub.get_task(&a).unwrap().live && hub.get_task(&b).unwrap().live);
    }

    /// Review 2: only what the immediately previous daemon run ended comes back.
    #[test]
    fn a_session_ended_by_an_earlier_run_is_not_resumed() {
        let k = file_kit();
        let t = live_with_native(&k, "a", "nat-a");
        k.hub.shutdown();
        // A run that brought nothing back (it did other work), then another restart.
        let (hub2, r2) = restarted(&k);
        assert_eq!(r2.resume, vec![t.clone()]);
        hub2.lock().event(None, "settings_changed", json!({ "auto_resume": true })).unwrap();
        drop(hub2);
        let (_hub3, r3) = restarted(&k);
        assert!(r3.resume.is_empty(), "{:?}", r3.resume);
    }

    /// Review 2: with auto-resume off, the candidates are marked (once), so a later run
    /// with it on does not bring back stale sessions.
    #[test]
    fn auto_resume_off_marks_the_candidates_consumed() {
        let k = file_kit();
        let t = live_with_native(&k, "a", "nat-a");
        k.hub.shutdown();
        let (hub2, r2) = restarted_with(&k, false);
        assert!(r2.resume.is_empty());
        assert_eq!(kinds_with(&hub2, &t, "session.auto_resume_skipped").len(), 1);
        let (_hub3, r3) = restarted(&k);
        assert!(r3.resume.is_empty());
        assert_eq!(kinds_with(&hub2, &t, "session.auto_resume_skipped").len(), 1, "said once");
    }

    /// Review 2: a session auto-resumed after a crash that crashes the daemon again is not
    /// resumed a second time (no crash loop).
    #[test]
    fn a_crash_after_an_automatic_resume_is_not_resumed_again() {
        let k = file_kit();
        let t = live_with_native(&k, "crashy", "nat-1");
        let (hub2, r2) = restarted(&k);
        assert_eq!(hub2.run_auto_resume(&r2.resume, false), vec![t.clone()]);
        // Crash again (hub2 never shuts down).
        let (hub3, r3) = restarted(&k);
        assert!(r3.resume.is_empty(), "{:?}", r3.resume);
        let skipped = kinds_with(&hub3, &t, "session.auto_resume_skipped");
        assert!(skipped.last().unwrap()["reason"].as_str().unwrap().contains("crash"), "{skipped:?}");
        // A third start does not say it again.
        let (hub4, r4) = restarted(&k);
        assert!(r4.resume.is_empty());
        assert_eq!(kinds_with(&hub4, &t, "session.auto_resume_skipped").len(), skipped.len());
    }

    fn ev(kind: &str, payload: serde_json::Value, boot: &str, ts_ms: i64) -> Event {
        Event { seq: 0, boot_id: boot.into(), task_id: Some("t".into()), kind: kind.into(), payload, ts_ms }
    }

    fn ended(cause: &str, boot: &str, ts: i64) -> Event {
        ev("status", json!({ "from": "idle", "to": "ended", "cause": cause }), boot, ts)
    }

    #[test]
    fn the_verdict_reads_boots_age_markers_and_crash_loops() {
        let now = 100 * 3_600_000;
        let launch = |auto: bool, boot: &str| ev("session_launch", json!({ "auto_resume": auto }), boot, now - 10);
        let shut = ended("mirante:daemon_shutdown", "prev", now - 1000);
        assert_eq!(verdict(&[launch(false, "prev"), shut.clone()], "cur", Some("prev"), now), Verdict::Resume);
        // Ended by an older run.
        assert_eq!(verdict(&[launch(false, "old"), shut.clone()], "cur", Some("other"), now), Verdict::No);
        // A crash found by this run.
        let crash = ended("mirante:daemon_restart", "cur", now - 5);
        assert_eq!(verdict(&[launch(false, "prev"), crash.clone()], "cur", Some("prev"), now), Verdict::Resume);
        // ... found by an earlier run.
        let old_crash = ended("mirante:daemon_restart", "prev", now - 5);
        assert_eq!(verdict(&[old_crash], "cur", Some("prev"), now), Verdict::No);
        // Too old.
        let stale = ended("mirante:daemon_shutdown", "prev", now - RESUME_MAX_AGE_MS - 1);
        assert!(matches!(verdict(&[stale], "cur", Some("prev"), now), Verdict::Skip(_)));
        // Consumed: a marker after the end.
        for m in ["session.auto_resume_failed", "session.auto_resume_skipped"] {
            let evs = [shut.clone(), ev(m, json!({}), "prev", now - 900)];
            assert_eq!(verdict(&evs, "cur", Some("prev"), now), Verdict::No, "{m}");
            assert!(!ended_by_daemon(&evs), "{m}");
        }
        // The session that crashed had been auto-resumed: a skip, not a loop.
        let looped = [launch(true, "prev"), crash.clone()];
        assert!(matches!(verdict(&looped, "cur", Some("prev"), now), Verdict::Skip(_)));
        // Auto-resumed, then a clean shutdown: fine.
        assert_eq!(verdict(&[launch(true, "prev"), shut], "cur", Some("prev"), now), Verdict::Resume);
        // The person stopped it: not the daemon's.
        assert_eq!(verdict(&[ended("mirante:stopped", "prev", now)], "cur", Some("prev"), now), Verdict::No);
    }

    #[test]
    fn saving_keeps_other_keys_and_never_follows_a_planted_tmp() {
        let dir = mirante_ipc::test_dir::TestDir::new();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"auto_resume": true, "future": {"x": 1}}"#).unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep").unwrap();
        std::os::unix::fs::symlink(&victim, path.with_extension("json.tmp")).unwrap();
        save_auto_resume(&path, false).unwrap();
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v, json!({"auto_resume": false, "future": {"x": 1}}));
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert!(!load_auto_resume(&path));
    }

    #[test]
    fn the_env_switch_wins_and_says_so() {
        let k = file_kit();
        k.hub.force_auto_resume_off();
        assert!(!k.hub.auto_resume_enabled());
        let s = k.hub.settings(SettingsParams { auto_resume: Some(true) }).unwrap();
        assert_eq!((s.auto_resume, s.forced_by_env), (false, true));
        assert!(load_auto_resume(&k.hub.paths().settings()), "the person's choice is kept for later runs");
        assert!(!k.hub.auto_resume_enabled());
        assert_eq!(k.hub.status().auto_resume_forced_off, Some(true));
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
