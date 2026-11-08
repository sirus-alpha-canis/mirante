//! The memory policy: a pure state machine fed one sample per tick, with the clock injected
//! (`now`), returning the actions the hub should take. It never touches a process itself.
//!
//! Per session: a budget (default `min(4 GiB, 25 % of RAM)`), `Soft` at 80 % and `Hard` at
//! 100 % of it, each reported once when the state is entered. For the machine: a [`Level`]
//! from the OS pressure signal and swap use, stepping up after [`UP_SAMPLES`] samples in a row
//! and down only after [`CALM_SAMPLES`] calm samples below a lower threshold (hysteresis). The ladder:
//! 1. `Elevated` → pause new dispatches and queued deliveries, offer to hibernate the
//!    largest idle session (the user confirms; once per session per episode, and at most
//!    once per `offer_cooldown`, 10 min, per session). Stepping up also takes
//!    [`UP_SAMPLES`] samples in a row, so a flapping kernel level opens no episode;
//! 2. `Critical` sustained for `critical_sustain` → hibernate the largest IDLE session
//!    automatically, one action per cooldown. Never a session that is working (turn, tool,
//!    background work) and never one with a pending permission request: when only such
//!    sessions are left, `Refused` says so instead.
//!
//! Also: an idle session that keeps growing faster than `leak_mb_per_min` over
//! `leak_window` is reported as a leak (with an offer), and, only when configured, a session
//! idle longer than `idle_hibernate` is hibernated.
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use super::sampler::{Pressure, SystemSample, TreeSample};
use crate::driver::proctree::Proc;

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;
/// Calm samples in a row before the level steps down.
pub const CALM_SAMPLES: u32 = 3;
/// Samples in a row above the level before it steps up: a kernel level that flaps between
/// two values every sample (seen on macOS: 1 <-> 2) opens no episode.
pub const UP_SAMPLES: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Bytes one session's process tree may use.
    pub budget: u64,
    pub soft_pct: u8,
    pub hard_pct: u8,
    /// Sampling interval while the level is `Normal`.
    pub interval: Duration,
    /// Sampling interval under pressure.
    pub pressure_interval: Duration,
    /// At most one automatic action (and one refusal) per cooldown.
    pub cooldown: Duration,
    /// A session is offered for hibernation at most once per pressure episode, and never
    /// again within this long of its last offer.
    pub offer_cooldown: Duration,
    /// How long `Critical` must last before a session is hibernated automatically.
    pub critical_sustain: Duration,
    /// Hibernate sessions idle this long (`None`: never; the default).
    pub idle_hibernate: Option<Duration>,
    /// An idle session growing faster than this is reported as leaking.
    pub leak_mb_per_min: u64,
    pub leak_window: Duration,
    /// Linux only: launch agents under `systemd-run --user --scope -p MemoryHigh=<budget>`.
    pub systemd_scope: bool,
}

impl Config {
    /// Defaults for a machine with `total` bytes of RAM.
    pub fn for_ram(total: u64) -> Config {
        Config {
            budget: (4 * GIB).min(total / 4).max(GIB / 4),
            soft_pct: 80,
            hard_pct: 100,
            interval: Duration::from_secs(2),
            pressure_interval: Duration::from_millis(500),
            cooldown: Duration::from_secs(60),
            offer_cooldown: Duration::from_secs(600),
            critical_sustain: Duration::from_secs(10),
            idle_hibernate: None,
            leak_mb_per_min: 200,
            leak_window: Duration::from_secs(300),
            systemd_scope: false,
        }
    }

