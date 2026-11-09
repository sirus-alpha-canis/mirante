//! Connecting to the daemon, starting it on first use.
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use mirante_core::claude_launch::daemon_env_to_scrub;
use mirante_ipc::types::{DaemonBinary, DaemonStatus};
use mirante_ipc::{logfile, Client, Paths};

/// How long `start_daemon` waits for the socket. The first launch of a freshly built
/// binary can take 10-20 s on macOS while XProtect scans it.
pub const START_TIMEOUT: Duration = Duration::from_secs(30);

/// Said (once) when the daemon has not answered after [`NOTICE_AFTER`].
pub const SLOW_START_NOTICE: &str =
    "starting the daemon (the first launch after a build can take ~20 s while macOS scans it)…";

/// Said when another command holds the start lock.
pub const WAITING_NOTICE: &str = "waiting for the daemon another mirante command is starting…";

const NOTICE_AFTER: Duration = Duration::from_secs(1);

/// `MIRANTE_DAEMON_BIN`, else `mirante-daemon` next to this executable.
pub fn daemon_bin() -> io::Result<PathBuf> {
    if let Some(p) = std::env::var_os("MIRANTE_DAEMON_BIN").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    Ok(std::env::current_exe()?.with_file_name("mirante-daemon"))
}

/// The daemon's command line, environment included: `MIRANTE_HOME` set, and the variables
/// of the terminal it is started from scrubbed (it outlives that terminal and every agent
/// inherits its env): nested Claude session ids, cmux/tmux/VS Code integration,
/// `TERM_PROGRAM*`. Credentials (`ANTHROPIC_*`, `OPENAI_*`, `CODEX_*`, Claude OAuth,
/// `CLAUDE_CONFIG_DIR`) are never removed. `present` is this process's variable names.
pub fn daemon_command(bin: &Path, paths: &Paths, present: Vec<String>) -> Command {
    daemon_command_with_env(bin, paths, present, &[])
}

/// [`daemon_command`] plus `extra` variables (the desktop app passes PATH and `MIRANTE_*`
/// from the user's login shell). An extra never re-adds a scrubbed variable and never moves
/// the daemon to another `MIRANTE_HOME` than `paths`.
pub fn daemon_command_with_env(bin: &Path, paths: &Paths, present: Vec<String>, extra: &[(String, String)]) -> Command {
    let mut cmd = Command::new(bin);
    let scrubbed = daemon_env_to_scrub(present);
    for k in &scrubbed {
        cmd.env_remove(k);
    }
    for (k, v) in extra {
        if !scrubbed.contains(k) && k != mirante_ipc::paths::HOME_ENV {
            cmd.env(k, v);
        }
    }
    cmd.env(mirante_ipc::paths::HOME_ENV, paths.home());
    cmd
}

fn env_names() -> Vec<String> {
    std::env::vars_os().filter_map(|(k, _)| k.into_string().ok()).collect()
}

/// Spawns the daemon in its own session (it survives this terminal), logging to
/// `~/.mirante/daemon.log` (rotated to `daemon.log.1` past 10 MiB), and waits up to
/// [`START_TIMEOUT`] for its socket. Quiet; see [`start_daemon_with_notice`].
pub fn start_daemon(paths: &Paths) -> io::Result<Client> {
    start_daemon_with(paths, &daemon_bin()?)
}

/// [`start_daemon`], saying on stderr-like `notice` what it waits for when that takes long
/// (the CLI prints these lines).
pub fn start_daemon_with_notice(paths: &Paths, notice: &dyn Fn(&str)) -> io::Result<Client> {
    start_daemon_inner(paths, &daemon_bin()?, &[], START_TIMEOUT, notice)
}

/// [`start_daemon`] with an explicit daemon binary (the desktop app is not installed next to
/// `mirante-daemon`, so it finds the binary itself).
pub fn start_daemon_with(paths: &Paths, bin: &Path) -> io::Result<Client> {
    start_daemon_with_env(paths, bin, &[])
}

/// [`start_daemon_with`] with extra variables for the daemon (see [`daemon_command_with_env`]).
pub fn start_daemon_with_env(paths: &Paths, bin: &Path, extra: &[(String, String)]) -> io::Result<Client> {
    start_daemon_inner(paths, bin, extra, START_TIMEOUT, &|_| {})
}

/// An exclusive `flock` on [`Paths::start_lock`], released on drop.
struct StartLock {
    _file: std::fs::File,
}

