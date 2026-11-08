//! Orphan sweep: processes that outlived their agent. An agent's child that was re-parented
//! to init/launchd (ppid 1), runs as us, and whose working directory or command line points
//! into Mirante's worktree root is almost certainly left over from a session (a dev server, a
//! watcher). They are only REPORTED (`mirante doctor`); killing one is the user's explicit
//! choice, and even then only that process tree, re-verified by start time. Nothing is ever
//! matched or killed by process name.
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::driver::proctree::{alive, kill_tree, proc_of, KillReport, Proc, Signaler};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Orphan {
    pub proc: Proc,
    pub ppid: i32,
    pub cwd: Option<PathBuf>,
    pub argv: String,
}

#[cfg(target_os = "macos")]
mod sys {
    use std::ffi::{c_void, CStr};
    use std::mem::{size_of, zeroed};
    use std::path::PathBuf;

    pub fn all_pids() -> Vec<i32> {
        let mut cap = 4096usize;
        loop {
            let mut buf = vec![0 as libc::pid_t; cap];
            let bytes = (cap * size_of::<libc::pid_t>()) as libc::c_int;
            // SAFETY: the buffer holds `cap` pids and its size in bytes is passed.
            let n = unsafe { libc::proc_listallpids(buf.as_mut_ptr() as *mut c_void, bytes) };
            if n < 0 {
                return Vec::new();
            }
            if (n as usize) < cap {
                buf.truncate(n as usize);
                buf.retain(|p| *p > 0);
                return buf;
            }
            cap *= 4;
        }
    }

    /// (ppid, uid) of a process.
    pub fn owner(pid: i32) -> Option<(i32, u32)> {
        // SAFETY: plain C struct of the size passed.
        let mut info: libc::proc_bsdinfo = unsafe { zeroed() };
        let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, &mut info as *mut _ as *mut c_void, size) };
        (n == size).then_some((info.pbi_ppid as i32, info.pbi_uid))
    }

    pub fn cwd(pid: i32) -> Option<PathBuf> {
        // SAFETY: plain C struct of the size passed.
        let mut info: libc::proc_vnodepathinfo = unsafe { zeroed() };
        let size = size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
        let n = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, &mut info as *mut _ as *mut c_void, size)
        };
        if n != size {
            return None;
        }
        // SAFETY: `vip_path` is a NUL-terminated MAXPATHLEN buffer, laid out as 32x32 chars.
        let raw = unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr() as *const libc::c_char) };
        let s = raw.to_str().ok()?;
        (!s.is_empty()).then(|| PathBuf::from(s))
    }

    /// The command line (`KERN_PROCARGS2`: argc, exec path, padding, then argv).
    pub fn argv(pid: i32) -> Option<String> {
        let mut max: libc::c_int = 0;
        let mut len = size_of::<libc::c_int>();
        let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
        // SAFETY: `max` is a c_int and its size is passed.
        let r = unsafe {
            libc::sysctl(mib.as_mut_ptr(), 2, &mut max as *mut _ as *mut c_void, &mut len, std::ptr::null_mut(), 0)
        };
        if r != 0 || max <= 0 {
            return None;
        }
        let mut buf = vec![0u8; max as usize];
        let mut len = buf.len();
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        // SAFETY: the buffer has `len` bytes.
        let r = unsafe {
            libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr() as *mut c_void, &mut len, std::ptr::null_mut(), 0)
        };
        if r != 0 || len < 4 {
            return None;
        }
        let argc = i32::from_ne_bytes(buf[..4].try_into().ok()?).max(0) as usize;
        let mut rest = buf[4..len].split(|b| *b == 0).skip(1).skip_while(|s| s.is_empty());
        let args: Vec<String> = rest.by_ref().take(argc).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        Some(args.join(" "))
    }

    pub fn my_uid() -> u32 {
        // SAFETY: plain syscall.
        unsafe { libc::getuid() }
    }
}

#[cfg(target_os = "linux")]
mod sys {
    use std::path::PathBuf;

    pub fn all_pids() -> Vec<i32> {
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok()).collect()
    }

    pub fn owner(pid: i32) -> Option<(i32, u32)> {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        let field = |k: &str| status.lines().find_map(|l| l.strip_prefix(k)).and_then(|v| v.split_whitespace().next());
        Some((field("PPid:")?.parse().ok()?, field("Uid:")?.parse().ok()?))
    }

    pub fn cwd(pid: i32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    pub fn argv(pid: i32) -> Option<String> {
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let parts: Vec<String> =
            raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        Some(parts.join(" "))
    }

    pub fn my_uid() -> u32 {
        // SAFETY: plain syscall.
        unsafe { libc::getuid() }
    }
}

/// `root` as given and as the kernel spells it (symlinks resolved, e.g. /tmp → /private/tmp).
fn spellings(root: &Path) -> Vec<PathBuf> {
    let mut v = vec![root.to_path_buf()];
    if let Ok(c) = std::fs::canonicalize(root) {
        if c != root {
            v.push(c);
        }
    }
    v
}

/// The working directory of `pid` (`None`: gone or unreadable).
pub fn process_cwd(pid: i32) -> Option<PathBuf> {
    sys::cwd(pid)
}