    /// The argv prefix that runs an agent in a systemd user scope with `MemoryHigh` set to
    /// the budget (the kernel then reclaims from it first). `None` unless `systemd_scope`.
    pub fn scope_prefix(&self) -> Option<Vec<String>> {
        self.systemd_scope.then(|| {
            ["systemd-run", "--user", "--scope", "--quiet", "-p"]
                .iter()
                .map(|s| s.to_string())
                .chain([format!("MemoryHigh={}", self.budget), "--".to_string()])
                .collect()
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Normal,
    Elevated,
    Critical,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Normal => "normal",
            Level::Elevated => "elevated",
            Level::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemState {
    Ok,
    Soft,
    Hard,
}

impl MemState {
    pub fn as_str(self) -> &'static str {
        match self {
            MemState::Ok => "ok",
            MemState::Soft => "soft",
            MemState::Hard => "hard",
        }
    }
}

/// What the hub tells the policy about one live session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub task_id: String,
    pub root: Option<Proc>,
    /// No streaming turn, no running tool, no background work.
    pub idle: bool,
    pub pending_permission: bool,
    /// When it last became idle (`None` while busy).
    pub idle_since: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionMem {
    pub bytes: u64,
    pub peak: u64,
    pub budget: u64,
    pub state: MemState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// A session entered `Soft` or `Hard`.
    Warn {
        task_id: String,
        bytes: u64,
        budget: u64,
        state: MemState,
    },
    /// Pause (true) or resume (false) new dispatches and queued deliveries.
    Pause(bool),
    /// Ask the user whether to hibernate this session.
    OfferHibernate {
        task_id: String,
        reason: String,
    },
    /// Hibernate this idle session now (sustained critical pressure).
    AutoHibernate {
        task_id: String,
        reason: String,
    },
    /// An idle session keeps growing.
    Leak {
        task_id: String,
        mb_per_min: u64,
    },
    /// Configured idle hibernation is due.
    IdleHibernate {
        task_id: String,
    },
    LevelChanged {
        from: Level,
        to: Level,
    },
    /// An automatic hibernation was due but this session may not be touched.
    Refused {
        task_id: String,
        reason: String,
    },
}

struct Track {
    mem: SessionMem,
    /// (time, bytes) while idle, newest last, for the leak detector.
    history: VecDeque<(Instant, u64)>,
    leak_reported: bool,
    /// The `idle_since` an idle hibernation was already asked for.
    idle_reported: Option<Instant>,
}

pub struct Policy {
    cfg: Config,
    level: Level,
    calm: u32,
    /// Samples in a row above the level, and the lowest level they reached.
    rising: u32,
    rising_to: Level,
    /// Bumped each time the level leaves `Normal` (a pressure episode).
    episode: u64,
    /// Per task: the episode and time of its last hibernation offer.
    offered: HashMap<String, (u64, Instant)>,
    last_free: Option<u64>,
    critical_since: Option<Instant>,
    last_action: Option<Instant>,
    last_refusal: Option<Instant>,
    sessions: HashMap<String, Track>,
}

/// The kernel's own level wins when there is one (`kernel_signal`): on macOS swap stays
/// allocated and free pages stay near zero (file cache) on a machine under no pressure —
/// measured on the founder's Mac: 90 % swap, 2 % "free", kernel level normal. Without it
/// (Linux without PSI), swap counts only together with little free memory: over
/// `elevated_pct` with under 10 % free is `Elevated`; over `critical_pct` with under 5 %
/// free and falling is `Critical`.
fn classify(s: &SystemSample, free_falling: bool, elevated_pct: u64, critical_pct: u64) -> Level {
    match s.pressure {
        Pressure::Critical => return Level::Critical,
        Pressure::Warn => return Level::Elevated,
        Pressure::Normal if s.kernel_signal => return Level::Normal,
        Pressure::Normal => {}
    }
    let swap = s.swap_pct();
    let free_pct = s.free.saturating_mul(100).checked_div(s.total).unwrap_or(100);
    let swapping = |pct: u64| s.swap_total > 0 && swap > pct;
    if swapping(critical_pct) && free_falling && free_pct < 5 {
        Level::Critical
    } else if swapping(elevated_pct) && free_pct < 10 {
        Level::Elevated
    } else {
        Level::Normal
    }
}

fn due(last: Option<Instant>, now: Instant, cooldown: Duration) -> bool {
    last.is_none_or(|t| now.saturating_duration_since(t) >= cooldown)
}

impl Policy {
    pub fn new(cfg: Config) -> Policy {
        Policy {
            cfg,
            level: Level::Normal,
            calm: 0,
            rising: 0,
            rising_to: Level::Normal,
            episode: 0,
            offered: HashMap::new(),
            last_free: None,
            critical_since: None,
            last_action: None,
            last_refusal: None,
            sessions: HashMap::new(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn level(&self) -> Level {
        self.level
    }

    pub fn session(&self, task_id: &str) -> Option<SessionMem> {
        self.sessions.get(task_id).map(|t| t.mem)
    }

    /// When to sample next: faster under pressure, and while samples rise toward a higher
    /// level (so the climb is confirmed quickly).
    pub fn next_interval(&self) -> Duration {
        if self.level == Level::Normal && self.rising == 0 {
            self.cfg.interval
        } else {
            self.cfg.pressure_interval
        }
    }

    /// The session ended (or was hibernated): drop what was tracked about it.
    pub fn forget(&mut self, task_id: &str) {
        self.sessions.remove(task_id);
    }

    fn update_level(&mut self, now: Instant, s: &SystemSample, out: &mut Vec<Action>) {
        let falling = self.last_free.is_some_and(|f| s.free < f);
        self.last_free = Some(s.free);
        let up = classify(s, falling, 75, 90);
        // Stepping down needs a margin below the thresholds and several calm samples.
        let down = classify(s, falling, 70, 85);
        let from = self.level;
        if up > self.level {
            self.calm = 0;
            self.rising_to = if self.rising == 0 { up } else { self.rising_to.min(up) };
            self.rising += 1;
            if self.rising >= UP_SAMPLES {
                self.level = self.rising_to;
                self.rising = 0;
            }
        } else if down < self.level {
            self.rising = 0;
            self.calm += 1;
            if self.calm >= CALM_SAMPLES {
                self.level = down;
                self.calm = 0;
            }
        } else {
            self.rising = 0;
            self.calm = 0;
        }
        if self.level != from {
            if from == Level::Normal {
                self.episode += 1;
            }
            out.push(Action::LevelChanged { from, to: self.level });
            if from == Level::Normal {
                out.push(Action::Pause(true));
            } else if self.level == Level::Normal {
                out.push(Action::Pause(false));
            }
        }
        match self.level {
            Level::Critical => {
                self.critical_since.get_or_insert(now);
            }
            _ => self.critical_since = None,
        }
    }

    fn update_session(&mut self, now: Instant, info: &SessionInfo, sample: TreeSample, out: &mut Vec<Action>) {
        let budget = self.cfg.budget;
        let state = if sample.bytes >= budget * u64::from(self.cfg.hard_pct) / 100 {
            MemState::Hard
        } else if sample.bytes >= budget * u64::from(self.cfg.soft_pct) / 100 {
            MemState::Soft
        } else {
            MemState::Ok
        };
        let t = self.sessions.entry(info.task_id.clone()).or_insert_with(|| Track {
            mem: SessionMem { bytes: 0, peak: 0, budget, state: MemState::Ok },
            history: VecDeque::new(),
            leak_reported: false,
            idle_reported: None,
        });
        if state != t.mem.state && state != MemState::Ok {
            out.push(Action::Warn { task_id: info.task_id.clone(), bytes: sample.bytes, budget, state });
        }
        t.mem = SessionMem { bytes: sample.bytes, peak: t.mem.peak.max(sample.bytes), budget, state };
        // Leak detector: growth while idle only (a working agent may legitimately grow).
        if !info.idle {
            t.history.clear();
            t.leak_reported = false;
        } else {
            t.history.push_back((now, sample.bytes));
            while t.history.front().is_some_and(|(at, _)| now.saturating_duration_since(*at) > self.cfg.leak_window) {
                t.history.pop_front();
            }
            let (first_at, first) = *t.history.front().expect("just pushed");
            let span = now.saturating_duration_since(first_at);
            if span >= self.cfg.leak_window / 2 && span > Duration::ZERO {
                let grown = sample.bytes.saturating_sub(first);
                let per_min = grown * 60 / span.as_secs().max(1) / MIB;
                if per_min > self.cfg.leak_mb_per_min {
                    if !t.leak_reported {
                        t.leak_reported = true;
                        out.push(Action::Leak { task_id: info.task_id.clone(), mb_per_min: per_min });
                        out.push(Action::OfferHibernate {
                            task_id: info.task_id.clone(),
                            reason: format!("idle but growing {per_min} MB/min"),
                        });
                    }
                } else {
                    t.leak_reported = false;
                }
            }
        }
        // Configured idle hibernation, once per idle period.
        if let (Some(after), Some(since), true) = (self.cfg.idle_hibernate, info.idle_since, info.idle) {
            if !info.pending_permission
                && now.saturating_duration_since(since) >= after
                && t.idle_reported != Some(since)
            {
                t.idle_reported = Some(since);
                out.push(Action::IdleHibernate { task_id: info.task_id.clone() });
            }
        }
    }

    /// One sample of the machine and of every live session; returns what to do.
    pub fn tick(
        &mut self,
        now: Instant,
        system: &SystemSample,
        sessions: &[(SessionInfo, Option<TreeSample>)],
    ) -> Vec<Action> {
        let mut out = Vec::new();
        self.update_level(now, system, &mut out);
        // Only live sessions are tracked: one that ended without a `forget` (an exit, a
        // stop) leaves nothing behind for a later session of the same task to inherit.
        self.sessions.retain(|task_id, _| sessions.iter().any(|(i, _)| i.task_id == *task_id));
        for (info, sample) in sessions {
            if let Some(sample) = sample {
                self.update_session(now, info, *sample, &mut out);
            }
        }
        // Candidates, largest first.
        let mut measured: Vec<(&SessionInfo, u64)> =
            sessions.iter().filter_map(|(i, s)| s.map(|s| (i, s.bytes))).collect();
        measured.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.task_id.cmp(&b.0.task_id)));
        let victim = measured.iter().find(|(i, _)| i.idle && !i.pending_permission).map(|(i, _)| i.task_id.clone());
        let sustained =
            self.critical_since.is_some_and(|t| now.saturating_duration_since(t) >= self.cfg.critical_sustain);
        let mut acted = false;
        if sustained && due(self.last_action, now, self.cfg.cooldown) {
            match &victim {
                Some(task_id) => {
                    self.last_action = Some(now);
                    acted = true;
                    out.push(Action::AutoHibernate {
                        task_id: task_id.clone(),
                        reason: format!("critical memory pressure for {} s", self.cfg.critical_sustain.as_secs()),
                    });
                }
                None if due(self.last_refusal, now, self.cfg.cooldown) => {
                    let pending = measured.iter().find(|(i, _)| i.idle && i.pending_permission);
                    let refusal = match pending {
                        Some((i, _)) => Some((
                            i.task_id.clone(),
                            "a permission request is pending: never hibernated automatically".to_string(),
                        )),
                        None => measured.first().map(|(i, _)| {
                            (
                                i.task_id.clone(),
                                "it is working (turn, tool or background work): never hibernated automatically"
                                    .to_string(),
                            )
                        }),
                    };
                    if let Some((task_id, reason)) = refusal {
                        self.last_refusal = Some(now);
                        out.push(Action::Refused { task_id, reason });
                    }
                }
                None => {}
            }
        }
        // Offers for tasks that are gone, once their cooldown is over, are forgotten.
        let cooldown = self.cfg.offer_cooldown;
        self.offered.retain(|task_id, (_, at)| {
            sessions.iter().any(|(i, _)| &i.task_id == task_id) || now.saturating_duration_since(*at) < cooldown
        });
        if !acted && self.level != Level::Normal {
            if let Some(task_id) = victim {
                // Once per session per episode, and never within `offer_cooldown` of the last.
                let fresh = self.offered.get(&task_id).is_none_or(|(ep, at)| {
                    *ep != self.episode && now.saturating_duration_since(*at) >= self.cfg.offer_cooldown
                });
                if fresh {
                    self.offered.insert(task_id.clone(), (self.episode, now));
                    out.push(Action::OfferHibernate {
                        task_id,
                        reason: format!("memory pressure {}", self.level.as_str()),
                    });
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1 << 30;

    fn cfg() -> Config {
        Config::for_ram(16 * GB)
    }

    fn sys(pressure: Pressure, swap_pct: u64, free: u64) -> SystemSample {
        SystemSample {
            total: 16 * GB,
            free,
            swap_used: swap_pct * GB / 100,
            swap_total: GB,
            available: None,
            pressure,
            kernel_signal: false,
        }
    }

    fn normal() -> SystemSample {
        sys(Pressure::Normal, 0, 4 * GB)
    }

    fn s(task: &str, idle: bool, pending: bool, bytes: u64) -> (SessionInfo, Option<TreeSample>) {
        let info =
            SessionInfo { task_id: task.into(), root: None, idle, pending_permission: pending, idle_since: None };
        (info, Some(TreeSample { bytes, procs: 1 }))
    }

    fn kinds(a: &[Action]) -> Vec<String> {
        a.iter()
            .map(|a| match a {
                Action::Warn { task_id, state, .. } => format!("warn:{task_id}:{}", state.as_str()),
                Action::Pause(p) => format!("pause:{p}"),
                Action::OfferHibernate { task_id, .. } => format!("offer:{task_id}"),
                Action::AutoHibernate { task_id, .. } => format!("auto:{task_id}"),
                Action::Leak { task_id, .. } => format!("leak:{task_id}"),
                Action::IdleHibernate { task_id } => format!("idle:{task_id}"),
                Action::LevelChanged { to, .. } => format!("level:{}", to.as_str()),
                Action::Refused { task_id, .. } => format!("refused:{task_id}"),
            })
            .collect()
    }

    #[test]
    fn defaults_follow_the_ram_and_leave_idle_hibernation_off() {
        assert_eq!(Config::for_ram(16 * GB).budget, 4 * GB);
        assert_eq!(Config::for_ram(8 * GB).budget, 2 * GB);
        let c = cfg();
        assert_eq!((c.soft_pct, c.hard_pct, c.idle_hibernate, c.systemd_scope), (80, 100, None, false));
        assert_eq!(c.scope_prefix(), None);
        let on = Config { systemd_scope: true, budget: 1000, ..c };
        assert_eq!(
            on.scope_prefix().unwrap(),
            ["systemd-run", "--user", "--scope", "--quiet", "-p", "MemoryHigh=1000", "--"]
        );
    }

    #[test]
    fn soft_and_hard_are_reported_once_per_state() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        assert!(kinds(&p.tick(t0, &normal(), &[s("a", false, false, GB)])).is_empty());
        assert_eq!(kinds(&p.tick(t0, &normal(), &[s("a", false, false, 3 * GB + GB / 2)])), ["warn:a:soft"]);
        assert!(kinds(&p.tick(t0, &normal(), &[s("a", false, false, 3 * GB + GB / 2)])).is_empty());
        assert_eq!(kinds(&p.tick(t0, &normal(), &[s("a", false, false, 5 * GB)])), ["warn:a:hard"]);
        let m = p.session("a").unwrap();
        assert_eq!((m.bytes, m.peak, m.budget, m.state), (5 * GB, 5 * GB, 4 * GB, MemState::Hard));
        p.tick(t0, &normal(), &[s("a", false, false, GB)]);
        assert_eq!(p.session("a").unwrap().peak, 5 * GB);
        assert_eq!(kinds(&p.tick(t0, &normal(), &[s("a", false, false, 3 * GB + GB / 2)])), ["warn:a:soft"]);
        p.forget("a");
        assert_eq!(p.session("a"), None);
    }

    /// A session that ended without the hub telling the policy (no hibernation) is not
    /// remembered: what is tracked is what the last tick saw live.
    #[test]
    fn a_session_missing_from_a_tick_is_forgotten() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        p.tick(t0, &normal(), &[s("a", false, false, 5 * GB), s("b", false, false, GB)]);
        p.tick(t0, &normal(), &[s("b", false, false, GB)]);
        assert_eq!(p.session("a"), None);
        assert!(p.session("b").is_some());
        // Back with a new session: it starts fresh (no stale peak or warning state).
        p.tick(t0, &normal(), &[s("a", false, false, GB)]);
        assert_eq!(p.session("a").unwrap().peak, GB);
    }

    /// Up `UP_SAMPLES` samples in a row before the level rises; down after `CALM_SAMPLES`.
    fn up(p: &mut Policy, t: Instant, s: &SystemSample) -> Vec<String> {
        let mut out = Vec::new();
        for _ in 0..UP_SAMPLES {
            out.extend(kinds(&p.tick(t, s, &[])));
        }
        out
    }

    #[test]
    fn the_level_steps_up_after_consecutive_samples_and_down_only_after_calm_samples() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        assert_eq!(p.next_interval(), Duration::from_secs(2));
        for _ in 1..UP_SAMPLES {
            assert!(kinds(&p.tick(t0, &sys(Pressure::Warn, 0, GB), &[])).is_empty());
        }
        assert_eq!(kinds(&p.tick(t0, &sys(Pressure::Warn, 0, GB), &[])), ["level:elevated", "pause:true"]);
        assert_eq!(p.next_interval(), Duration::from_millis(500));
        // Swap at 72 %: below the 75 % entry threshold but above the 70 % exit margin.
        for _ in 0..5 {
            assert!(kinds(&p.tick(t0, &sys(Pressure::Normal, 72, GB), &[])).is_empty());
        }
        assert_eq!(p.level(), Level::Elevated);
        assert!(kinds(&p.tick(t0, &normal(), &[])).is_empty());
        assert!(kinds(&p.tick(t0, &normal(), &[])).is_empty());
        assert_eq!(kinds(&p.tick(t0, &normal(), &[])), ["level:normal", "pause:false"]);
        // Swap over 90 % with little free memory: elevated; falling under 5 %: critical.
        up(&mut p, t0, &sys(Pressure::Normal, 95, 3 * GB / 2));
        assert_eq!(p.level(), Level::Elevated);
        let mut free = GB / 2;
        let mut seen = Vec::new();
        for _ in 0..UP_SAMPLES {
            free -= MIB;
            seen.extend(kinds(&p.tick(t0, &sys(Pressure::Normal, 95, free), &[])));
        }
        assert_eq!(seen, ["level:critical"]);
    }

    /// Review: while samples rise toward a higher level the guard samples fast, so a
    /// climb is confirmed in `UP_SAMPLES` short intervals, not `UP_SAMPLES` x 2 s.
    #[test]
    fn rising_samples_use_the_pressure_interval() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        p.tick(t0, &sys(Pressure::Warn, 0, GB), &[]);
        assert_eq!(p.level(), Level::Normal);
        assert_eq!(p.next_interval(), Duration::from_millis(500));
        p.tick(t0, &normal(), &[]);
        assert_eq!(p.next_interval(), Duration::from_secs(2));
    }

    /// Review: offers for tasks that are gone (ended, archived) are forgotten once their
    /// cooldown has passed; the map does not grow for ever.
    #[test]
    fn offers_for_gone_tasks_are_pruned_after_their_cooldown() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        up(&mut p, t0, &sys(Pressure::Warn, 0, GB));
        let out = kinds(&p.tick(t0, &sys(Pressure::Warn, 0, GB), &[s("gone", true, false, GB)]));
        assert_eq!(out, ["offer:gone"]);
        assert!(p.offered.contains_key("gone"));
        // Still within the cooldown: kept (a session coming back is not offered again).
        p.tick(t0 + Duration::from_secs(60), &sys(Pressure::Warn, 0, GB), &[]);
        assert!(p.offered.contains_key("gone"));
        let later = t0 + p.config().offer_cooldown + Duration::from_secs(1);
        p.tick(later, &sys(Pressure::Warn, 0, GB), &[s("here", true, false, GB)]);
        assert!(!p.offered.contains_key("gone"), "{:?}", p.offered.keys().collect::<Vec<_>>());
        assert!(p.offered.contains_key("here"));
    }

    /// The founder's Mac: the kernel's level flapping 1 <-> 2 (normal <-> warn) every sample
    /// opened an episode (and an offer) each time. A level must hold to count.
    #[test]
    fn a_flapping_kernel_level_opens_no_episode() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        let sessions = [s("big", true, false, GB)];
        let mut out = Vec::new();
        for i in 0..40u64 {
            let pressure = if i % 2 == 0 { Pressure::Warn } else { Pressure::Normal };
            let mac = SystemSample { kernel_signal: true, ..sys(pressure, 0, 8 * GB) };
            out.extend(kinds(&p.tick(t0 + Duration::from_secs(2 * i), &mac, &sessions)));
        }
        assert!(out.is_empty(), "{out:?}");
        assert_eq!(p.level(), Level::Normal);
    }

    /// Measured on the founder's Mac: swap 90 % used, 2 % "free" (file cache), kernel level
    /// normal. With the kernel's own level, that is normal: nothing is paused.
    #[test]
    fn the_kernels_level_wins_over_swap_and_free() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        let mac = SystemSample { kernel_signal: true, ..sys(Pressure::Normal, 90, GB / 3) };
        for i in 0..5 {
            assert!(kinds(&p.tick(t0 + Duration::from_secs(i), &mac, &[])).is_empty());
        }
        assert_eq!(p.level(), Level::Normal);
        let warn = SystemSample { kernel_signal: true, ..sys(Pressure::Warn, 0, 8 * GB) };
        assert_eq!(up(&mut p, t0 + Duration::from_secs(9), &warn), ["level:elevated", "pause:true"]);
        // Without a kernel signal the same swap counts only with little free memory.
        let mut q = Policy::new(cfg());
        assert!(kinds(&q.tick(t0, &sys(Pressure::Normal, 92, 4 * GB), &[])).is_empty());
    }

    #[test]
    fn pressure_pauses_and_offers_the_largest_idle_session_once_per_episode() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        let warn = sys(Pressure::Warn, 0, GB);
        let sessions = [s("busy", false, false, 3 * GB), s("small", true, false, GB / 2), s("big", true, false, GB)];
        for _ in 1..UP_SAMPLES {
            assert!(kinds(&p.tick(t0, &warn, &sessions)).is_empty());
        }
        let a = kinds(&p.tick(t0, &warn, &sessions));
        assert_eq!(a, ["level:elevated", "pause:true", "offer:big"]);
        // The whole episode (here half an hour): one offer, never repeated.
        let mut later = Vec::new();
        for i in 1..=900u64 {
            later.extend(kinds(&p.tick(t0 + Duration::from_secs(2 * i), &warn, &sessions)));
        }
        assert!(later.is_empty(), "{later:?}");
    }

