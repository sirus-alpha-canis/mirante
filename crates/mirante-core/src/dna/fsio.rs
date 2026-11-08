//! Path-safe file access in `<repo>/.mirante/memory/`.
//!
//! The memory directory sits in a checkout that people and agents write (agents edit
//! checkouts; their worktrees share its repository), and the daemon writes there outside
//! the agents' sandbox. A path checked first and used later can be swapped for a symlink in
//! between (e.g. `.mirante/memory` → `~/.claude/commands`), so nothing here uses a path
//! twice. Every access goes through [`MemoryDir`], a descriptor of the directory opened
//! one level at a time from the repo root with `O_DIRECTORY | O_NOFOLLOW`; every file
//! operation after that is relative to that descriptor (`openat`, `linkat`, `renameat`,
//! `unlinkat`, `fstatat`). Swapping a level for a symlink after the open changes nothing:
//! the writes stay in the directory that was opened.
//!
//! - Notes are created without clobbering (temp file, then `linkat`, which fails if the
//!   target exists, even as a dangling symlink); updates need the hash of the content they
//!   replace and land with `renameat`. Both fsync the file and the directory, and hold an
//!   exclusive `flock` on the directory so two writers cannot interleave.
//! - Reads open with `O_NOFOLLOW | O_NONBLOCK` and accept only a regular file with a single
//!   link on the directory's device (a planted hard link to `~/.ssh/id_rsa` is refused).
//!
//! Residual risk: someone who can rename the opened directory itself into another place
//! on the same file system makes the write land there, but that needs write access to the
//! destination, which then needs no help from Mirante.
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::note::is_slug;
use super::{sha256_hex, DnaError, MAX_NOTE_BYTES, MEMORY_DIR};

const README: &str = "# Repository memory\n\
\n\
Notes in this folder are this repository's memory, written by people and proposed by\n\
agents through Mirante, then accepted by a person. Each note is Markdown with a\n\
frontmatter block:\n\
\n\
```markdown\n\
---\n\
id: <uuid>\n\
title: \"What the note says, in one line\"\n\
kind: fact | decision | pattern | gotcha\n\
status: active | retired\n\
created: YYYY-MM-DD\n\
source: manual | task:<id>\n\
anchors:\n\
  - \"path/to/file.rs#symbol:L10-24@<commit sha>~<fingerprint>\"\n\
---\n\
The note itself. Link other notes with [[their-file-name]].\n\
```\n\
\n\
Each anchor cites committed code (path, optional symbol, lines, commit and a fingerprint\n\
of those lines). Mirante flags a note as stale when the cited code changes; a person then\n\
confirms it (re-anchoring to the current commit) or retires it.\n\
\n\
Mirante never deletes notes: retiring only sets `status: retired`. Commit this folder so\n\
the team shares the same memory. It also opens as an Obsidian vault.\n";

/// Note names that are not notes (`README.md`; APFS is case-insensitive).
const RESERVED: &[&str] = &["readme"];

fn unsafe_at(p: &str, why: &str) -> DnaError {
    DnaError::Unsafe(format!("{p}: {why}"))
}

fn cname(name: &str) -> Result<CString, DnaError> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(DnaError::Unsafe(format!("{name:?} is not a file name")));
    }
    CString::new(name).map_err(|_| DnaError::Unsafe(format!("{name:?} contains NUL")))
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
fn open_dir_at(parent: RawFd, name: &str, shown: &str) -> Result<Option<OwnedFd>, DnaError> {
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

/// One entry of the memory directory, as `fstatat` (not following symlinks) saw it.
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
}

/// An exclusive `flock` on a directory descriptor, released on drop.
struct Lock(RawFd);

impl Lock {
    fn take(fd: RawFd) -> Result<Lock, DnaError> {
        // SAFETY: plain syscall on a descriptor we own for the lock's lifetime.
        cvt(|| unsafe { libc::flock(fd, libc::LOCK_EX) })?;
        Ok(Lock(fd))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // SAFETY: as above.
        unsafe { libc::flock(self.0, libc::LOCK_UN) };
    }
}

/// `<repo>/.mirante/memory`, opened (see the module docs). All note I/O goes through it.
#[derive(Debug)]
pub struct MemoryDir {
    fd: OwnedFd,
    dev: u64,
    path: PathBuf,
}

