//! Path-safe directory access through descriptors.
//!
//! Mirante writes in places other processes can change: `<repo>/.mirante/memory` sits in a
//! checkout agents write; `$MIRANTE_HOME/skills` is in the user's home, where an agent
//! running as the user could plant a symlink. A path checked first and used later can be
//! swapped for a symlink in between, so nothing here uses a path twice. A [`SafeDir`] is a
//! descriptor of a directory opened one level at a time from a root with
//! `O_DIRECTORY | O_NOFOLLOW`; every operation after that is relative to it (`openat`,
//! `mkdirat`, `linkat`, `renameat`, `unlinkat`, `fstatat`). Swapping a level for a symlink
//! after the open changes nothing: the operations stay in the directory that was opened.
//!
//! - Files are created without clobbering: a temp file then `linkat` (fails if the target
//!   exists, even as a dangling symlink), or `O_CREAT | O_EXCL | O_NOFOLLOW` directly.
//!   Writes fsync the file and the directory.
//! - Reads open with `O_NOFOLLOW | O_NONBLOCK` and accept only a regular file with a single
//!   link on the directory's device (a planted hard link to `~/.ssh/id_rsa` is refused).
//! - [`SafeDir::rename_new`] never replaces its target (`renameatx_np(RENAME_EXCL)` on
//!   macOS, `renameat2(RENAME_NOREPLACE)` on Linux).
//! - [`SafeDir::lock`] takes an exclusive `flock` on the directory.
//!
//! Residual risk: someone who can rename the opened directory itself into another place on
//! the same file system makes the operations land there, but that needs write access to the
//! destination, which then needs no help from Mirante.
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// What can go wrong; each user maps it into its own error type.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SafeError {
    /// A level is a symlink or not a directory, a file is not regular, or has other links.
    #[error("{0}")]
    Unsafe(String),
    /// Creating something whose name is taken.
    #[error("{0} already exists")]
    Exists(String),
    #[error("{0} does not exist")]
    Missing(String),
    /// A file larger than the caller's limit.
    #[error("{0} is larger than {1} bytes")]
    TooLarge(String, usize),
    #[error("{0} is not UTF-8")]
    NotUtf8(String),
    #[error("{0}")]
    Io(String),
}

impl From<io::Error> for SafeError {
    fn from(e: io::Error) -> Self {
        SafeError::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, SafeError>;

fn unsafe_at(p: &str, why: &str) -> SafeError {
    SafeError::Unsafe(format!("{p}: {why}"))
}

/// A single path component as a C string: not empty, `.`, `..`, no `/`, no NUL.
pub fn cname(name: &str) -> Result<CString> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(SafeError::Unsafe(format!("{name:?} is not a file name")));
    }
    CString::new(name).map_err(|_| SafeError::Unsafe(format!("{name:?} contains NUL")))
}

