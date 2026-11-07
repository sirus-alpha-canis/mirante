//! On-disk layout. Everything lives under one directory (mode 0700): `$MIRANTE_HOME`, else
//! `$HOME/.mirante`. Tests point `MIRANTE_HOME` at a temp dir.
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Overrides the home directory (also passed to every spawned agent, so its hooks find
/// the same daemon).
pub const HOME_ENV: &str = "MIRANTE_HOME";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    home: PathBuf,
}

impl Paths {
    pub fn new(home: &Path) -> Paths {
        Paths { home: home.to_path_buf() }
    }

    /// `$MIRANTE_HOME`, else `$HOME/.mirante`. Fails when neither is set: never falls back
    /// to the current directory.
    pub fn from_env() -> io::Result<Paths> {
        Paths::resolve(std::env::var_os(HOME_ENV), std::env::var_os("HOME"))
    }

    pub fn resolve(mirante_home: Option<OsString>, home: Option<OsString>) -> io::Result<Paths> {
        if let Some(h) = mirante_home.filter(|h| !h.is_empty()) {
            return Ok(Paths::new(Path::new(&h)));
        }
        match home.filter(|h| !h.is_empty()) {
            Some(h) => Ok(Paths::new(&Path::new(&h).join(".mirante"))),
            None => Err(io::Error::new(io::ErrorKind::NotFound, "neither MIRANTE_HOME nor HOME is set")),
        }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn socket(&self) -> PathBuf {
        self.home.join("daemon.sock")
    }

    pub fn database(&self) -> PathBuf {
        self.home.join("mirante.sqlite")
    }

    /// Hook payloads that could not reach the daemon; ingested on the next daemon start.
    pub fn spool(&self) -> PathBuf {
        self.home.join("hook-spool.jsonl")
    }

    pub fn worktrees(&self) -> PathBuf {
        self.home.join("worktrees")
    }

    /// Per-task scratch: the generated Claude settings file, agent stderr logs.
    pub fn session_dir(&self, task_id: &str) -> PathBuf {
        self.home.join("sessions").join(task_id)
    }

    pub fn log(&self) -> PathBuf {
        self.home.join("daemon.log")
    }

    /// `flock`ed by the running daemon: one daemon per home.
    pub fn lock(&self) -> PathBuf {
        self.home.join("daemon.lock")
    }

    /// Creates the home directory with mode 0700, and tightens an existing one.
    pub fn ensure_home(&self) -> io::Result<()> {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.home)?;
        std::fs::set_permissions(&self.home, std::fs::Permissions::from_mode(0o700))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_dir::TestDir;

    #[test]
    fn mirante_home_wins_then_home_dot_mirante_and_never_cwd() {
        let p = Paths::resolve(Some("/x/m".into()), Some("/home/u".into())).unwrap();
        assert_eq!(p.socket(), PathBuf::from("/x/m/daemon.sock"));
        let p = Paths::resolve(None, Some("/home/u".into())).unwrap();
        assert_eq!(p.database(), PathBuf::from("/home/u/.mirante/mirante.sqlite"));
        let p = Paths::resolve(Some("".into()), Some("/home/u".into())).unwrap();
        assert_eq!(p.home(), Path::new("/home/u/.mirante"));
        assert!(Paths::resolve(None, None).is_err());
        assert!(Paths::resolve(Some("".into()), Some("".into())).is_err());
    }

    #[test]
    fn layout_is_under_home() {
        let p = Paths::new(Path::new("/h"));
        assert_eq!(p.spool(), PathBuf::from("/h/hook-spool.jsonl"));
        assert_eq!(p.worktrees(), PathBuf::from("/h/worktrees"));
        assert_eq!(p.session_dir("t1"), PathBuf::from("/h/sessions/t1"));
        assert_eq!(p.log(), PathBuf::from("/h/daemon.log"));
        assert_eq!(p.lock(), PathBuf::from("/h/daemon.lock"));
    }

    #[test]
    fn ensure_home_creates_a_private_directory() {
        let d = TestDir::new();
        let p = Paths::new(&d.path().join("home"));
        p.ensure_home().unwrap();
        let mode = std::fs::metadata(p.home()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        std::fs::set_permissions(p.home(), std::fs::Permissions::from_mode(0o755)).unwrap();
        p.ensure_home().unwrap();
        assert_eq!(std::fs::metadata(p.home()).unwrap().permissions().mode() & 0o777, 0o700);
    }
}
