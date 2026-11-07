//! Finding and starting `mirante-daemon` from the desktop app. A GUI app does not inherit
//! the shell's PATH, so the binary is looked up in a fixed list.
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use mirante_ipc::Paths;

/// `MIRANTE_DAEMON_BIN`, else the first executable `mirante-daemon` in `~/.cargo/bin`,
/// `/opt/homebrew/bin`, `/usr/local/bin`.
pub fn find_daemon_bin(env_bin: Option<OsString>, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = env_bin.filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(h) = home {
        dirs.push(h.join(".cargo/bin"));
    }
    dirs.push("/opt/homebrew/bin".into());
    dirs.push("/usr/local/bin".into());
    dirs.into_iter().map(|d| d.join("mirante-daemon")).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Starts the daemon (detached, logging to `~/.mirante/daemon.log`) unless one answers.
pub fn ensure_running(paths: &Paths) -> Result<(), String> {
    if mirante_ipc::Client::connect(&paths.socket()).is_ok() {
        return Ok(());
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let bin = find_daemon_bin(std::env::var_os("MIRANTE_DAEMON_BIN"), home.as_deref()).ok_or_else(|| {
        "mirante-daemon not found: install it (cargo install --path crates/mirante-daemon) or set MIRANTE_DAEMON_BIN"
            .to_string()
    })?;
    mirante_cli::daemon_ctl::start_daemon_with(paths, &bin).map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_ipc::test_dir::TestDir;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn env_wins_then_cargo_bin_when_executable() {
        let d = TestDir::new();
        assert_eq!(find_daemon_bin(Some("/x/daemon".into()), Some(d.path())), Some(PathBuf::from("/x/daemon")));
        let bin = d.path().join(".cargo/bin/mirante-daemon");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_ne!(find_daemon_bin(Some("".into()), Some(d.path())), Some(bin.clone()));
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(find_daemon_bin(None, Some(d.path())), Some(bin));
    }
}
