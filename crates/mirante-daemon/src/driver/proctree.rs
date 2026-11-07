//! Process trees: which processes descend from an agent, and ending all of them.
//!
//! A process is named by its pid AND its start time ([`Proc`]): a pid can be reused as soon
//! as its process is reaped, so nothing is ever signaled unless its start time still
//! matches. Agents run as the leader of their own process group (the PTY child calls
//! `setsid`; `codex app-server` is spawned with `process_group(0)`), so one `killpg`
//! reaches every child that stayed in the group; children that moved to another group
//! (`setsid`, `setpgrp`) are found by walking parent links (macOS: `proc_listchildpids`;
//! Linux: `/proc/<pid>/stat`).
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// One process: pid plus start time (µs since the epoch on macOS, clock ticks since boot
/// on Linux). Two values with the same pid and a different start are different processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Proc {
    pub pid: i32,
    pub start: u64,
}

#[cfg(target_os = "macos")]
mod sys {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};

    /// `SZOMB` from `<sys/proc.h>`: exited, not yet reaped.
    const SZOMB: u32 = 5;

    fn bsdinfo(pid: i32) -> Option<libc::proc_bsdinfo> {
        if pid <= 0 {
            return None;
        }
        // SAFETY: `info` is a plain C struct of the size passed; the kernel fills at most that.
        let mut info: libc::proc_bsdinfo = unsafe { zeroed() };
        let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, &mut info as *mut _ as *mut c_void, size) };
        (n == size && info.pbi_status != SZOMB).then_some(info)
    }

    pub fn start_time(pid: i32) -> Option<u64> {
        bsdinfo(pid).map(|i| i.pbi_start_tvsec * 1_000_000 + i.pbi_start_tvusec)
    }

    pub fn pgid(pid: i32) -> Option<i32> {
        bsdinfo(pid).map(|i| i.pbi_pgid as i32)
    }

    pub fn children(pid: i32) -> Vec<i32> {
        if pid <= 0 {
            return Vec::new();
        }
        let mut cap = 64usize;
        loop {
            let mut buf = vec![0 as libc::pid_t; cap];
            let bytes = (cap * size_of::<libc::pid_t>()) as libc::c_int;
            // SAFETY: the buffer holds `cap` pids and its size in bytes is passed.
            let n = unsafe { libc::proc_listchildpids(pid, buf.as_mut_ptr() as *mut c_void, bytes) };
            if n < 0 {
                return Vec::new();
            }
            let n = n as usize;
            if n < cap {
                buf.truncate(n);
                buf.retain(|p| *p > 0);
                return buf;
            }
            cap *= 4;
        }
    }
}

#[cfg(target_os = "linux")]
mod sys {
    /// (state, ppid, pgid, start) from `/proc/<pid>/stat` (fields 3, 4, 5 and 22).
    fn stat(pid: i32) -> Option<(char, i32, i32, u64)> {
        if pid <= 0 {
            return None;
        }
        let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // The command name may contain spaces and parentheses: fields start after the last ')'.
        let rest = &s[s.rfind(')')? + 2..];
        let f: Vec<&str> = rest.split_whitespace().collect();
        let state = f.first()?.chars().next()?;
        Some((state, f.get(1)?.parse().ok()?, f.get(2)?.parse().ok()?, f.get(19)?.parse().ok()?))
    }

    pub fn start_time(pid: i32) -> Option<u64> {
        stat(pid).filter(|s| s.0 != 'Z' && s.0 != 'X').map(|s| s.3)
    }

    pub fn pgid(pid: i32) -> Option<i32> {
        stat(pid).map(|s| s.2)
    }

    pub fn children(pid: i32) -> Vec<i32> {
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok())
            .filter(|c| stat(*c).is_some_and(|s| s.1 == pid))
            .collect()
    }
}

/// The process `pid` now (`None`: gone or a zombie).
pub fn proc_of(pid: i32) -> Option<Proc> {
    sys::start_time(pid).map(|start| Proc { pid, start })
}

/// Still running, and still the same process (not a reused pid).
pub fn alive(p: Proc) -> bool {
    sys::start_time(p.pid) == Some(p.start)
}

pub fn pgid(pid: i32) -> Option<i32> {
    sys::pgid(pid)
}

/// Direct children of `pid` (unverified pids; see [`descendants`]).
pub fn children(pid: i32) -> Vec<i32> {
    sys::children(pid)
}