impl StartLock {
    /// Waits for the lock until `deadline`, saying [`WAITING_NOTICE`] once when it is taken.
    fn acquire(paths: &Paths, deadline: Instant, notice: &dyn Fn(&str)) -> io::Result<StartLock> {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(paths.start_lock())?;
        let mut said = false;
        loop {
            // SAFETY: valid fd owned by `f`; the lock is released when `f` closes.
            if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(StartLock { _file: f });
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("another command is still starting the daemon; see {}", paths.log().display()),
                ));
            }
            if !said {
                notice(WAITING_NOTICE);
                said = true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// The last `n` non-empty lines of the daemon log (best effort).
fn log_tail(paths: &Paths, n: usize) -> String {
    let text = std::fs::read_to_string(paths.log()).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// One starter at a time (the start lock); a command that finds a daemon answering once it
/// holds the lock uses it. While waiting: a notice after [`NOTICE_AFTER`]; a daemon that
/// exits is reported with the end of its log (unless it lost the daemon lock to a daemon
/// started some other way, whose socket is then awaited); the timeout names the log, not a
/// raw "socket not found".
pub(crate) fn start_daemon_inner(
    paths: &Paths,
    bin: &Path,
    extra: &[(String, String)],
    timeout: Duration,
    notice: &dyn Fn(&str),
) -> io::Result<Client> {
    paths.ensure_home()?;
    let started = Instant::now();
    let deadline = started + timeout;
    let _starter = StartLock::acquire(paths, deadline, notice)?;
    if let Ok(c) = Client::connect(&paths.socket()) {
        return Ok(c);
    }
    let _ = logfile::rotate_if_over(&paths.log(), logfile::LOG_CAP);
    let log = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(paths.log())?;
    let mut cmd = daemon_command_with_env(bin, paths, env_names(), extra);
    cmd.stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    // SAFETY: setsid is async-signal-safe and touches no Rust state.
    unsafe {
        cmd.pre_exec(|| if libc::setsid() == -1 { Err(io::Error::last_os_error()) } else { Ok(()) });
    }
    let mut child = cmd.spawn().map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", bin.display())))?;
    let mut noticed = false;
    let mut exited = false;
    loop {
        let err = match Client::connect(&paths.socket()) {
            Ok(c) => return Ok(c),
            Err(e) => e,
        };
        if !exited {
            if let Some(status) = child.try_wait()? {
                let tail = log_tail(paths, 5);
                if !tail.contains("another mirante-daemon is running") {
                    return Err(io::Error::other(format!(
                        "the daemon exited during startup ({status}); the end of {}:\n{tail}",
                        paths.log().display()
                    )));
                }
                exited = true;
            }
        }
        let now = Instant::now();
        if now >= deadline {
            // A missing or refusing socket is the expected "not up yet"; anything else is news.
            let why = match err.kind() {
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => String::new(),
                _ => format!(" ({err})"),
            };
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "the daemon did not answer within {} s{why}; it may still be starting: try again shortly, \
                     or see {}",
                    timeout.as_secs(),
                    paths.log().display()
                ),
            ));
        }
        if !noticed && now - started >= NOTICE_AFTER {
            notice(SLOW_START_NOTICE);
            noticed = true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A client, starting the daemon if nothing listens on the socket.
pub fn connect(paths: &Paths) -> io::Result<Client> {
    connect_with_notice(paths, &|_| {})
}

/// [`connect`], passing `notice` to [`start_daemon_with_notice`] when it starts one.
pub fn connect_with_notice(paths: &Paths, notice: &dyn Fn(&str)) -> io::Result<Client> {
    match Client::connect(&paths.socket()) {
        Ok(c) => Ok(c),
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => {
            start_daemon_with_notice(paths, notice)
        }
        Err(e) => Err(e),
    }
}

/// Waits until the daemon of `paths` is gone: nothing answers on the socket and its lock is
/// free (so a new daemon can take it).
pub fn wait_stopped(paths: &Paths, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if Client::connect(&paths.socket()).is_err() && lock_is_free(&paths.lock()) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "the old daemon is still running"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn lock_is_free(path: &Path) -> bool {
    let Ok(f) = std::fs::OpenOptions::new().read(true).open(path) else {
        return true;
    };
    // SAFETY: valid fd owned by `f`; the lock (if taken) is released when `f` closes.
    unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

/// A warning when the running daemon is another version than this CLI (after an upgrade
/// the old daemon keeps running until restarted).
pub fn version_warning(daemon: &DaemonStatus, mine: &str) -> Option<String> {
    if daemon.version == mine {
        return None;
    }
    let head = format!("mirante: the daemon runs v{} but this CLI is v{mine}", daemon.version);
    Some(match daemon.live_sessions {
        0 => format!("{head}; no sessions are live: run `mirante daemon restart`"),
        n => format!(
            "{head}; {n} live session(s): run `mirante daemon restart` when they are done \
             (it ends them; the new daemon resumes them on their conversations when auto-resume \
             is on, else `mirante resume <task>` brings them back)"
        ),
    })
}

/// How long `mirante-daemon --version` may take (a first launch after a build can be slow
/// while macOS scans it).
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// `mirante-daemon 0.1.0 (build <id>)`, or `mirante-daemon 0.1.0` from a binary that
/// predates build ids.
pub fn parse_daemon_version(out: &str) -> Option<DaemonBinary> {
    let line = out.lines().next()?.trim();
    let rest = line.strip_prefix("mirante-daemon ")?;
    let (version, build) = match rest.split_once(" (build ") {
        Some((v, b)) => (v, Some(b.strip_suffix(')')?.trim().to_string())),
        None => (rest, None),
    };
    let version = version.trim();
    if version.is_empty() || version.contains(' ') {
        return None;
    }
    Some(DaemonBinary { version: version.to_string(), build: build.filter(|b| !b.is_empty()) })
}

/// Runs `bin --version` (no shell, no stdin, at most `timeout`) and reads what it says.
/// None when it cannot be run, is too slow, fails, or says something else.
pub fn binary_version(bin: &Path, timeout: Duration) -> Option<DaemonBinary> {
    use std::io::Read;
    let mut child = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    parse_daemon_version(&out)
}

/// The running daemon is older than the binary on disk: the binary names a build and the
/// daemon another one (or none: it predates build ids).
pub fn daemon_outdated(daemon: &DaemonStatus, disk: &DaemonBinary) -> bool {
    match &disk.build {
        Some(b) => daemon.build.as_deref() != Some(b.as_str()),
        None => false,
    }
}

/// `mirante doctor`'s line when the running daemon is older than `bin`.
pub fn outdated_line(daemon: &DaemonStatus, bin: &Path, disk: &DaemonBinary) -> Option<String> {
    if !daemon_outdated(daemon, disk) {
        return None;
    }
    let running = match &daemon.build {
        Some(b) => format!("build {b}"),
        None => format!("v{}, which predates build ids", daemon.version),
    };
    Some(format!(
        "daemon: the running daemon ({running}) is older than {} (build {}); restart with: mirante daemon restart\n",
        bin.display(),
        disk.build.as_deref().unwrap_or("?"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_ipc::test_dir::TestDir;
    use std::ffi::OsStr;

    /// `cmd` will not pass `key` on.
    fn removes(cmd: &Command, key: &str) -> bool {
        cmd.get_envs().any(|(k, v)| k == OsStr::new(key) && v.is_none())
    }

    fn sets<'a>(cmd: &'a Command, key: &str) -> Option<&'a OsStr> {
        cmd.get_envs().find(|(k, _)| *k == OsStr::new(key)).and_then(|(_, v)| v)
    }

    #[test]
    fn extra_env_is_set_but_never_overrides_the_home_or_a_scrubbed_variable() {
        let d = TestDir::new();
        let paths = Paths::new(d.path());
        let extra = [
            ("PATH", "/Users/me/.local/bin:/usr/bin"),
            ("MIRANTE_CODEX_BIN", "/x/codex"),
            ("MIRANTE_HOME", "/somewhere/else"),
            ("CLAUDECODE", "1"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()));
        let cmd = daemon_command_with_env(Path::new("/bin/true"), &paths, vec!["CLAUDECODE".into()], &extra);
        assert_eq!(sets(&cmd, "PATH"), Some(OsStr::new("/Users/me/.local/bin:/usr/bin")));
        assert_eq!(sets(&cmd, "MIRANTE_CODEX_BIN"), Some(OsStr::new("/x/codex")));
        assert_eq!(sets(&cmd, "MIRANTE_HOME"), Some(paths.home().as_os_str()));
        assert!(removes(&cmd, "CLAUDECODE"));
    }

    #[test]
    fn the_daemon_starts_with_a_scrubbed_env_but_keeps_credentials() {
        let d = TestDir::new();
        let paths = Paths::new(d.path());
        let present = [
            "CLAUDECODE",
            "CLAUDE_CODE_SESSION_ID",
            "CMUX_CLAUDE_PID",
            "CMUX_AGENT_X",
            "TMUX",
            "TMUX_PANE",
            "TERM_PROGRAM",
            "TERM_PROGRAM_VERSION",
            "VSCODE_PID",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "CODEX_HOME",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CONFIG_DIR",
            "PATH",
        ]
        .map(String::from)
        .to_vec();
        let cmd = daemon_command(Path::new("/bin/true"), &paths, present);
        for k in [
            "CLAUDECODE",
            "CLAUDE_CODE_SESSION_ID",
            "CMUX_CLAUDE_PID",
            "CMUX_AGENT_X",
            "TMUX",
            "TMUX_PANE",
            "TERM_PROGRAM",
            "TERM_PROGRAM_VERSION",
            "VSCODE_PID",
        ] {
            assert!(removes(&cmd, k), "{k} must be scrubbed");
        }
        for k in [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "CODEX_HOME",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CONFIG_DIR",
            "PATH",
        ] {
            assert!(!cmd.get_envs().any(|(n, _)| n == OsStr::new(k)), "{k} must be kept");
        }
        let home = cmd.get_envs().find(|(k, _)| *k == OsStr::new("MIRANTE_HOME")).and_then(|(_, v)| v);
        assert_eq!(home, Some(d.path().as_os_str()));
    }

    #[test]
    fn version_skew_warns_and_offers_a_restart_only_when_idle() {
        let st = |version: &str, live| DaemonStatus {
            pid: 1,
            boot_id: "b".into(),
            version: version.into(),
            live_sessions: live,
            memory: None,
            claude_inline_answers: false,
            auto_resume: None,
            auto_resume_forced_off: None,
            pause_new_sessions_under_pressure: None,
            pause_forced_by_env: None,
            build: None,
        };
        assert_eq!(version_warning(&st("0.1.0", 0), "0.1.0"), None);
        let idle = version_warning(&st("0.0.9", 0), "0.1.0").unwrap();
        assert!(idle.contains("v0.0.9") && idle.contains("v0.1.0") && idle.contains("mirante daemon restart"));
        let busy = version_warning(&st("0.0.9", 2), "0.1.0").unwrap();
        assert!(busy.contains("2 live session(s)") && busy.contains("when they are done"));
        assert!(busy.contains("resumes them on their conversations when auto-resume is on"), "{busy}");
    }

    fn status_with_build(build: Option<&str>) -> DaemonStatus {
        DaemonStatus {
            pid: 1,
            boot_id: "b".into(),
            version: "0.1.0".into(),
            live_sessions: 0,
            memory: None,
            claude_inline_answers: false,
            auto_resume: None,
            auto_resume_forced_off: None,
            pause_new_sessions_under_pressure: None,
            pause_forced_by_env: None,
            build: build.map(String::from),
        }
    }

    #[test]
    fn the_version_line_gives_the_version_and_the_build() {
        let b = parse_daemon_version("mirante-daemon 0.1.0 (build 3acbd53aa1b2-dirty.1762700000)\n").unwrap();
        assert_eq!(b, DaemonBinary { version: "0.1.0".into(), build: Some("3acbd53aa1b2-dirty.1762700000".into()) });
        // An older binary says no build.
        assert_eq!(
            parse_daemon_version("mirante-daemon 0.0.9\n").unwrap(),
            DaemonBinary { version: "0.0.9".into(), build: None }
        );
        assert_eq!(parse_daemon_version("something else"), None);
        assert_eq!(parse_daemon_version(""), None);
    }

    #[test]
    fn a_daemon_is_outdated_only_when_the_binary_names_another_build() {
        let disk = |b: Option<&str>| DaemonBinary { version: "0.1.0".into(), build: b.map(String::from) };
        assert!(!daemon_outdated(&status_with_build(Some("a.1")), &disk(Some("a.1"))));
        assert!(daemon_outdated(&status_with_build(Some("a.1")), &disk(Some("b.2"))));
        // A daemon that predates build ids, with a binary that has one: older.
        assert!(daemon_outdated(&status_with_build(None), &disk(Some("b.2"))));
        // A binary without a build id says nothing either way.
        assert!(!daemon_outdated(&status_with_build(Some("a.1")), &disk(None)));
        assert!(!daemon_outdated(&status_with_build(None), &disk(None)));
    }

    #[test]
    fn the_doctor_line_says_how_to_restart() {
        let disk = DaemonBinary { version: "0.1.0".into(), build: Some("b.2".into()) };
        let bin = Path::new("/x/mirante-daemon");
        let line = outdated_line(&status_with_build(Some("a.1")), bin, &disk).unwrap();
        assert!(line.contains("a.1") && line.contains("b.2") && line.contains("/x/mirante-daemon"), "{line}");
        assert!(line.contains("restart with: mirante daemon restart"), "{line}");
        let old = outdated_line(&status_with_build(None), bin, &disk).unwrap();
        assert!(old.contains("predates build ids"), "{old}");
        assert_eq!(outdated_line(&status_with_build(Some("b.2")), bin, &disk), None);
    }

    #[test]
    fn the_binary_is_asked_its_version_without_a_shell_and_with_a_time_limit() {
        let d = TestDir::new();
        let p = d.path().join("daemon.sh");
        mirante_core::test_script::write_sh(
            &p,
            "[ \"$1\" = --version ] && echo 'mirante-daemon 0.2.0 (build c.3)'\n",
            0o755,
        );
        assert_eq!(
            binary_version(&p, Duration::from_secs(10)),
            Some(DaemonBinary { version: "0.2.0".into(), build: Some("c.3".into()) })
        );
        let slow = d.path().join("slow.sh");
        mirante_core::test_script::write_sh(&slow, "sleep 5\necho 'mirante-daemon 0.2.0 (build c.3)'\n", 0o755);
        let started = Instant::now();
        assert_eq!(binary_version(&slow, Duration::from_millis(300)), None);
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(binary_version(&d.path().join("missing"), Duration::from_secs(1)), None);
    }

    /// A stand-in daemon: a shell script that appends a line to `<home>/launches` and then
    /// runs `rest`.
    fn script(d: &TestDir, rest: &str) -> PathBuf {
        let p = d.path().join("fake-daemon.sh");
        let body = format!("echo launched >> \"$MIRANTE_HOME/launches\"\n{rest}\n");
        mirante_core::test_script::write_sh(&p, &body, 0o755);
        p
    }

    fn launches(paths: &Paths) -> usize {
        std::fs::read_to_string(paths.home().join("launches")).unwrap_or_default().lines().count()
    }

    #[test]
    fn a_daemon_that_exits_at_start_says_why_without_os_error_2() {
        let d = TestDir::new();
        let paths = Paths::new(&d.path().join("h"));
        let bin = script(&d, "echo 'mirante-daemon: the store is locked by a newer version' >&2; exit 1");
        let e = start_daemon_inner(&paths, &bin, &[], Duration::from_secs(10), &|_| {}).err().unwrap().to_string();
        assert!(e.contains("exited") && e.contains("the store is locked by a newer version"), "{e}");
        assert!(!e.contains("os error"), "{e}");
    }

    #[test]
    fn a_slow_daemon_times_out_with_a_plain_message_and_a_notice() {
        let d = TestDir::new();
        let paths = Paths::new(&d.path().join("h"));
        let bin = script(&d, "sleep 5");
        let seen = std::sync::Mutex::new(Vec::new());
        let notice = |m: &str| seen.lock().unwrap().push(m.to_string());
        let e = start_daemon_inner(&paths, &bin, &[], Duration::from_millis(1500), &notice).err().unwrap();
        let msg = e.to_string();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert!(msg.contains("did not answer within") && msg.contains("daemon.log"), "{msg}");
        assert!(!msg.contains("os error"), "{msg}");
        assert!(seen.lock().unwrap().iter().any(|m| m.contains("macOS scans it")), "{seen:?}");
    }

    #[test]
    fn concurrent_starts_launch_one_daemon() {
        let d = TestDir::new();
        let paths = Paths::new(&d.path().join("h"));
        paths.ensure_home().unwrap();
        // The script only records the launch; the test plays the daemon and binds the
        // socket once the first launch is recorded.
        let bin = script(&d, "sleep 3");
        let binder = {
            let paths = paths.clone();
            std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(10);
                while launches(&paths) == 0 && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                std::thread::sleep(Duration::from_millis(300));
                std::os::unix::net::UnixListener::bind(paths.socket()).unwrap()
            })
        };
        let starters: Vec<_> = (0..3)
            .map(|_| {
                let (paths, bin) = (paths.clone(), bin.clone());
                std::thread::spawn(move || start_daemon_inner(&paths, &bin, &[], Duration::from_secs(10), &|_| {}))
            })
            .collect();
        for s in starters {
            s.join().unwrap().unwrap();
        }
        let _listener = binder.join().unwrap();
        assert_eq!(launches(&paths), 1);
    }

    #[test]
    fn a_free_lock_and_no_socket_mean_stopped() {
        let d = TestDir::new();
        let paths = Paths::new(d.path());
        wait_stopped(&paths, Duration::from_millis(100)).unwrap();
        let held = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(paths.lock()).unwrap();
        // SAFETY: valid fd owned by `held`.
        assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
        assert!(wait_stopped(&paths, Duration::from_millis(100)).is_err());
        drop(held);
        wait_stopped(&paths, Duration::from_millis(100)).unwrap();
    }
}
