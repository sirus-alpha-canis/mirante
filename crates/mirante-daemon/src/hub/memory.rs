//! The memory guard in the daemon: a thread samples every live session's process tree and
//! the machine (`crate::memguard`), feeds the policy, and carries out what it decides.
//! Nothing is limited up front (macOS cannot enforce RLIMIT_AS/RLIMIT_DATA): Mirante
//! measures and acts, in this order — warn; pause new sessions and queued deliveries; offer
//! to hibernate the largest idle session (the user confirms with `mirante hibernate`);
//! only under sustained critical pressure hibernate an idle session on its own, one per
//! cooldown, never one with a pending permission and never one that is working. Every
//! step is an `memory.*` event.
//!
//! Hibernate = remember the native id and transcript, end the process tree, mark the task
//! hibernated; `mirante wake` resumes it natively (`--resume` / `thread/resume`).
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use mirante_core::model::Status;
use mirante_ipc::types::{
    BootState, DoctorReport, KillOrphan, MemView, OrphanView, StartResult, StartSession, SystemView, TaskView,
};
use serde_json::json;

use super::{Hub, HubError, Result, State};
use crate::driver::proctree::Kill;
use crate::memguard::{self, Action, Config, MemSampler, Policy, SessionInfo, SessionMem, SystemSample, TreeSample};

const GIB: u64 = 1 << 30;

/// How the hub shows memory between samples.
#[derive(Debug, Default)]
pub(crate) struct MemShared {
    pub(crate) system: Option<SystemView>,
    /// New sessions and queued deliveries wait (the policy paused them).
    pub(crate) paused: bool,
    pub(crate) sessions: std::collections::HashMap<String, SessionMem>,
    /// Tasks hibernated by this daemon (a restart ends every session anyway).
    pub(crate) hibernated: std::collections::HashSet<String>,
}

pub(crate) fn mem_view(m: &SessionMem) -> MemView {
    MemView { bytes: m.bytes, peak: m.peak, budget: m.budget, state: m.state.as_str().to_string() }
}

/// The guard's configuration for a machine with `total` bytes of RAM, with the user's
/// overrides: `MIRANTE_MEM_BUDGET_MB` (per session) and `MIRANTE_IDLE_HIBERNATE_MIN`
/// (hibernate a session idle that long; off unless set). Unparseable values are ignored.
pub fn config_from(total: u64, budget_mb: Option<&str>, idle_min: Option<&str>) -> Config {
    let mut c = Config::for_ram(total);
    if let Some(mb) = budget_mb.and_then(|s| s.trim().parse::<u64>().ok()).filter(|mb| *mb > 0) {
        c.budget = mb << 20;
    }
    if let Some(min) = idle_min.and_then(|s| s.trim().parse::<u64>().ok()).filter(|m| *m > 0) {
        c.idle_hibernate = Some(std::time::Duration::from_secs(min * 60));
    }
    c
}

fn system_view(level: &str, s: &SystemSample, paused: bool) -> SystemView {
    SystemView {
        level: level.to_string(),
        total: s.total,
        free: s.free,
        swap_used: s.swap_used,
        swap_total: s.swap_total,
        paused,
    }
}

fn orphan_view(o: &memguard::Orphan) -> OrphanView {
    OrphanView {
        pid: o.proc.pid,
        start: o.proc.start,
        cwd: o.cwd.as_ref().map(|p| p.to_string_lossy().into_owned()),
        argv: o.argv.clone(),
    }
}

impl State {
    /// Everything the policy needs to know about the live sessions now.
    fn mem_sessions(&self) -> Vec<SessionInfo> {
        let mut out = Vec::new();
        for (task_id, l) in &self.live {
            let status = self.store.get_task(task_id).map(|t| t.status).unwrap_or(Status::Working);
            let pending = l.hold.load(std::sync::atomic::Ordering::SeqCst);
            out.push(SessionInfo {
                task_id: task_id.clone(),
                root: l.driver.as_ref().and_then(|d| d.root()),
                idle: status == Status::Idle && l.boot == BootState::Ready && !l.delivering(),
                pending_permission: pending,
                idle_since: l.idle_since,
            });
        }
        out
    }