impl MemoryDir {
    /// The memory directory of `repo`: `Ok(None)` when a level does not exist,
    /// `Err(Unsafe)` when one is a symlink or not a directory. With `create`, missing levels
    /// are made first.
    fn open_in(repo: &Path, create: bool) -> Result<Option<MemoryDir>, DnaError> {
        let root = repo.canonicalize()?;
        let mut fd: OwnedFd = File::open(&root)?.into();
        let mut shown = root.clone();
        for seg in MEMORY_DIR.split('/') {
            shown.push(seg);
            let label = shown.display().to_string();
            let mut next = open_dir_at(fd.as_raw_fd(), seg, &label)?;
            if next.is_none() && create {
                let c = cname(seg)?;
                // SAFETY: valid name; mkdirat does not keep the pointer.
                match cvt(|| unsafe { libc::mkdirat(fd.as_raw_fd(), c.as_ptr(), 0o755) }) {
                    Ok(_) => {}
                    Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(e.into()),
                }
                next = open_dir_at(fd.as_raw_fd(), seg, &label)?;
            }
            match next {
                Some(n) => fd = n,
                None => return Ok(None),
            }
        }
        let dev = File::from(fd.try_clone()?).metadata()?.dev();
        after_open();
        Ok(Some(MemoryDir { fd, dev, path: shown }))
    }

    /// The existing memory directory of `repo`, `None` when it does not exist.
    pub fn open(repo: &Path) -> Result<Option<MemoryDir>, DnaError> {
        MemoryDir::open_in(repo, false)
    }

    /// The memory directory of `repo`; an error when it does not exist.
    pub fn existing(repo: &Path) -> Result<MemoryDir, DnaError> {
        MemoryDir::open(repo)?.ok_or_else(|| {
            unsafe_at(&repo.join(MEMORY_DIR).display().to_string(), "does not exist (enable repo memory first)")
        })
    }

    /// Where it was opened (for display).
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn raw(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Its entries (not `.`/`..`), at most `limit + 1` of them, in directory order.
    pub fn entries(&self, limit: usize) -> Result<Vec<Entry>, DnaError> {
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
            out.push(Entry {
                name: name.to_string_lossy().into_owned(),
                mode: st.st_mode as u32,
                size: st.st_size as u64,
                mtime: st.st_mtime as i64,
                mtime_nsec: st.st_mtime_nsec as i64,
            });
            if out.len() > limit {
                break;
            }
        }
        Ok(out)
    }