/// Retries `f` while it fails with `EINTR`; a negative return is the error in `errno`.
fn cvt(mut f: impl FnMut() -> libc::c_int) -> io::Result<libc::c_int> {
    loop {
        let r = f();
        if r >= 0 {
            return Ok(r);
        }
        let e = io::Error::last_os_error();
        if e.kind() != ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// `openat(dir, name, flags | O_CLOEXEC, mode)`.
fn open_at(dir: RawFd, name: &CStr, flags: libc::c_int, mode: libc::c_uint) -> io::Result<OwnedFd> {
    // SAFETY: `name` is a valid NUL-terminated string; openat does not keep the pointer.
    let fd = cvt(|| unsafe { libc::openat(dir, name.as_ptr(), flags | libc::O_CLOEXEC, mode) })?;
    // SAFETY: openat just returned this descriptor, owned by nobody else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// `fstatat(dir, name, AT_SYMLINK_NOFOLLOW)`.
fn stat_at(dir: RawFd, name: &CStr) -> io::Result<libc::stat> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: valid name and an out-pointer to a properly sized `stat`.
    cvt(|| unsafe { libc::fstatat(dir, name.as_ptr(), st.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) })?;
    // SAFETY: fstatat succeeded, so it filled `st`.
    Ok(unsafe { st.assume_init() })
}

fn file_type(mode: u32) -> u32 {
    mode & libc::S_IFMT as u32
}

/// Opens the directory `name` inside `parent`, never through a symlink.
fn open_dir_at(parent: RawFd, name: &str, shown: &str) -> Result<Option<OwnedFd>> {
    let c = cname(name)?;
    match open_at(parent, &c, libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW, 0) {
        Ok(fd) => Ok(Some(fd)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => {
            let why = match stat_at(parent, &c) {
                Ok(st) if file_type(st.st_mode as u32) == libc::S_IFLNK as u32 => "is a symlink",
                _ => "is not a directory",
            };
            Err(unsafe_at(shown, why))
        }
        Err(e) => Err(e.into()),
    }
}

/// `mkdirat(parent, name, mode)`; an existing entry is fine (the open after it checks it).
fn mkdir_at(parent: RawFd, name: &str, mode: libc::mode_t) -> Result<bool> {
    let c = cname(name)?;
    // SAFETY: valid name; mkdirat does not keep the pointer.
    match cvt(|| unsafe { libc::mkdirat(parent, c.as_ptr(), mode) }) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// One entry of a directory, as `fstatat` (not following symlinks) saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub mode: u32,
    pub size: u64,
    pub mtime: i64,
    pub mtime_nsec: i64,
}

impl Entry {
    pub fn is_file(&self) -> bool {
        file_type(self.mode) == libc::S_IFREG as u32
    }

    pub fn is_dir(&self) -> bool {
        file_type(self.mode) == libc::S_IFDIR as u32
    }
}

#[allow(clippy::unnecessary_cast)] // the stat field types differ between platforms
fn entry(name: String, st: &libc::stat) -> Entry {
    Entry {
        name,
        mode: st.st_mode as u32,
        size: st.st_size as u64,
        mtime: st.st_mtime as i64,
        mtime_nsec: st.st_mtime_nsec as i64,
    }
}

/// An exclusive `flock` on a directory descriptor, released on drop.
pub struct Lock(RawFd);

impl Drop for Lock {
    fn drop(&mut self) {
        // SAFETY: plain syscall on a descriptor the SafeDir owns for the lock's lifetime.
        unsafe { libc::flock(self.0, libc::LOCK_UN) };
    }
}

/// A directory opened through descriptors (see the module docs).
#[derive(Debug)]
pub struct SafeDir {
    fd: OwnedFd,
    dev: u64,
    path: PathBuf,
}

/// How deep [`SafeDir::remove_tree`] goes.
const MAX_DEPTH: usize = 8;

impl SafeDir {
    /// `root` (canonicalized), then each of `segs`, one level at a time. `Ok(None)` when a
    /// level does not exist; with `create`, missing levels are made with `mode` first.
    /// `Err(Unsafe)` when a level is a symlink or not a directory.
    fn walk(root: &Path, segs: &[&str], create: Option<libc::mode_t>) -> Result<Option<SafeDir>> {
        let root = root.canonicalize().map_err(|e| match e.kind() {
            ErrorKind::NotFound => SafeError::Missing(root.display().to_string()),
            _ => e.into(),
        })?;
        let mut fd: OwnedFd = File::open(&root)?.into();
        let mut shown = root.clone();
        for seg in segs {
            shown.push(seg);
            let label = shown.display().to_string();
            let mut next = open_dir_at(fd.as_raw_fd(), seg, &label)?;
            if next.is_none() {
                if let Some(mode) = create {
                    mkdir_at(fd.as_raw_fd(), seg, mode)?;
                    next = open_dir_at(fd.as_raw_fd(), seg, &label)?;
                }
            }
            match next {
                Some(n) => fd = n,
                None => return Ok(None),
            }
        }
        let dev = File::from(fd.try_clone()?).metadata()?.dev();
        Ok(Some(SafeDir { fd, dev, path: shown }))
    }

    /// The existing directory `root/segs…`, `None` when a level does not exist.
    pub fn open(root: &Path, segs: &[&str]) -> Result<Option<SafeDir>> {
        SafeDir::walk(root, segs, None)
    }

    /// The directory `root/segs…`, making missing levels with `mode` (`root` must exist).
    pub fn create(root: &Path, segs: &[&str], mode: libc::mode_t) -> Result<SafeDir> {
        SafeDir::walk(root, segs, Some(mode))?.ok_or_else(|| SafeError::Io("could not create the directory".into()))
    }

    /// Where it was opened (for display).
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn raw(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// The same directory through a duplicated descriptor (never re-opened by path).
    pub fn try_clone(&self) -> Result<SafeDir> {
        Ok(SafeDir { fd: self.fd.try_clone()?, dev: self.dev, path: self.path.clone() })
    }

    fn shown(&self, name: &str) -> String {
        self.path.join(name).display().to_string()
    }

    /// The child directory `name`, never through a symlink; `None` when it does not exist.
    pub fn open_dir(&self, name: &str) -> Result<Option<SafeDir>> {
        let shown = self.path.join(name);
        let Some(fd) = open_dir_at(self.raw(), name, &shown.display().to_string())? else {
            return Ok(None);
        };
        let dev = File::from(fd.try_clone()?).metadata()?.dev();
        Ok(Some(SafeDir { fd, dev, path: shown }))
    }

    /// The child directory `name`, made with `mode` when missing.
    pub fn ensure_dir(&self, name: &str, mode: libc::mode_t) -> Result<SafeDir> {
        mkdir_at(self.raw(), name, mode)?;
        self.open_dir(name)?.ok_or_else(|| SafeError::Missing(self.shown(name)))
    }

    /// Makes the child directory `name` with `mode`; [`SafeError::Exists`] when anything (a
    /// directory, a file, a symlink) has that name. Returns it opened.
    pub fn create_dir_new(&self, name: &str, mode: libc::mode_t) -> Result<SafeDir> {
        if !mkdir_at(self.raw(), name, mode)? {
            return Err(SafeError::Exists(name.to_string()));
        }
        self.open_dir(name)?.ok_or_else(|| SafeError::Missing(self.shown(name)))
    }

    /// Its entries (not `.`/`..`), at most `limit + 1` of them, in directory order.
    pub fn entries(&self, limit: usize) -> Result<Vec<Entry>> {
        struct Dir(*mut libc::DIR);
        impl Drop for Dir {
            fn drop(&mut self) {
                // SAFETY: the stream came from fdopendir and is closed once (this also
                // closes the duplicated descriptor it owns).
                unsafe { libc::closedir(self.0) };
            }
        }
        // SAFETY: duplicating a descriptor we own; the copy is handed to fdopendir below.
        let dup = cvt(|| unsafe { libc::fcntl(self.raw(), libc::F_DUPFD_CLOEXEC, 0) })?;
        // SAFETY: `dup` is a fresh directory descriptor; on success the stream owns it.
        let dirp = unsafe { libc::fdopendir(dup) };
        if dirp.is_null() {
            let e = io::Error::last_os_error();
            // SAFETY: fdopendir failed, so `dup` is still ours to close.
            unsafe { libc::close(dup) };
            return Err(e.into());
        }
        let dir = Dir(dirp);
        // SAFETY: valid stream; the duplicate shares the offset, so start from the top.
        unsafe { libc::rewinddir(dir.0) };
        let mut out = Vec::new();
        loop {
            // SAFETY: valid stream; the entry is read before the next readdir call.
            let ent = unsafe { libc::readdir(dir.0) };
            if ent.is_null() {
                break;
            }
            // SAFETY: readdir returned a valid entry whose d_name is NUL-terminated.
            let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) }.to_owned();
            if name.as_bytes() == b"." || name.as_bytes() == b".." {
                continue;
            }
            let st = match stat_at(self.raw(), &name) {
                Ok(st) => st,
                Err(e) if e.kind() == ErrorKind::NotFound => continue, // removed meanwhile
                Err(e) => return Err(e.into()),
            };
            out.push(entry(name.to_string_lossy().into_owned(), &st));
            if out.len() > limit {
                break;
            }
        }
        Ok(out)
    }

    /// The entry `name` as `fstatat` sees it (not following a symlink); `None` if absent.
    pub fn stat(&self, name: &str) -> Result<Option<Entry>> {
        let c = cname(name)?;
        match stat_at(self.raw(), &c) {
            Ok(st) => Ok(Some(entry(name.to_string(), &st))),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The bytes of the file `name`: a regular file (never through a symlink) with a single
    /// link, on this directory's device, at most `max` bytes.
    pub fn read_bytes(&self, name: &str, max: usize) -> Result<Vec<u8>> {
        let shown = self.shown(name);
        let c = cname(name)?;
        // O_NONBLOCK: a planted FIFO must not hang the reader; it is refused below.
        let f = match open_at(self.raw(), &c, libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK, 0) {
            Ok(fd) => File::from(fd),
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => return Err(unsafe_at(&shown, "is a symlink")),
            Err(e) if e.kind() == ErrorKind::NotFound => return Err(SafeError::Missing(shown)),
            Err(e) => return Err(e.into()),
        };
        let meta = f.metadata()?;
        if !meta.is_file() {
            return Err(unsafe_at(&shown, "is not a regular file"));
        }
        if meta.nlink() != 1 || meta.dev() != self.dev {
            return Err(unsafe_at(&shown, "is a hard link to another file"));
        }
        if meta.len() > max as u64 {
            return Err(SafeError::TooLarge(shown, max));
        }
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        f.take(max as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > max {
            return Err(SafeError::TooLarge(shown, max));
        }
        Ok(bytes)
    }

    /// [`SafeDir::read_bytes`] as UTF-8 text.
    pub fn read_file(&self, name: &str, max: usize) -> Result<String> {
        let bytes = self.read_bytes(name, max)?;
        String::from_utf8(bytes).map_err(|_| SafeError::NotUtf8(self.shown(name)))
    }

    /// fsync the directory.
    pub fn sync(&self) -> Result<()> {
        // SAFETY: plain syscall on our descriptor.
        cvt(|| unsafe { libc::fsync(self.raw()) })?;
        Ok(())
    }

    /// An exclusive `flock` on the directory, held until the [`Lock`] drops.
    pub fn lock(&self) -> Result<Lock> {
        // SAFETY: plain syscall on a descriptor we own for the lock's lifetime.
        cvt(|| unsafe { libc::flock(self.raw(), libc::LOCK_EX) })?;
        Ok(Lock(self.raw()))
    }

    /// Removes the file (or symlink) `name`.
    pub fn unlink(&self, name: &str) -> Result<()> {
        let c = cname(name)?;
        // SAFETY: valid name; unlinkat does not keep the pointer.
        cvt(|| unsafe { libc::unlinkat(self.raw(), c.as_ptr(), 0) })?;
        Ok(())
    }

    /// Creates the file `name` with `bytes` and `mode` directly (`O_CREAT | O_EXCL |
    /// O_NOFOLLOW`), synced. [`SafeError::Exists`] when the name is taken. For a directory
    /// nobody else writes yet (a staging folder).
    pub fn write_new(&self, name: &str, bytes: &[u8], mode: libc::c_uint) -> Result<()> {
        let c = cname(name)?;
        let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW;
        let mut f = match open_at(self.raw(), &c, flags, mode) {
            Ok(fd) => File::from(fd),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => return Err(SafeError::Exists(name.to_string())),
            Err(e) => return Err(e.into()),
        };
        if let Err(e) = f.write_all(bytes).and_then(|()| f.sync_all()) {
            let _ = self.unlink(name);
            return Err(e.into());
        }
        Ok(())
    }

    /// Writes `bytes` to a fresh temp file (`.<name>.tmp-<uuid>`) and syncs it. Returns its
    /// name.
    pub fn write_temp(&self, name: &str, bytes: &[u8], mode: libc::c_uint) -> Result<String> {
        let tmp = format!(".{name}.tmp-{}", uuid::Uuid::now_v7());
        self.write_new(&tmp, bytes, mode)?;
        Ok(tmp)
    }

    /// Creates the file `name` with `bytes`, never clobbering: a temp file, then `linkat`
    /// (fails if anything, even a dangling symlink, has the name), under the directory's
    /// lock; then the directory is synced.
    pub fn create_file(&self, name: &str, bytes: &[u8], mode: libc::c_uint) -> Result<()> {
        let target = cname(name)?;
        let _lock = self.lock()?;
        let tmp = self.write_temp(name, bytes, mode)?;
        let ctmp = cname(&tmp)?;
        // SAFETY: valid names, both relative to our descriptor; flags 0: no symlink follow.
        let linked = cvt(|| unsafe { libc::linkat(self.raw(), ctmp.as_ptr(), self.raw(), target.as_ptr(), 0) });
        let _ = self.unlink(&tmp);
        match linked {
            Ok(_) => self.sync(),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(SafeError::Exists(name.to_string())),
            Err(e) => Err(e.into()),
        }
    }

    /// `renameat(from → to)` inside this directory, replacing `to` (an atomic update).
    pub fn rename(&self, from: &str, to: &str) -> Result<()> {
        let (f, t) = (cname(from)?, cname(to)?);
        // SAFETY: valid names, both relative to our descriptor.
        cvt(|| unsafe { libc::renameat(self.raw(), f.as_ptr(), self.raw(), t.as_ptr()) })?;
        Ok(())
    }

    /// Renames `from` to `to` inside this directory only when nothing has the name `to`:
    /// [`SafeError::Exists`] otherwise (an empty directory included).
    pub fn rename_new(&self, from: &str, to: &str) -> Result<()> {
        let (f, t) = (cname(from)?, cname(to)?);
        match rename_noreplace(self.raw(), &f, &t) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(SafeError::Exists(to.to_string())),
            Err(e) => Err(e.into()),
        }
    }

    /// Removes the directory `name` and everything in it, through descriptors (never
    /// following a symlink: a symlink inside is unlinked, not followed), at most
    /// [`MAX_DEPTH`] levels deep.
    pub fn remove_tree(&self, name: &str) -> Result<()> {
        self.remove_tree_at(name, 0)
    }

    fn remove_tree_at(&self, name: &str, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(unsafe_at(&self.shown(name), "is nested too deep"));
        }
        let Some(dir) = self.open_dir(name)? else {
            return Ok(());
        };
        loop {
            let batch = dir.entries(256)?;
            if batch.is_empty() {
                break;
            }
            for e in batch {
                if e.is_dir() {
                    dir.remove_tree_at(&e.name, depth + 1)?;
                } else {
                    dir.unlink(&e.name)?;
                }
            }
        }
        let c = cname(name)?;
        // SAFETY: valid name; unlinkat does not keep the pointer.
        cvt(|| unsafe { libc::unlinkat(self.raw(), c.as_ptr(), libc::AT_REMOVEDIR) })?;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn rename_noreplace(dir: RawFd, from: &CStr, to: &CStr) -> io::Result<()> {
    // SAFETY: valid names, both relative to `dir`; RENAME_EXCL fails if `to` exists.
    cvt(|| unsafe { libc::renameatx_np(dir, from.as_ptr(), dir, to.as_ptr(), libc::RENAME_EXCL) }).map(drop)
}

#[cfg(target_os = "linux")]
fn rename_noreplace(dir: RawFd, from: &CStr, to: &CStr) -> io::Result<()> {
    // SAFETY: valid names, both relative to `dir`; RENAME_NOREPLACE fails if `to` exists.
    cvt(|| unsafe { libc::renameat2(dir, from.as_ptr(), dir, to.as_ptr(), libc::RENAME_NOREPLACE) }).map(drop)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn rename_noreplace(dir: RawFd, from: &CStr, to: &CStr) -> io::Result<()> {
    // Best effort elsewhere: check, then rename (callers hold the directory's lock).
    if stat_at(dir, to).is_ok() {
        return Err(io::Error::from(ErrorKind::AlreadyExists));
    }
    // SAFETY: valid names, both relative to `dir`.
    cvt(|| unsafe { libc::renameat(dir, from.as_ptr(), dir, to.as_ptr()) }).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn opens_nested_levels_without_following_symlinks() {
        let d = TempDir::new("safedir");
        std::fs::create_dir_all(d.path().join("a/b/c")).unwrap();
        let c = SafeDir::open(d.path(), &["a", "b", "c"]).unwrap().unwrap();
        assert!(c.path().ends_with("a/b/c"));
        assert!(SafeDir::open(d.path(), &["a", "missing"]).unwrap().is_none());
        // A level that is a symlink (to a real directory) is refused, at any depth.
        let outside = TempDir::new("safedir-out");
        std::fs::create_dir(outside.path().join("c")).unwrap();
        std::fs::rename(d.path().join("a/b"), d.path().join("a/kept")).unwrap();
        std::os::unix::fs::symlink(outside.path(), d.path().join("a/b")).unwrap();
        assert!(matches!(SafeDir::open(d.path(), &["a", "b", "c"]), Err(SafeError::Unsafe(_))));
        assert!(matches!(SafeDir::create(d.path(), &["a", "b", "x"], 0o700), Err(SafeError::Unsafe(_))));
        assert!(!outside.path().join("x").exists());
        let a = SafeDir::open(d.path(), &["a"]).unwrap().unwrap();
        assert!(matches!(a.open_dir("b"), Err(SafeError::Unsafe(_))));
        assert!(a.open_dir("kept").unwrap().is_some());
        // create makes missing levels with the mode asked for.
        let n = SafeDir::create(d.path(), &["n1", "n2"], 0o700).unwrap();
        assert!(n.path().ends_with("n1/n2"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(d.path().join("n1/n2")).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn create_dir_new_refuses_existing_and_symlinks() {
        let d = TempDir::new("safedir-new");
        let root = SafeDir::open(d.path(), &[]).unwrap().unwrap();
        root.create_dir_new("x", 0o700).unwrap();
        assert_eq!(root.create_dir_new("x", 0o700).unwrap_err(), SafeError::Exists("x".into()));
        std::os::unix::fs::symlink("/tmp", d.path().join("y")).unwrap();
        assert_eq!(root.create_dir_new("y", 0o700).unwrap_err(), SafeError::Exists("y".into()));
        std::fs::write(d.path().join("f"), "x").unwrap();
        assert!(root.create_dir_new("f", 0o700).is_err());
        assert!(root.create_dir_new("../z", 0o700).is_err());
    }

    #[test]
    fn rename_new_never_replaces_even_an_empty_dir() {
        let d = TempDir::new("safedir-rename");
        let root = SafeDir::open(d.path(), &[]).unwrap().unwrap();
        let s1 = root.create_dir_new(".stage-1", 0o700).unwrap();
        s1.write_new("a.txt", b"one", 0o600).unwrap();
        root.rename_new(".stage-1", "v1").unwrap();
        assert_eq!(std::fs::read_to_string(d.path().join("v1/a.txt")).unwrap(), "one");
        root.create_dir_new(".stage-2", 0o700).unwrap();
        assert_eq!(root.rename_new(".stage-2", "v1").unwrap_err(), SafeError::Exists("v1".into()));
        root.create_dir_new("v2", 0o700).unwrap(); // empty
        assert_eq!(root.rename_new(".stage-2", "v2").unwrap_err(), SafeError::Exists("v2".into()));
        assert!(d.path().join(".stage-2").is_dir());
    }

    #[test]
    fn write_new_and_create_file_never_clobber() {
        let d = TempDir::new("safedir-write");
        let root = SafeDir::open(d.path(), &[]).unwrap().unwrap();
        root.write_new("a", b"1", 0o600).unwrap();
        assert_eq!(root.write_new("a", b"2", 0o600).unwrap_err(), SafeError::Exists("a".into()));
        std::os::unix::fs::symlink(d.path().join("target"), d.path().join("dangling")).unwrap();
        assert!(root.write_new("dangling", b"x", 0o600).is_err());
        assert!(root.create_file("dangling", b"x", 0o600).is_err());
        assert!(!d.path().join("target").exists());
        root.create_file("b", b"two", 0o600).unwrap();
        assert_eq!(root.read_file("b", 10).unwrap(), "two");
        assert!(matches!(root.read_file("b", 2), Err(SafeError::TooLarge(_, 2))));
    }

    #[test]
    fn remove_tree_unlinks_symlinks_and_never_follows_them() {
        let d = TempDir::new("safedir-rm");
        let outside = TempDir::new("safedir-rm-out");
        std::fs::write(outside.path().join("keep"), "keep").unwrap();
        std::fs::create_dir_all(d.path().join("t/sub/deeper")).unwrap();
        std::fs::write(d.path().join("t/sub/deeper/f"), "x").unwrap();
        std::fs::write(d.path().join("t/g"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path(), d.path().join("t/link")).unwrap();
        let root = SafeDir::open(d.path(), &[]).unwrap().unwrap();
        root.remove_tree("t").unwrap();
        assert!(!d.path().join("t").exists());
        assert_eq!(std::fs::read_to_string(outside.path().join("keep")).unwrap(), "keep");
        root.remove_tree("t").unwrap(); // already gone: fine
    }
}