    /// Carries out one policy decision that needs no process to end; hibernations are
    /// returned for the caller to do without the lock.
    fn apply_mem_action(&mut self, a: Action, hibernate: &mut Vec<(String, String)>) -> Result<()> {
        match a {
            Action::Warn { task_id, bytes, budget, state } => self.event(
                Some(&task_id),
                "memory.warn",
                json!({ "bytes": bytes, "budget": budget, "state": state.as_str() }),
            ),
            Action::Pause(paused) => {
                self.mem.paused = paused;
                if let Some(v) = self.mem.system.as_mut() {
                    v.paused = paused;
                }
                let kind = if paused { "memory.paused" } else { "memory.resumed" };
                self.event(None, kind, json!({ "what": "new sessions and queued deliveries" }))?;
                if !paused {
                    let tasks: Vec<String> = self.live.keys().cloned().collect();
                    for t in tasks {
                        self.kick(&t)?;
                    }
                }
                Ok(())
            }
            Action::OfferHibernate { task_id, reason } => self.event(
                Some(&task_id),
                "memory.offer_hibernate",
                json!({ "reason": reason, "hint": "mirante hibernate <task> (mirante wake <task> resumes it)" }),
            ),
            Action::AutoHibernate { task_id, reason } => {
                hibernate.push((task_id, reason));
                Ok(())
            }
            Action::IdleHibernate { task_id } => {
                hibernate.push((task_id, "idle hibernation".into()));
                Ok(())
            }
            Action::Leak { task_id, mb_per_min } => {
                self.event(Some(&task_id), "memory.leak", json!({ "mb_per_min": mb_per_min }))
            }
            Action::LevelChanged { from, to } => {
                self.event(None, "memory.level", json!({ "from": from.as_str(), "to": to.as_str() }))
            }
            Action::Refused { task_id, reason } => {
                self.event(Some(&task_id), "memory.refused", json!({ "reason": reason }))
            }
        }
    }

    /// Checkouts under the worktree root that no non-archived task owns.
    fn orphan_checkouts(&self, all: Vec<PathBuf>) -> Result<Vec<String>> {
        let owned: Vec<PathBuf> = self
            .store
            .list_tasks(false)?
            .into_iter()
            .map(|t| std::fs::canonicalize(&t.worktree_path).unwrap_or_else(|_| PathBuf::from(&t.worktree_path)))
            .collect();
        let mut out: Vec<String> =
            all.into_iter().filter(|p| !owned.contains(p)).map(|p| p.to_string_lossy().into_owned()).collect();
        out.sort();
        Ok(out)
    }
}

impl Hub {
    /// One round of the guard: sample (without the hub lock), decide, act.
    pub fn memory_tick(&self, policy: &mut Policy, sampler: &dyn MemSampler, now: Instant) {
        let infos = self.lock().mem_sessions();
        let system = sampler.system();
        let sessions: Vec<(SessionInfo, Option<TreeSample>)> = infos
            .into_iter()
            .map(|i| {
                let s = i.root.and_then(|r| sampler.tree(r));
                (i, s)
            })
            .collect();
        let actions = policy.tick(now, &system, &sessions);
        let mut hibernate = Vec::new();
        {
            let mut st = self.lock();
            let live: Vec<String> = st.live.keys().cloned().collect();
            st.mem.sessions.retain(|t, _| live.contains(t));
            for (info, _) in &sessions {
                if let Some(m) = policy.session(&info.task_id) {
                    st.mem.sessions.insert(info.task_id.clone(), m);
                }
            }
            let paused = st.mem.paused;
            st.mem.system = Some(system_view(policy.level().as_str(), &system, paused));
            for a in actions {
                if let Err(e) = st.apply_mem_action(a, &mut hibernate) {
                    st.log_error(None, "memory guard", &e);
                }
            }
            st.publish();
        }
        for (task_id, reason) in hibernate {
            policy.forget(&task_id);
            if let Err(e) = self.hibernate_with(&task_id, Some(&reason)) {
                let mut st = self.lock();
                let _ = st.event(Some(&task_id), "memory.refused", json!({ "reason": e.to_string() }));
                st.publish();
            }
        }
    }

    /// The guard thread: [`Hub::memory_tick`] every 2 s (every 500 ms under pressure).
    pub fn spawn_memguard(self: &Arc<Self>, sampler: Box<dyn MemSampler>, cfg: Config) -> std::thread::JoinHandle<()> {
        let hub = Arc::clone(self);
        std::thread::spawn(move || {
            let mut policy = Policy::new(cfg);
            loop {
                hub.memory_tick(&mut policy, sampler.as_ref(), Instant::now());
                std::thread::sleep(policy.next_interval());
            }
        })
    }