    /// The text of the file `name`: a regular file (never through a symlink) with a single
    /// link, on this directory's device, at most [`MAX_NOTE_BYTES`], UTF-8.
    pub fn read_file(&self, name: &str) -> Result<String, DnaError> {
        let shown = self.path.join(name).display().to_string();
        let c = cname(name)?;
        // O_NONBLOCK: a planted FIFO must not hang the reader; it is refused below.
        let f = match open_at(self.raw(), &c, libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK, 0) {
            Ok(fd) => File::from(fd),
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => return Err(unsafe_at(&shown, "is a symlink")),
            Err(e) => return Err(e.into()),
        };
        let meta = f.metadata()?;
        if !meta.is_file() {
            return Err(unsafe_at(&shown, "is not a regular file"));
        }
        if meta.nlink() != 1 || meta.dev() != self.dev {
            return Err(unsafe_at(&shown, "is a hard link to another file"));
        }
        if meta.len() > MAX_NOTE_BYTES as u64 {
            return Err(DnaError::Note(format!("{shown} is larger than {MAX_NOTE_BYTES} bytes")));
        }
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        f.take(MAX_NOTE_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_NOTE_BYTES {
            return Err(DnaError::Note(format!("{shown} is larger than {MAX_NOTE_BYTES} bytes")));
        }
        String::from_utf8(bytes).map_err(|_| DnaError::Note(format!("{shown} is not UTF-8")))
    }

    /// The text of the note `slug` (see [`MemoryDir::read_file`]).
    pub fn read(&self, slug: &str) -> Result<String, DnaError> {
        check_slug(slug)?;
        self.read_file(&format!("{slug}.md"))
    }

    fn sync(&self) -> Result<(), DnaError> {
        // SAFETY: plain syscall on our descriptor.
        cvt(|| unsafe { libc::fsync(self.raw()) })?;
        Ok(())
    }

    fn unlink(&self, name: &CStr) -> io::Result<()> {
        // SAFETY: valid name; unlinkat does not keep the pointer.
        cvt(|| unsafe { libc::unlinkat(self.raw(), name.as_ptr(), 0) }).map(drop)
    }

    /// Writes `text` to a fresh temp file (`.<name>.tmp-<uuid>`, never following a symlink)
    /// and syncs it. Returns its name.
    fn write_temp(&self, name: &str, text: &str) -> Result<CString, DnaError> {
        let tmp = cname(&format!(".{name}.tmp-{}", uuid::Uuid::now_v7()))?;
        let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW;
        let mut f = File::from(open_at(self.raw(), &tmp, flags, 0o644)?);
        if let Err(e) = f.write_all(text.as_bytes()).and_then(|()| f.sync_all()) {
            let _ = self.unlink(&tmp);
            return Err(e.into());
        }
        Ok(tmp)
    }

    /// Creates the file `name` with `text`; fails if anything (a file, a symlink) is there.
    fn create_file(&self, name: &str, text: &str) -> Result<(), DnaError> {
        let target = cname(name)?;
        let _lock = Lock::take(self.raw())?;
        let tmp = self.write_temp(name, text)?;
        // SAFETY: valid names, both relative to our descriptor; flags 0: no symlink follow.
        let linked = cvt(|| unsafe { libc::linkat(self.raw(), tmp.as_ptr(), self.raw(), target.as_ptr(), 0) });
        let _ = self.unlink(&tmp);
        match linked {
            Ok(_) => self.sync(),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(DnaError::Io(format!("{name} already exists"))),
            Err(e) => Err(e.into()),
        }
    }

    /// Creates the note `slug` with `text`. Never overwrites.
    pub fn create(&self, slug: &str, text: &str) -> Result<(), DnaError> {
        check_slug(slug)?;
        check_size(text)?;
        self.create_file(&format!("{slug}.md"), text)
    }

    /// Replaces the note `slug` with `text`, only if its current content hashes to
    /// `expect_hash`, else [`DnaError::Changed`]. Atomic: temp file, then `renameat`.
    pub fn update(&self, slug: &str, expect_hash: &str, text: &str) -> Result<(), DnaError> {
        check_slug(slug)?;
        check_size(text)?;
        let name = format!("{slug}.md");
        let target = cname(&name)?;
        let _lock = Lock::take(self.raw())?;
        if sha256_hex(self.read_file(&name)?.as_bytes()) != expect_hash {
            return Err(DnaError::Changed);
        }
        let tmp = self.write_temp(&name, text)?;
        // SAFETY: valid names, both relative to our descriptor.
        let renamed = cvt(|| unsafe { libc::renameat(self.raw(), tmp.as_ptr(), self.raw(), target.as_ptr()) });
        if let Err(e) = renamed {
            let _ = self.unlink(&tmp);
            return Err(e.into());
        }
        self.sync()
    }

    /// Removes the note `slug` only if its content hashes to `expect_hash` (undoing a
    /// write this process made). True when removed.
    pub fn remove(&self, slug: &str, expect_hash: &str) -> Result<bool, DnaError> {
        check_slug(slug)?;
        let name = format!("{slug}.md");
        let _lock = Lock::take(self.raw())?;
        if sha256_hex(self.read_file(&name)?.as_bytes()) != expect_hash {
            return Ok(false);
        }
        self.unlink(&cname(&name)?)?;
        self.sync()?;
        Ok(true)
    }
}

/// `<repo>/.mirante/memory`, checked (both levels real directories, no symlinks), for
/// display. Access goes through [`MemoryDir`].
pub fn memory_dir(repo: &Path) -> Result<PathBuf, DnaError> {
    Ok(MemoryDir::existing(repo)?.path)
}

/// Where the note `slug` lives (unchecked; for display and tests).
pub fn note_path(repo: &Path, slug: &str) -> PathBuf {
    repo.join(MEMORY_DIR).join(format!("{slug}.md"))
}

/// Creates `.mirante/memory/` and its `README.md`. Idempotent: an existing README is kept.
pub fn init(repo: &Path) -> Result<PathBuf, DnaError> {
    let dir = MemoryDir::open_in(repo, true)?.ok_or_else(|| DnaError::Io("could not create the memory dir".into()))?;
    match dir.create_file("README.md", README) {
        Ok(()) => {}
        Err(DnaError::Io(_))
            if stat_at(dir.raw(), c"README.md")
                .is_ok_and(|st| file_type(st.st_mode as u32) == libc::S_IFREG as u32) => {}
        Err(e) => return Err(e),
    }
    Ok(dir.path)
}

fn check_slug(slug: &str) -> Result<(), DnaError> {
    if !is_slug(slug) {
        return Err(DnaError::Note(format!("{slug:?} is not a note name (a-z, 0-9 and -, at most 64)")));
    }
    if RESERVED.contains(&slug) {
        return Err(DnaError::Note(format!("{slug:?} is reserved")));
    }
    Ok(())
}

fn check_size(text: &str) -> Result<(), DnaError> {
    if text.len() > MAX_NOTE_BYTES {
        return Err(DnaError::Note(format!("larger than {MAX_NOTE_BYTES} bytes")));
    }
    Ok(())
}

/// Creates the note `slug` with `text`. Never overwrites: an existing note (or any file or
/// symlink of that name) is an error.
pub fn create_note(repo: &Path, slug: &str, text: &str) -> Result<(), DnaError> {
    check_slug(slug)?;
    check_size(text)?;
    MemoryDir::existing(repo)?.create(slug, text)
}

/// The text of the note `slug` (see [`MemoryDir::read_file`]).
pub fn read_note(repo: &Path, slug: &str) -> Result<String, DnaError> {
    check_slug(slug)?;
    MemoryDir::existing(repo)?.read(slug)
}

/// See [`MemoryDir::update`].
pub fn update_note(repo: &Path, slug: &str, expect_hash: &str, text: &str) -> Result<(), DnaError> {
    check_slug(slug)?;
    check_size(text)?;
    MemoryDir::existing(repo)?.update(slug, expect_hash, text)
}

/// See [`MemoryDir::remove`].
pub fn remove_note(repo: &Path, slug: &str, expect_hash: &str) -> Result<bool, DnaError> {
    MemoryDir::existing(repo)?.remove(slug, expect_hash)
}

#[cfg(test)]
thread_local! {
    /// Runs once right after the memory directory is opened (tests swap it for a symlink).
    static AFTER_OPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
}

#[cfg(test)]
fn after_open() {
    if let Some(f) = AFTER_OPEN.with(|h| h.borrow_mut().take()) {
        f();
    }
}

#[cfg(not(test))]
fn after_open() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dna::sha256_hex;
    use crate::test_support::TempDir;