    #[test]
    fn a_new_episode_offers_a_session_again_only_after_ten_minutes() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        let warn = sys(Pressure::Warn, 0, GB);
        let sessions = [s("big", true, false, GB)];
        let episode = |p: &mut Policy, at: Instant| {
            let mut out = Vec::new();
            for i in 0..UP_SAMPLES + 2 {
                out.extend(kinds(&p.tick(at + Duration::from_secs(u64::from(i)), &warn, &sessions)));
            }
            for i in 0..CALM_SAMPLES + 1 {
                out.extend(kinds(&p.tick(at + Duration::from_secs(30 + u64::from(i)), &normal(), &sessions)));
            }
            out
        };
        assert!(episode(&mut p, t0).contains(&"offer:big".to_string()));
        // Five minutes later, a second episode: paused again, no second offer.
        let second = episode(&mut p, t0 + Duration::from_secs(300));
        assert!(second.contains(&"pause:true".to_string()) && !second.contains(&"offer:big".to_string()), "{second:?}");
        // Past the ten minutes: the next episode may offer it once more.
        assert!(episode(&mut p, t0 + Duration::from_secs(700)).contains(&"offer:big".to_string()));
        assert_eq!(cfg().offer_cooldown, Duration::from_secs(600));
    }

    #[test]
    fn sustained_critical_hibernates_only_an_idle_session_without_a_pending_permission() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        let crit = sys(Pressure::Critical, 0, GB);
        let sessions = [s("busy", false, false, 3 * GB), s("asking", true, true, 2 * GB), s("idle", true, false, GB)];
        // Not sustained yet: only the offer.
        for _ in 1..UP_SAMPLES {
            assert!(kinds(&p.tick(t0, &crit, &sessions)).is_empty());
        }
        assert_eq!(kinds(&p.tick(t0, &crit, &sessions)), ["level:critical", "pause:true", "offer:idle"]);
        assert!(kinds(&p.tick(t0 + Duration::from_secs(5), &crit, &sessions)).is_empty());
        assert_eq!(kinds(&p.tick(t0 + Duration::from_secs(10), &crit, &sessions)), ["auto:idle"]);
        // One automatic action per cooldown.
        assert!(kinds(&p.tick(t0 + Duration::from_secs(40), &crit, &sessions)).is_empty());
        let later = t0 + Duration::from_secs(71);
        assert_eq!(kinds(&p.tick(later, &crit, &sessions)), ["auto:idle"]);
    }

    #[test]
    fn a_pending_permission_or_a_busy_agent_is_refused_explicitly() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        let crit = sys(Pressure::Critical, 0, GB);
        let asking = [s("busy", false, false, 3 * GB), s("asking", true, true, GB)];
        for _ in 0..UP_SAMPLES {
            p.tick(t0, &crit, &asking);
        }
        let a = p.tick(t0 + Duration::from_secs(10), &crit, &asking);
        assert_eq!(kinds(&a), ["refused:asking"]);
        let Action::Refused { reason, .. } = &a[0] else { unreachable!() };
        assert!(reason.contains("permission request is pending"), "{reason}");
        // Refusals are not repeated within the cooldown.
        assert!(kinds(&p.tick(t0 + Duration::from_secs(20), &crit, &asking)).is_empty());
        let mut q = Policy::new(cfg());
        let busy = [s("busy", false, false, 3 * GB)];
        for _ in 0..UP_SAMPLES {
            q.tick(t0, &crit, &busy);
        }
        let a = q.tick(t0 + Duration::from_secs(10), &crit, &busy);
        assert_eq!(kinds(&a), ["refused:busy"]);
        assert!(matches!(&a[0], Action::Refused { reason, .. } if reason.contains("working")));
    }

    #[test]
    fn an_idle_session_that_keeps_growing_is_a_leak() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        let mut out = Vec::new();
        for m in 0..=3u64 {
            // 300 MB per minute, idle.
            out.extend(p.tick(t0 + Duration::from_secs(60 * m), &normal(), &[s("a", true, false, GB + m * 300 * MIB)]));
        }
        assert_eq!(kinds(&out), ["leak:a", "offer:a"]);
        assert!(matches!(&out[0], Action::Leak { mb_per_min: 300, .. }));
        // Reported once; growth while working is not a leak.
        let mut q = Policy::new(cfg());
        let mut out = Vec::new();
        for m in 0..=3u64 {
            out.extend(q.tick(
                t0 + Duration::from_secs(60 * m),
                &normal(),
                &[s("a", false, false, GB + m * 300 * MIB)],
            ));
        }
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn idle_hibernation_happens_only_when_configured() {
        let t0 = Instant::now();
        let idle = |since: Instant| {
            let (mut info, sample) = s("a", true, false, GB);
            info.idle_since = Some(since);
            [(info, sample)]
        };
        let mut off = Policy::new(cfg());
        assert!(off.tick(t0 + Duration::from_secs(7200), &normal(), &idle(t0)).is_empty());
        let mut on = Policy::new(Config { idle_hibernate: Some(Duration::from_secs(3600)), ..cfg() });
        assert!(on.tick(t0 + Duration::from_secs(3599), &normal(), &idle(t0)).is_empty());
        assert_eq!(kinds(&on.tick(t0 + Duration::from_secs(3600), &normal(), &idle(t0))), ["idle:a"]);
        assert!(on.tick(t0 + Duration::from_secs(3700), &normal(), &idle(t0)).is_empty(), "once per idle period");
        // Never with a pending permission.
        let (mut info, sample) = s("b", true, true, GB);
        info.idle_since = Some(t0);
        assert!(on.tick(t0 + Duration::from_secs(9000), &normal(), &[(info, sample)]).is_empty());
    }
}