    /// The user's `mirante hibernate`: ends the live session to free memory, keeping it
    /// resumable. Refused while a permission request waits (it would be lost).
    pub fn hibernate(&self, reference: &str) -> Result<TaskView> {
        self.hibernate_with(reference, None)
    }

    /// `auto` is the guard's reason: then the session must also be idle right now.
    fn hibernate_with(&self, reference: &str, auto: Option<&str>) -> Result<TaskView> {
        let (task_id, session_id, driver) = {
            let mut st = self.lock();
            let task = st.resolve(reference)?;
            let infos = st.mem_sessions();
            let l = st
                .live
                .get(&task.id)
                .ok_or_else(|| HubError::Conflict(format!("task {} has no live session", task.branch)))?;
            let info = infos.iter().find(|i| i.task_id == task.id);
            if info.is_some_and(|i| i.pending_permission) {
                return Err(HubError::Conflict(format!(
                    "task {} waits on a permission request: answer it first (a session waiting on you is never hibernated)",
                    task.branch
                )));
            }
            if auto.is_some() && !info.is_some_and(|i| i.idle) {
                return Err(HubError::Conflict(format!("task {} is busy: not hibernated", task.branch)));
            }
            let bytes = st.mem.sessions.get(&task.id).map(|m| m.bytes);
            let payload = json!({
                "session_id": l.session_id,
                "native_id": l.native_id,
                "transcript_path": l.transcript_path,
                "bytes": bytes,
                "auto": auto.is_some(),
                "reason": auto.unwrap_or("asked by the user"),
            });
            let out = (task.id.clone(), l.session_id.clone(), l.driver.clone());
            let kind = if auto.is_some() { "memory.auto_hibernated" } else { "memory.hibernated" };
            st.event(Some(&task.id), kind, payload)?;
            st.publish();
            out
        };
        if let Some(d) = driver {
            d.stop();
        }
        let mut st = self.lock();
        st.finish_session(&task_id, &session_id, "mirante:hibernated")?;
        st.mem.hibernated.insert(task_id.clone());
        st.mem.sessions.remove(&task_id);
        st.publish();
        let t = st.store.get_task(&task_id)?;
        st.view(t)
    }

    /// `mirante wake`: resumes a hibernated session on its native conversation.
    pub fn wake(&self, reference: &str) -> Result<StartResult> {
        let task_id = {
            let st = self.lock();
            let task = st.resolve(reference)?;
            if !st.mem.hibernated.contains(&task.id) {
                return Err(HubError::Conflict(format!(
                    "task {} is not hibernated (mirante resume {} restarts an ended session)",
                    task.branch, task.branch
                )));
            }
            task.id
        };
        self.start_session(StartSession { task: task_id, prompt: None }, true)
    }

    /// Orphan processes (parent gone, inside Mirante's worktrees), orphan checkouts, and
    /// memory. Nothing is killed or deleted here.
    pub fn doctor(&self) -> Result<DoctorReport> {
        let processes = memguard::find_orphans(&self.paths.worktrees());
        let checkouts = self.worktrees.checkouts();
        let st = self.lock();
        Ok(DoctorReport {
            orphan_processes: processes.iter().map(orphan_view).collect(),
            orphan_worktrees: st.orphan_checkouts(checkouts)?,
            memory: st.mem.system.clone(),
        })
    }

    /// Kills one orphan the user picked: it must still be an orphan of Mirante's worktrees
    /// with the same pid AND start time (never a reused pid, never by name).
    pub fn kill_orphan(&self, k: KillOrphan) -> Result<OrphanView> {
        let found = memguard::find_orphans(&self.paths.worktrees())
            .into_iter()
            .find(|o| o.proc.pid == k.pid && o.proc.start == k.start)
            .ok_or_else(|| {
                HubError::NotFound(format!("pid {} is not (or no longer) an orphan in Mirante's worktrees", k.pid))
            })?;
        let report = memguard::kill_orphan(&found, &Kill);
        let mut st = self.lock();
        let v = orphan_view(&found);
        st.event(None, "orphan_killed", json!({ "pid": v.pid, "argv": v.argv, "survivors": report.survivors.len() }))?;
        st.publish();
        if report.survivors.is_empty() {
            Ok(v)
        } else {
            Err(HubError::Conflict(format!("pid {} survived SIGKILL", k.pid)))
        }
    }
}