    fn repo_dir() -> TempDir {
        TempDir::new("dna-fs")
    }

    #[test]
    fn init_creates_dirs_and_readme_once() {
        let d = repo_dir();
        init(d.path()).unwrap();
        let readme = d.path().join(".mirante/memory/README.md");
        let first = std::fs::read_to_string(&readme).unwrap();
        assert!(first.contains("[[") && first.contains("never deletes"));
        std::fs::write(&readme, "edited by the team").unwrap();
        init(d.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&readme).unwrap(), "edited by the team");
        assert!(memory_dir(d.path()).unwrap().ends_with(".mirante/memory"));
    }

    #[test]
    fn refuses_symlinked_memory_dirs() {
        let d = TempDir::new("dna-sym");
        let outside = TempDir::new("dna-out");
        std::os::unix::fs::symlink(outside.path(), d.path().join(".mirante")).unwrap();
        assert!(matches!(init(d.path()), Err(DnaError::Unsafe(_))));
        assert!(matches!(create_note(d.path(), "x", "t"), Err(DnaError::Unsafe(_))));
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        // The inner level as a symlink is refused too.
        let d2 = TempDir::new("dna-sym2");
        std::fs::create_dir(d2.path().join(".mirante")).unwrap();
        std::os::unix::fs::symlink(outside.path(), d2.path().join(".mirante/memory")).unwrap();
        assert!(matches!(init(d2.path()), Err(DnaError::Unsafe(_))));
        assert!(matches!(read_note(d2.path(), "x"), Err(DnaError::Unsafe(_))));
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn create_never_clobbers_and_rejects_bad_slugs() {
        let d = repo_dir();
        init(d.path()).unwrap();
        create_note(d.path(), "a", "one").unwrap();
        assert!(create_note(d.path(), "a", "two").is_err());
        for bad in ["../a", "a/b", "A", "", ".hidden"] {
            assert!(create_note(d.path(), bad, "x").is_err(), "{bad}");
        }
        std::os::unix::fs::symlink("/etc/hosts", note_path(d.path(), "b")).unwrap();
        assert!(create_note(d.path(), "b", "x").is_err()); // dangling or real symlink: refused
        assert_eq!(std::fs::read_to_string(note_path(d.path(), "a")).unwrap(), "one");
        // No temp files are left behind.
        let names: Vec<String> = std::fs::read_dir(memory_dir(d.path()).unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.contains(".tmp-")), "{names:?}");
    }

    #[test]
    fn update_needs_the_hash_it_read() {
        let d = repo_dir();
        init(d.path()).unwrap();
        create_note(d.path(), "a", "one").unwrap();
        assert_eq!(update_note(d.path(), "a", &sha256_hex(b"stale"), "x"), Err(DnaError::Changed));
        update_note(d.path(), "a", &sha256_hex(b"one"), "two").unwrap();
        assert_eq!(read_note(d.path(), "a").unwrap(), "two");
        assert!(update_note(d.path(), "missing", &sha256_hex(b""), "x").is_err());
    }

    #[test]
    fn read_refuses_symlinks_and_oversize() {
        let d = repo_dir();
        init(d.path()).unwrap();
        let target = d.path().join("secret.txt");
        std::fs::write(&target, "secret").unwrap();
        std::os::unix::fs::symlink(&target, note_path(d.path(), "link")).unwrap();
        assert!(read_note(d.path(), "link").is_err());
        std::fs::write(note_path(d.path(), "big"), "x".repeat(MAX_NOTE_BYTES + 1)).unwrap();
        assert!(read_note(d.path(), "big").is_err());
        std::fs::create_dir(note_path(d.path(), "dir")).unwrap();
        assert!(read_note(d.path(), "dir").is_err());
        assert!(read_note(d.path(), "../secret").is_err());
    }

    /// Swaps `.mirante/memory` for a symlink to `outside` (the real one moved aside to
    /// `.mirante/kept`) right after the next open.
    fn swap_after_open(repo: &Path, outside: &Path) {
        let (repo, outside) = (repo.to_path_buf(), outside.to_path_buf());
        AFTER_OPEN.with(|h| {
            *h.borrow_mut() = Some(Box::new(move || {
                std::fs::rename(repo.join(".mirante/memory"), repo.join(".mirante/kept")).unwrap();
                std::os::unix::fs::symlink(&outside, repo.join(".mirante/memory")).unwrap();
            }))
        });
    }

    #[test]
    fn a_dir_swapped_for_a_symlink_mid_write_is_never_written_through() {
        let d = repo_dir();
        let outside = TempDir::new("dna-swap-out");
        init(d.path()).unwrap();
        swap_after_open(d.path(), outside.path());
        let _ = create_note(d.path(), "a", "one");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0, "create wrote outside");
        assert_eq!(std::fs::read_to_string(d.path().join(".mirante/kept/a.md")).unwrap(), "one");

        // Same for an update.
        std::fs::remove_file(d.path().join(".mirante/memory")).unwrap();
        std::fs::rename(d.path().join(".mirante/kept"), d.path().join(".mirante/memory")).unwrap();
        std::fs::write(outside.path().join("a.md"), "one").unwrap();
        swap_after_open(d.path(), outside.path());
        let _ = update_note(d.path(), "a", &sha256_hex(b"one"), "two");
        assert_eq!(std::fs::read_to_string(outside.path().join("a.md")).unwrap(), "one", "update wrote outside");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
    }

    #[test]
    fn read_refuses_hard_links() {
        let d = repo_dir();
        init(d.path()).unwrap();
        let secret = d.path().join("secret.txt");
        std::fs::write(&secret, "secret").unwrap();
        std::fs::hard_link(&secret, note_path(d.path(), "linked")).unwrap();
        assert!(matches!(read_note(d.path(), "linked"), Err(DnaError::Unsafe(_))));
    }

    #[test]
    fn readme_is_reserved_and_remove_needs_the_hash() {
        let d = repo_dir();
        init(d.path()).unwrap();
        assert!(create_note(d.path(), "readme", "x").is_err());
        create_note(d.path(), "a", "one").unwrap();
        assert!(!remove_note(d.path(), "a", &sha256_hex(b"other")).unwrap());
        assert!(note_path(d.path(), "a").exists());
        assert!(remove_note(d.path(), "a", &sha256_hex(b"one")).unwrap());
        assert!(!note_path(d.path(), "a").exists());
    }
}
