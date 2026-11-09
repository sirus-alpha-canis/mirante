//! Finding and starting `mirante-daemon` from the desktop app. A GUI app does not inherit
//! the shell's PATH, so the binary is looked up in a fixed list, then in the login shell's
//! PATH; the daemon gets the login shell's PATH and `MIRANTE_*` ([`crate::login_env`]).
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use std::time::Duration;

use mirante_ipc::types::{method, DaemonBinary};
use mirante_ipc::Paths;

/// `MIRANTE_DAEMON_BIN`, else the first executable `mirante-daemon` in `~/.cargo/bin`,
/// `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`, then in `login_path` (the login
/// shell's PATH, when known).
pub fn find_daemon_bin(env_bin: Option<OsString>, home: Option<&Path>, login_path: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = env_bin.filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(h) = home {
        dirs.push(h.join(".cargo/bin"));
        dirs.push(h.join(".local/bin"));
    }
    dirs.push("/opt/homebrew/bin".into());
    dirs.push("/usr/local/bin".into());
    dirs.extend(std::env::split_paths(login_path.unwrap_or_default()).filter(|d| d.is_absolute()));
    dirs.into_iter().map(|d| d.join("mirante-daemon")).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The `mirante-daemon` the app would start: `MIRANTE_DAEMON_BIN` (the app's, else the
/// login shell's), else [`find_daemon_bin`].
pub fn resolve_bin() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    // Resolved once per run (up to LOGIN_ENV_TIMEOUT the first time).
    let env = crate::login_env::daemon_env();
    let login = |key: &str| env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    let env_bin = std::env::var_os("MIRANTE_DAEMON_BIN")
        .filter(|p| !p.is_empty())
        .or_else(|| login("MIRANTE_DAEMON_BIN").map(OsString::from));
    find_daemon_bin(env_bin, home.as_deref(), login("PATH")).ok_or_else(|| {
        "mirante-daemon not found: run scripts/install.sh in the Mirante checkout, or set MIRANTE_DAEMON_BIN"
            .to_string()
    })
}

/// Starts the daemon (detached, logging to `~/.mirante/daemon.log`) unless one answers.
pub fn ensure_running(paths: &Paths) -> Result<(), String> {
    if mirante_ipc::Client::connect(&paths.socket()).is_ok() {
        return Ok(());
    }
    let bin = resolve_bin()?;
    let env = crate::login_env::daemon_env();
    mirante_cli::daemon_ctl::start_daemon_with_env(paths, &bin, env).map(|_| ()).map_err(|e| e.to_string())
}

/// What the binary the app would start says it is (`--version`, no shell, at most
/// [`mirante_cli::daemon_ctl::VERSION_TIMEOUT`]); None when it cannot tell.
pub fn binary_version() -> Option<DaemonBinary> {
    let bin = resolve_bin().ok()?;
    mirante_cli::daemon_ctl::binary_version(&bin, mirante_cli::daemon_ctl::VERSION_TIMEOUT)
}

/// How long the old daemon may take to end its sessions and let go of its lock.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(15);

/// Stops the daemon that answers (`daemon.shutdown`: its live sessions end, and come back on
/// their conversations when auto-resume is on), waits for it to be gone, then `start`s one.
pub fn restart_with(paths: &Paths, start: impl FnOnce(&Paths) -> Result<(), String>) -> Result<(), String> {
    if let Ok(mut c) = mirante_ipc::Client::connect(&paths.socket()) {
        c.set_timeout(Some(Duration::from_secs(60))).map_err(|e| e.to_string())?;
        let _: serde_json::Value = c
            .call(method::DAEMON_SHUTDOWN, serde_json::json!({}))
            .map_err(|e| format!("could not stop the daemon: {e}"))?;
        mirante_cli::daemon_ctl::wait_stopped(paths, STOP_TIMEOUT).map_err(|e| e.to_string())?;
    }
    start(paths)
}

/// [`restart_with`] the daemon the app starts ([`ensure_running`]).
pub fn restart(paths: &Paths) -> Result<(), String> {
    restart_with(paths, ensure_running)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_ipc::test_dir::TestDir;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn restart_stops_the_daemon_that_answers_then_starts_one() {
        let d = TestDir::new();
        let paths = Paths::new(d.path());
        paths.ensure_home().unwrap();
        let fake = crate::fake::FakeDaemon::start(&paths.socket());
        let started = std::sync::atomic::AtomicBool::new(false);
        restart_with(&paths, |p| {
            // The old one is gone by then.
            assert!(mirante_ipc::Client::connect(&p.socket()).is_err());
            started.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
        assert_eq!(fake.methods(), vec![method::DAEMON_SHUTDOWN.to_string()]);
        assert!(started.load(std::sync::atomic::Ordering::SeqCst));
        // Nothing running: it only starts one.
        let mut again = false;
        restart_with(&paths, |_| {
            again = true;
            Ok(())
        })
        .unwrap();
        assert!(again);
        assert_eq!(fake.methods().len(), 1);
    }

    #[test]
    fn env_wins_then_cargo_bin_when_executable() {
        let d = TestDir::new();
        assert_eq!(find_daemon_bin(Some("/x/daemon".into()), Some(d.path()), None), Some(PathBuf::from("/x/daemon")));
        let bin = d.path().join(".cargo/bin/mirante-daemon");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_ne!(find_daemon_bin(Some("".into()), Some(d.path()), None), Some(bin.clone()));
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(find_daemon_bin(None, Some(d.path()), None), Some(bin));
    }

    fn exe(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn local_bin_and_the_login_path_are_searched() {
        let d = TestDir::new();
        let local = d.path().join(".local/bin/mirante-daemon");
        exe(&local);
        assert_eq!(find_daemon_bin(None, Some(d.path()), None), Some(local.clone()));
        std::fs::remove_file(&local).unwrap();
        let custom = d.path().join("tools/bin/mirante-daemon");
        exe(&custom);
        let login = format!("relative/dir:{}", custom.parent().unwrap().display());
        // Skipped when /opt/homebrew or /usr/local holds one on this machine.
        let found = find_daemon_bin(None, Some(d.path()), Some(&login));
        if !Path::new("/opt/homebrew/bin/mirante-daemon").exists()
            && !Path::new("/usr/local/bin/mirante-daemon").exists()
        {
            assert_eq!(found, Some(custom));
        }
    }
}
