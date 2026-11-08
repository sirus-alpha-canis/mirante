//! Connecting to the daemon, starting it on first use.
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use mirante_core::claude_launch::daemon_env_to_scrub;
use mirante_ipc::types::DaemonStatus;
use mirante_ipc::{logfile, Client, Paths};

/// How long `start_daemon` waits for the socket.
pub const START_TIMEOUT: Duration = Duration::from_secs(5);

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
/// `~/.mirante/daemon.log` (rotated to `daemon.log.1` past 10 MiB), and waits up to 5 s for
/// its socket.
pub fn start_daemon(paths: &Paths) -> io::Result<Client> {
    start_daemon_with(paths, &daemon_bin()?)
}

/// [`start_daemon`] with an explicit daemon binary (the desktop app is not installed next to
/// `mirante-daemon`, so it finds the binary itself).
pub fn start_daemon_with(paths: &Paths, bin: &Path) -> io::Result<Client> {
    start_daemon_with_env(paths, bin, &[])
}

/// [`start_daemon_with`] with extra variables for the daemon (see [`daemon_command_with_env`]).
pub fn start_daemon_with_env(paths: &Paths, bin: &Path, extra: &[(String, String)]) -> io::Result<Client> {
    paths.ensure_home()?;
    let _ = logfile::rotate_if_over(&paths.log(), logfile::LOG_CAP);
    let log = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(paths.log())?;
    let mut cmd = daemon_command_with_env(bin, paths, env_names(), extra);
    cmd.stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    // SAFETY: setsid is async-signal-safe and touches no Rust state.
    unsafe {
        cmd.pre_exec(|| if libc::setsid() == -1 { Err(io::Error::last_os_error()) } else { Ok(()) });
    }
    cmd.spawn()?;
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        match Client::connect(&paths.socket()) {
            Ok(c) => return Ok(c),
            Err(e) if Instant::now() >= deadline => {
                return Err(io::Error::new(
                    e.kind(),
                    format!(
                        "the daemon did not answer within {} s; it may still be starting (try again shortly, \
                         or see {}): {e}",
                        START_TIMEOUT.as_secs(),
                        paths.log().display()
                    ),
                ))
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// A client, starting the daemon if nothing listens on the socket.
pub fn connect(paths: &Paths) -> io::Result<Client> {
    match Client::connect(&paths.socket()) {
        Ok(c) => Ok(c),
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => start_daemon(paths),
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
        };
        assert_eq!(version_warning(&st("0.1.0", 0), "0.1.0"), None);
        let idle = version_warning(&st("0.0.9", 0), "0.1.0").unwrap();
        assert!(idle.contains("v0.0.9") && idle.contains("v0.1.0") && idle.contains("mirante daemon restart"));
        let busy = version_warning(&st("0.0.9", 2), "0.1.0").unwrap();
        assert!(busy.contains("2 live session(s)") && busy.contains("when they are done"));
        assert!(busy.contains("resumes them on their conversations when auto-resume is on"), "{busy}");
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
