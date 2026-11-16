//! Path-safe file access in `<repo>/.mirante/memory/`.
//!
//! The memory directory sits in a checkout that people and agents write (agents edit
//! checkouts; their worktrees share its repository), and the daemon writes there outside
//! the agents' sandbox. A path checked first and used later can be swapped for a symlink in
//! between (e.g. `.mirante/memory` → `~/.claude/commands`), so every access goes through
//! [`MemoryDir`], a [`SafeDir`] over `.mirante/memory` opened one level at a time from the
//! repo root without following symlinks; every file operation after that is relative to
//! that descriptor (see [`crate::safedir`]).
//!
//! - Notes are created without clobbering ([`SafeDir::create_file`]); updates need the hash
//!   of the content they replace and land with `renameat`. Both fsync the file and the
//!   directory, and hold an exclusive `flock` on the directory so two writers cannot
//!   interleave.
//! - Reads accept only a regular file with a single link on the directory's device (a
//!   planted hard link to `~/.ssh/id_rsa` is refused), at most [`MAX_NOTE_BYTES`].
use std::path::{Path, PathBuf};

use super::note::is_slug;
use super::{sha256_hex, DnaError, MAX_NOTE_BYTES, MEMORY_DIR};
use crate::safedir::{SafeDir, SafeError};

pub use crate::safedir::Entry;

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

impl From<SafeError> for DnaError {
    fn from(e: SafeError) -> Self {
        match e {
            SafeError::Unsafe(m) => DnaError::Unsafe(m),
            SafeError::Exists(m) => DnaError::Exists(m),
            SafeError::Missing(m) => DnaError::Missing(m),
            e @ (SafeError::TooLarge(..) | SafeError::NotUtf8(_)) => DnaError::Note(e.to_string()),
            SafeError::Io(m) => DnaError::Io(m),
        }
    }
}

/// `<repo>/.mirante/memory`, opened (see the module docs). All note I/O goes through it.
#[derive(Debug)]
pub struct MemoryDir {
    dir: SafeDir,
}

impl MemoryDir {
    /// The memory directory of `repo`: `Ok(None)` when a level does not exist,
    /// `Err(Unsafe)` when one is a symlink or not a directory. With `create`, missing levels
    /// are made first.
    fn open_in(repo: &Path, create: bool) -> Result<Option<MemoryDir>, DnaError> {
        let segs: Vec<&str> = MEMORY_DIR.split('/').collect();
        let dir = if create { Some(SafeDir::create(repo, &segs, 0o755)?) } else { SafeDir::open(repo, &segs)? };
        after_open();
        Ok(dir.map(|dir| MemoryDir { dir }))
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
        self.dir.path()
    }

    /// Its entries (not `.`/`..`), at most `limit + 1` of them, in directory order.
    pub fn entries(&self, limit: usize) -> Result<Vec<Entry>, DnaError> {
        Ok(self.dir.entries(limit)?)
    }

    /// The text of the file `name`: a regular file (never through a symlink) with a single
    /// link, on this directory's device, at most [`MAX_NOTE_BYTES`], UTF-8.
    pub fn read_file(&self, name: &str) -> Result<String, DnaError> {
        Ok(self.dir.read_file(name, MAX_NOTE_BYTES)?)
    }

    /// The text of the note `slug` (see [`MemoryDir::read_file`]).
    pub fn read(&self, slug: &str) -> Result<String, DnaError> {
        check_slug(slug)?;
        self.read_file(&format!("{slug}.md"))
    }

    /// Creates the file `name` with `text`; fails if anything (a file, a symlink) is there.
    fn create_file(&self, name: &str, text: &str) -> Result<(), DnaError> {
        Ok(self.dir.create_file(name, text.as_bytes(), 0o644)?)
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
        let _lock = self.dir.lock()?;
        if sha256_hex(self.read_file(&name)?.as_bytes()) != expect_hash {
            return Err(DnaError::Changed);
        }
        let tmp = self.dir.write_temp(&name, text.as_bytes(), 0o644)?;
        if let Err(e) = self.dir.rename(&tmp, &name) {
            let _ = self.dir.unlink(&tmp);
            return Err(e.into());
        }
        Ok(self.dir.sync()?)
    }

    /// Removes the note `slug` only if its content hashes to `expect_hash` (undoing a
    /// write this process made). True when removed.
    pub fn remove(&self, slug: &str, expect_hash: &str) -> Result<bool, DnaError> {
        check_slug(slug)?;
        let name = format!("{slug}.md");
        let _lock = self.dir.lock()?;
        if sha256_hex(self.read_file(&name)?.as_bytes()) != expect_hash {
            return Ok(false);
        }
        self.dir.unlink(&name)?;
        self.dir.sync()?;
        Ok(true)
    }
}

/// `<repo>/.mirante/memory`, checked (both levels real directories, no symlinks), for
/// display. Access goes through [`MemoryDir`].
pub fn memory_dir(repo: &Path) -> Result<PathBuf, DnaError> {
    Ok(MemoryDir::existing(repo)?.path().to_path_buf())
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
        Err(DnaError::Exists(_)) if matches!(dir.dir.stat("README.md"), Ok(Some(e)) if e.is_file()) => {}
        Err(e) => return Err(e),
    }
    Ok(dir.path().to_path_buf())
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
    #[cfg(unix)]
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
        assert_eq!(create_note(d.path(), "a", "two"), Err(DnaError::Exists("a.md".into())));
        for bad in ["../a", "a/b", "A", "", ".hidden"] {
            assert!(create_note(d.path(), bad, "x").is_err(), "{bad}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hosts", note_path(d.path(), "b")).unwrap();
            assert!(create_note(d.path(), "b", "x").is_err()); // dangling or real symlink: refused
        }
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
        assert!(matches!(update_note(d.path(), "missing", &sha256_hex(b""), "x"), Err(DnaError::Missing(_))));
        assert!(matches!(read_note(d.path(), "missing"), Err(DnaError::Missing(_))));
    }

    #[test]
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
