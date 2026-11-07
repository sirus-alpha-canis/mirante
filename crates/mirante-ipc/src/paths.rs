//! On-disk layout. Everything lives under one directory (mode 0700): `$MIRANTE_HOME`, else
//! `$HOME/.mirante`. Tests point `MIRANTE_HOME` at a temp dir.
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
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

    /// Touched by the hook after a daemon timeout; while fresh, hooks spool without
    /// trying the socket.
    pub fn hook_breaker(&self) -> PathBuf {
        self.home.join("hook-breaker")
    }

    /// Appended to by the hook whenever an event did not reach the daemon live (spooled,
    /// breaker tripped, or lost). The daemon refuses to type a Claude answer when it changed
    /// since the card was created (see [`crate::hookloss`]).
    pub fn hook_lost(&self) -> PathBuf {
        self.home.join("hook-lost")
    }

    /// Explicit error when the socket path would not fit `sockaddr_un`.
    pub fn validate_socket(&self) -> io::Result<()> {
        validate_socket_path(&self.socket())
    }

    /// Makes sure the home directory exists. A missing directory is created with mode 0700;
    /// an existing one is left alone (never chmod'ed) but must be a real directory (not a
    /// symlink) owned by the current user, else this fails. Also validates the socket path.
    pub fn ensure_home(&self) -> io::Result<()> {
        self.validate_socket()?;
        match std::fs::symlink_metadata(&self.home) {
            Ok(m) => check_home(&self.home, &m)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if let Some(parent) = self.home.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent)?;
                }
                match std::fs::DirBuilder::new().mode(0o700).create(&self.home) {
                    Ok(()) => {}
                    // Lost a race: validate whatever is there now.
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                        check_home(&self.home, &std::fs::symlink_metadata(&self.home)?)?
                    }
                    Err(e) => return Err(e),
                }
            }
            Err(e) => return Err(e),
        }
        Ok(())
    }
}

fn check_home(home: &Path, m: &std::fs::Metadata) -> io::Result<()> {
    if m.file_type().is_symlink() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{} is a symlink", home.display())));
    }
    if !m.is_dir() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} is not a directory", home.display())));
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    if m.uid() != me {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is owned by uid {}, not {me}", home.display(), m.uid()),
        ));
    }
    Ok(())
}

/// `sockaddr_un.sun_path` is 104 bytes on macOS and 108 on Linux, NUL included.
#[cfg(target_os = "macos")]
pub const SOCKET_PATH_LIMIT: usize = 104;
#[cfg(not(target_os = "macos"))]
pub const SOCKET_PATH_LIMIT: usize = 108;

pub fn validate_socket_path(p: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let n = p.as_os_str().as_bytes().len();
    if n >= SOCKET_PATH_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "socket path is {n} bytes, limit is {}: set MIRANTE_HOME to a shorter directory",
                SOCKET_PATH_LIMIT - 1
            ),
        ));
    }
    Ok(())
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
    fn ensure_home_creates_a_private_directory_but_never_chmods_existing_ones() {
        use std::os::unix::fs::PermissionsExt;
        let d = TestDir::new();
        let p = Paths::new(&d.path().join("home"));
        p.ensure_home().unwrap();
        assert_eq!(std::fs::metadata(p.home()).unwrap().permissions().mode() & 0o777, 0o700);
        std::fs::set_permissions(p.home(), std::fs::Permissions::from_mode(0o755)).unwrap();
        p.ensure_home().unwrap();
        assert_eq!(std::fs::metadata(p.home()).unwrap().permissions().mode() & 0o777, 0o755);
    }

    #[test]
    fn ensure_home_refuses_a_symlink_or_a_file() {
        let d = TestDir::new();
        let real = d.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(Paths::new(&link).ensure_home().unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        let file = d.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        assert!(Paths::new(&file).ensure_home().is_err());
    }

    #[test]
    fn ensure_home_refuses_a_directory_owned_by_someone_else() {
        // /var/root-owned paths: "/" is owned by root, which is not us unless we are root.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let err = Paths::new(Path::new("/")).ensure_home().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn overlong_socket_paths_are_an_explicit_error() {
        let long = Path::new("/").join("a".repeat(SOCKET_PATH_LIMIT));
        let p = Paths::new(&long);
        assert_eq!(p.validate_socket().unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert!(p.ensure_home().is_err());
        assert!(Paths::new(Path::new("/h")).validate_socket().is_ok());
        // Boundary: exactly limit-1 bytes is fine, limit is not.
        let ok = "/".to_string() + &"a".repeat(SOCKET_PATH_LIMIT - 2);
        assert!(validate_socket_path(Path::new(&ok)).is_ok());
        assert!(validate_socket_path(Path::new(&(ok + "a"))).is_err());
    }
}