/// Budget shown for `n` bytes: "1.2G", "512M".
pub fn human(n: u64) -> String {
    if n >= GIB {
        format!("{:.1}G", n as f64 / GIB as f64)
    } else {
        format!("{}M", n >> 20)
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::Kit;
    use super::*;
    use crate::driver::proctree::{proc_of, Proc};
    use crate::driver::DriverEvent;
    use crate::memguard::{FakeSampler, Pressure};
    use mirante_core::model::AgentKind;
    use std::time::Duration;

    const MB: u64 = 1 << 20;

    fn sys(pressure: Pressure) -> SystemSample {
        SystemSample {
            total: 16 * GIB,
            free: 4 * GIB,
            swap_used: 0,
            swap_total: 4 * GIB,
            pressure,
            kernel_signal: true,
        }
    }

    fn cfg() -> Config {
        let mut c = Config::for_ram(16 * GIB);
        c.budget = 1000 * MB;
        c
    }

    fn kinds(k: &Kit, task: Option<&str>) -> Vec<String> {
        let st = k.hub.lock();
        st.store
            .events_since(0, 10_000)
            .unwrap()
            .into_iter()
            .filter(|e| task.is_none() || e.task_id.as_deref() == task)
            .map(|e| e.kind)
            .collect()
    }

    /// A started session whose fake driver reports `pid` as its root.
    fn with_root(k: &Kit, branch: &str, pid: i32) -> String {
        *k.factory.driver.root.lock().unwrap() = Some(Proc { pid, start: 1 });
        k.started(AgentKind::Claude, branch).task.id
    }

    #[test]
    fn samples_show_in_the_view_and_soft_and_hard_warn_once() {
        let k = Kit::new();
        let t = with_root(&k, "a", 101);
        let s = FakeSampler::default();
        s.set_system(sys(Pressure::Normal));
        let mut p = Policy::new(cfg());
        let now = Instant::now();
        s.set_tree(101, 500 * MB);
        k.hub.memory_tick(&mut p, &s, now);
        let v = k.hub.get_task(&t).unwrap();
        assert_eq!(v.memory, Some(MemView { bytes: 500 * MB, peak: 500 * MB, budget: 1000 * MB, state: "ok".into() }));
        s.set_tree(101, 900 * MB);
        k.hub.memory_tick(&mut p, &s, now + Duration::from_secs(2));
        s.set_tree(101, 950 * MB);
        k.hub.memory_tick(&mut p, &s, now + Duration::from_secs(4));
        assert_eq!(kinds(&k, Some(&t)).iter().filter(|k| *k == "memory.warn").count(), 1);
        assert_eq!(k.hub.get_task(&t).unwrap().memory.unwrap().state, "soft");
        assert_eq!(k.hub.status().memory.unwrap().level, "normal");
    }

    #[test]
    fn pressure_pauses_queued_deliveries_and_new_sessions_until_it_eases() {
        let k = Kit::new();
        let t = with_root(&k, "a", 101);
        let s = FakeSampler::default();
        s.set_tree(101, 100 * MB);
        let mut p = Policy::new(cfg());
        let now = Instant::now();
        s.set_system(sys(Pressure::Warn));
        k.hub.memory_tick(&mut p, &s, now);
        assert!(k.hub.status().memory.unwrap().paused);
        // Queued while paused: held even though the agent is idle.
        let m = mirante_ipc::types::SendMessage { task: t.clone(), text: "next".into(), mode: SendMode::Queue };
        assert_eq!(k.hub.send(m).unwrap().delivered, SendMode::Queue);
        assert!(k.calls().iter().all(|c| !c.starts_with("send:")));
        let other = k.create(AgentKind::Claude, "b").task.id;
        let r = k.hub.start_session(StartSession { task: other, prompt: None }, false);
        assert!(matches!(r, Err(HubError::Conflict(ref m)) if m.contains("memory")), "{r:?}");
        assert!(kinds(&k, None).contains(&"memory.offer_hibernate".to_string()));
        s.set_system(sys(Pressure::Normal));
        for i in 1..=5 {
            k.hub.memory_tick(&mut p, &s, now + Duration::from_secs(2 * i));
        }
        assert!(!k.hub.status().memory.unwrap().paused);
        assert!(k.calls().contains(&"send:next".to_string()), "{:?}", k.calls());
    }

    #[test]
    fn sustained_critical_pressure_hibernates_an_idle_session_but_never_one_waiting_on_you() {
        let k = Kit::new();
        let t = with_root(&k, "a", 101);
        k.hook(&t, json!({"hook_event_name": "UserPromptSubmit"}));
        k.hook(&t, json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash"}));
        let s = FakeSampler::default();
        s.set_tree(101, 800 * MB);
        s.set_system(sys(Pressure::Critical));
        let mut p = Policy::new(cfg());
        let now = Instant::now();
        for i in 0..20 {
            k.hub.memory_tick(&mut p, &s, now + Duration::from_secs(i));
        }
        assert!(k.hub.get_task(&t).unwrap().live, "a session with a pending permission is never hibernated");
        assert!(!kinds(&k, Some(&t)).contains(&"memory.auto_hibernated".to_string()));
        assert!(kinds(&k, Some(&t)).contains(&"memory.refused".to_string()));
        // Answered (in the terminal) and idle again: now it may go.
        k.hook(&t, json!({"hook_event_name": "Stop"}));
        assert!(k.pending(&t).is_empty());
        for i in 20..120 {
            k.hub.memory_tick(&mut p, &s, now + Duration::from_secs(i));
        }
        let v = k.hub.get_task(&t).unwrap();
        assert!(v.hibernated && !v.live, "{v:?}");
        assert_eq!(kinds(&k, Some(&t)).iter().filter(|k| *k == "memory.auto_hibernated").count(), 1);
        assert!(k.calls().contains(&"stop".to_string()));
    }

    #[test]
    fn hibernate_and_wake_round_trip_on_the_native_conversation() {
        let k = Kit::new();
        let t = with_root(&k, "a", 101);
        k.emit(DriverEvent::NativeId("nat-7".into()));
        assert!(matches!(k.hub.wake(&t), Err(HubError::Conflict(_))));
        let v = k.hub.hibernate(&t).unwrap();
        assert!(v.hibernated && !v.live);
        let r = k.hub.wake(&t).unwrap();
        assert!(r.resumed);
        assert_eq!(k.factory.resumes.lock().unwrap().last().cloned().flatten().as_deref(), Some("nat-7"));
        assert!(!k.hub.get_task(&t).unwrap().hibernated);
        assert!(matches!(k.hub.hibernate("nope"), Err(HubError::NotFound(_))));
    }

    #[test]
    fn a_real_process_tree_is_measured() {
        use std::os::unix::process::CommandExt;
        let k = Kit::new();
        let mut child =
            std::process::Command::new("/bin/sh").args(["-c", "sleep 30 & sleep 30"]).process_group(0).spawn().unwrap();
        let root = proc_of(child.id() as i32).unwrap();
        *k.factory.driver.root.lock().unwrap() = Some(root);
        let t = k.started(AgentKind::Claude, "real").task.id;
        let sampler = memguard::os_sampler();
        let mut p = Policy::new(cfg());
        k.hub.memory_tick(&mut p, sampler.as_ref(), Instant::now());
        let m = k.hub.get_task(&t).unwrap().memory.expect("sampled");
        assert!(m.bytes > 0 && m.bytes < 1000 * MB, "{m:?}");
        let r = crate::driver::proctree::kill_tree(root, Duration::from_secs(2), &Kill);
        assert!(r.survivors.is_empty());
        let _ = child.wait();
    }

    #[test]
    fn doctor_reports_orphan_checkouts_and_refuses_to_kill_a_non_orphan() {
        let k = Kit::new();
        k.create(AgentKind::Claude, "owned");
        let stray = k.hub.paths().worktrees().join("app/stray");
        std::fs::create_dir_all(stray.join(".git")).unwrap();
        let d = k.hub.doctor().unwrap();
        assert_eq!(d.orphan_worktrees.len(), 1, "{d:?}");
        assert!(d.orphan_worktrees[0].ends_with("app/stray"));
        let me = proc_of(std::process::id() as i32).unwrap();
        let r = k.hub.kill_orphan(KillOrphan { pid: me.pid, start: me.start });
        assert!(matches!(r, Err(HubError::NotFound(_))), "{r:?}");
    }

    #[test]
    fn config_reads_the_users_overrides() {
        let c = config_from(16 * GIB, Some("2048"), Some("60"));
        assert_eq!((c.budget, c.idle_hibernate), (2048 * MB, Some(Duration::from_secs(3600))));
        let d = config_from(16 * GIB, Some("x"), None);
        assert_eq!((d.budget, d.idle_hibernate), (4 * GIB, None));
        assert_eq!(human(1536 * MB), "1.5G");
        assert_eq!(human(512 * MB), "512M");
    }

    use mirante_ipc::types::SendMode;
}