/// Processes re-parented to init (ppid 1), owned by us, whose cwd or command line points
/// into `worktree_root`.
pub fn find_orphans(worktree_root: &Path) -> Vec<Orphan> {
    let roots = spellings(worktree_root);
    let me = sys::my_uid();
    let mut out = Vec::new();
    for pid in sys::all_pids() {
        let Some((ppid, uid)) = sys::owner(pid) else { continue };
        if ppid != 1 || uid != me {
            continue;
        }
        let cwd = sys::cwd(pid);
        let argv = sys::argv(pid).unwrap_or_default();
        let by_cwd = cwd.as_ref().is_some_and(|c| roots.iter().any(|r| c.starts_with(r)));
        let by_argv = roots.iter().any(|r| argv.contains(&*r.to_string_lossy()));
        if by_cwd || by_argv {
            if let Some(proc) = proc_of(pid) {
                out.push(Orphan { proc, ppid, cwd, argv });
            }
        }
    }
    out
}

/// Ends `o` and its descendants (SIGTERM, then SIGKILL after 2 s). Re-checks first that the
/// pid still names the same process (start time); a stale entry is never signaled.
pub fn kill_orphan(o: &Orphan, s: &dyn Signaler) -> KillReport {
    if !alive(o.proc) {
        return KillReport::default();
    }
    kill_tree(o.proc, Duration::from_secs(2), s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Instant;

    use mirante_ipc::test_dir::TestDir;

    use crate::driver::proctree::Kill;

    struct Recorder(Mutex<Vec<(i32, i32, bool)>>);

    impl Signaler for Recorder {
        fn signal(&self, pid: i32, sig: i32) -> bool {
            self.0.lock().unwrap().push((pid, sig, false));
            Kill.signal(pid, sig)
        }
        fn signal_group(&self, pgid: i32, sig: i32) -> bool {
            self.0.lock().unwrap().push((pgid, sig, true));
            Kill.signal_group(pgid, sig)
        }
    }

    /// Starts `inner` detached (its parent shell exits, so it is re-parented to pid 1) and
    /// returns it once it is an orphan.
    fn detached(inner: &str, dir: &Path, name: &str) -> Proc {
        let pidfile = dir.join(name);
        let script = format!("( {inner} ) >/dev/null 2>&1 & echo $! > '{}'", pidfile.display());
        assert!(std::process::Command::new("/bin/sh").args(["-c", &script]).status().unwrap().success());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let pid = std::fs::read_to_string(&pidfile).ok().and_then(|s| s.trim().parse::<i32>().ok());
            if let Some(p) = pid.and_then(proc_of) {
                if sys::owner(p.pid).is_some_and(|(ppid, _)| ppid == 1) {
                    return p;
                }
            }
            assert!(Instant::now() < deadline, "{name} never became an orphan");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Kills what a test started, even when an assertion failed first.
    struct Cleanup(Vec<Proc>);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            for p in &self.0 {
                kill_tree(*p, Duration::from_millis(500), &Kill);
            }
        }
    }

    #[test]
    fn orphans_are_found_by_cwd_or_command_line_inside_the_worktree_root_only() {
        let d = TestDir::new();
        let root = d.path().join("worktrees");
        std::fs::create_dir_all(root.join("app/feat")).unwrap();
        let in_cwd = detached(&format!("cd '{}' && exec sleep 30", root.join("app/feat").display()), d.path(), "a");
        let in_argv =
            detached(&format!("cd / && exec sh -c 'sleep 30; :' '{}'", root.join("app/x").display()), d.path(), "b");
        let outside = detached("cd / && exec sleep 31", d.path(), "c");
        let _cleanup = Cleanup(vec![in_cwd, in_argv, outside]);
        let found = find_orphans(&root);
        let pids: Vec<i32> = found.iter().map(|o| o.proc.pid).collect();
        assert!(pids.contains(&in_cwd.pid), "{found:?}");
        assert!(pids.contains(&in_argv.pid), "{found:?}");
        assert!(!pids.contains(&outside.pid), "{found:?}");
        let by_cwd = found.iter().find(|o| o.proc.pid == in_cwd.pid).unwrap();
        assert_eq!(by_cwd.ppid, 1);
        assert!(by_cwd.cwd.as_ref().unwrap().ends_with("worktrees/app/feat"), "{by_cwd:?}");
        let by_argv = found.iter().find(|o| o.proc.pid == in_argv.pid).unwrap();
        assert!(by_argv.argv.contains("app/x"), "{by_argv:?}");
    }

    #[test]
    fn killing_an_orphan_signals_only_that_process_and_never_a_stale_one() {
        let d = TestDir::new();
        let root = d.path().join("worktrees");
        std::fs::create_dir_all(&root).unwrap();
        let p = detached(&format!("cd '{}' && exec sleep 30", root.display()), d.path(), "a");
        let _cleanup = Cleanup(vec![p]);
        let o = find_orphans(&root).into_iter().find(|o| o.proc == p).expect("found");
        // A stale entry (same pid, another start time) is never signaled.
        let stale = Orphan { proc: Proc { pid: p.pid, start: p.start.wrapping_sub(1) }, ..o.clone() };
        let rec = Recorder(Mutex::new(Vec::new()));
        assert_eq!(kill_orphan(&stale, &rec), KillReport::default());
        assert!(rec.0.lock().unwrap().is_empty());
        assert!(alive(p));
        let r = kill_orphan(&o, &rec);
        assert!(r.survivors.is_empty(), "{r:?}");
        assert!(!alive(p));
        let calls = rec.0.lock().unwrap();
        assert!(!calls.is_empty());
        assert!(calls.iter().all(|(pid, _, _)| *pid == p.pid), "{calls:?}");
    }
}