/// Every live process below `root` (not `root` itself), parents before children. Empty
/// when `root` itself is gone (its pid may name another process now). A child that started
/// before its parent is a reused pid, not a descendant, and is skipped.
pub fn descendants(root: Proc) -> Vec<Proc> {
    let mut out = Vec::new();
    if !alive(root) {
        return out;
    }
    let mut seen = HashSet::from([root.pid]);
    let mut next = vec![root];
    while let Some(parent) = next.pop() {
        for pid in children(parent.pid) {
            let Some(p) = proc_of(pid) else { continue };
            if p.start >= parent.start && seen.insert(pid) {
                out.push(p);
                next.push(p);
            }
        }
    }
    out
}

/// Where signals go. Tests pass a recorder; production uses [`Kill`].
pub trait Signaler: Send + Sync {
    fn signal(&self, pid: i32, sig: i32) -> bool;
    fn signal_group(&self, pgid: i32, sig: i32) -> bool;
}

/// `kill(2)` / `killpg(2)`.
pub struct Kill;

impl Signaler for Kill {
    fn signal(&self, pid: i32, sig: i32) -> bool {
        // SAFETY: plain syscall; `pid` > 0 is checked by the caller.
        pid > 0 && unsafe { libc::kill(pid, sig) } == 0
    }
    fn signal_group(&self, pgid: i32, sig: i32) -> bool {
        // SAFETY: plain syscall; `pgid` > 1 is checked by the caller.
        pgid > 1 && unsafe { libc::killpg(pgid, sig) } == 0
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KillReport {
    /// Processes that got SIGTERM.
    pub terminated: usize,
    /// Processes that needed SIGKILL.
    pub killed: usize,
    /// Still alive after the verify pass (should be empty).
    pub survivors: Vec<Proc>,
}

/// Signals `sig` to the root's process group (when the group still provably belongs to
/// this tree) and to every known process of the tree that is still the same process.
fn signal_tree(root: Proc, known: &[Proc], sig: i32, s: &dyn Signaler) -> usize {
    let group_ours = alive(root) || known.iter().any(|p| alive(*p) && pgid(p.pid) == Some(root.pid));
    if group_ours && root.pid > 1 {
        s.signal_group(root.pid, sig);
    }
    let mut n = 0;
    for p in std::iter::once(&root).chain(known) {
        if p.pid > 1 && alive(*p) {
            s.signal(p.pid, sig);
            n += 1;
        }
    }
    n
}

/// Ends `root` and everything below it: SIGTERM to the tree, then up to `grace` for it to
/// exit (the tree is re-walked on every poll, so a process forked meanwhile is caught and
/// told too), then SIGKILL to whatever is left, then a verify pass. Never signals a
/// process that is not (by pid AND start time) part of the tree.
pub fn kill_tree(root: Proc, grace: Duration, s: &dyn Signaler) -> KillReport {
    let mut known: Vec<Proc> = descendants(root);
    let mut report = KillReport { terminated: signal_tree(root, &known, libc::SIGTERM, s), ..Default::default() };
    let deadline = Instant::now() + grace;
    loop {
        // Newly forked processes get SIGTERM too.
        let mut fresh = Vec::new();
        for p in std::iter::once(root).chain(known.clone()) {
            if alive(p) {
                for d in descendants(p) {
                    if !known.contains(&d) {
                        fresh.push(d);
                    }
                }
            }
        }
        for d in fresh {
            if !known.contains(&d) {
                if alive(d) && s.signal(d.pid, libc::SIGTERM) {
                    report.terminated += 1;
                }
                known.push(d);
            }
        }
        let left = !alive(root) && known.iter().all(|p| !alive(*p));
        if left || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    // Hard kill, repeated until nothing new shows up (a dying parent may still fork).
    for _ in 0..5 {
        for p in std::iter::once(root).chain(known.clone()) {
            if alive(p) {
                for d in descendants(p) {
                    if !known.contains(&d) {
                        known.push(d);
                    }
                }
            }
        }
        let n = signal_tree(root, &known, libc::SIGKILL, s);
        report.killed += n;
        if n == 0 {
            break;
        }
        let until = Instant::now() + Duration::from_millis(500);
        while Instant::now() < until && (alive(root) || known.iter().any(|p| alive(*p))) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    report.survivors = std::iter::once(root).chain(known).filter(|p| alive(*p)).collect();
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command};
    use std::sync::Mutex;

    /// Records every signal, then sends it.
    struct Recorder {
        calls: Mutex<Vec<(i32, i32, bool)>>,
    }

    impl Signaler for Recorder {
        fn signal(&self, pid: i32, sig: i32) -> bool {
            self.calls.lock().unwrap().push((pid, sig, false));
            Kill.signal(pid, sig)
        }
        fn signal_group(&self, pgid: i32, sig: i32) -> bool {
            self.calls.lock().unwrap().push((pgid, sig, true));
            Kill.signal_group(pgid, sig)
        }
    }

    /// A shell in its own process group (like an agent), reaped by a background thread so
    /// it never lingers as a zombie.
    fn spawn_group(script: &str) -> (Proc, std::thread::JoinHandle<()>) {
        use std::os::unix::process::CommandExt;
        let mut child: Child = Command::new("/bin/sh").args(["-c", script]).process_group(0).spawn().unwrap();
        let pid = child.id() as i32;
        let p = proc_of(pid).expect("just started");
        (p, std::thread::spawn(move || drop(child.wait())))
    }

    fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ok() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_process_is_its_pid_and_start_time() {
        let me = proc_of(std::process::id() as i32).unwrap();
        assert!(alive(me));
        assert!(!alive(Proc { pid: me.pid, start: me.start + 1 }), "another start time is another process");
        assert_eq!(proc_of(-5), None);
        assert_eq!(proc_of(0), None);
    }

    #[test]
    fn descendants_include_children_that_left_the_group() {
        // The grandchild calls setpgrp: out of the agent's process group, still below it.
        let (root, reaper) = spawn_group("perl -e 'setpgrp(0,0); sleep 30' & sleep 30 & wait");
        wait_until("one descendant in the group and one outside it", || {
            let groups: Vec<Option<i32>> = descendants(root).iter().map(|p| pgid(p.pid)).collect();
            groups.contains(&Some(root.pid)) && groups.iter().any(|g| g.is_some_and(|g| g != root.pid))
        });
        let r = kill_tree(root, Duration::from_secs(2), &Kill);
        assert!(r.survivors.is_empty(), "{r:?}");
        reaper.join().unwrap();
    }

    #[test]
    fn kill_tree_ends_a_tree_that_forks_while_dying_and_ignores_sigterm() {
        let dir = mirante_ipc::test_dir::TestDir::new();
        let pidfile = dir.path().join("forked.pid");
        let (r1, r2) = (dir.path().join("r1"), dir.path().join("r2"));
        // Child 1 ignores SIGTERM. Child 2 forks a new sleeper (in its own group) when told
        // to stop, records its pid, and keeps running. Each says when its trap is set.
        let script = format!(
            "sh -c 'trap \"\" TERM; touch {r1}; while :; do sleep 0.1; done' & \
             sh -c 'trap \"perl -e \\\"setpgrp(0,0); sleep 60\\\" & echo \\$! > {p}\" TERM; touch {r2}; while :; do sleep 0.1; done' & \
             wait",
            p = pidfile.display(),
            r1 = r1.display(),
            r2 = r2.display()
        );
        let (root, reaper) = spawn_group(&script);
        wait_until("both traps set", || r1.exists() && r2.exists());
        let rec = Recorder { calls: Mutex::new(Vec::new()) };
        let r = kill_tree(root, Duration::from_millis(800), &rec);
        assert!(r.survivors.is_empty(), "{r:?}");
        assert!(r.killed >= 1, "the TERM-ignoring child needed SIGKILL: {r:?}");
        let forked: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        assert!(proc_of(forked).is_none(), "the child forked during the kill is gone too");
        assert!(rec.calls.lock().unwrap().iter().any(|(pid, _, group)| *pid == forked && !group));
        reaper.join().unwrap();
    }

    #[test]
    fn a_reused_pid_is_never_signaled() {
        let rec = Recorder { calls: Mutex::new(Vec::new()) };
        let me = proc_of(std::process::id() as i32).unwrap();
        // Our own pid with a start time that is not ours: a "reused" pid.
        let stale = Proc { pid: me.pid, start: me.start.wrapping_sub(1) };
        let r = kill_tree(stale, Duration::ZERO, &rec);
        assert!(rec.calls.lock().unwrap().is_empty(), "{:?}", rec.calls.lock().unwrap());
        assert_eq!((r.terminated, r.killed), (0, 0));
        assert!(alive(me));
    }
}
