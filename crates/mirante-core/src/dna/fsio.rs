//! Path-safe writes in `<repo>/.mirante/memory/`.
//!
//! The memory directory sits in a checkout that other people (and, through merges, agents)
//! write, so every write refuses symlinks: `.mirante` and `.mirante/memory` must be real
//! directories inside the repo, notes are created without clobbering (temp file, then
//! `link`, which fails if the target exists, even as a dangling symlink), and updates need
//! the hash of the content they replace.
//!
//! Residual risk, by design: between the checks and the final `rename` of an update there
//! is a short TOCTOU window in which someone with write access to the user's own checkout
//! could swap a directory for a symlink. Anyone who can do that can already write the
//! checkout directly.
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
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

fn unsafe_dir(p: &Path, why: &str) -> DnaError {
    DnaError::Unsafe(format!("{}: {why}", p.display()))
}

/// Ok when `p` is a real directory (not a symlink), `Err(Unsafe)` when it is anything else,
/// and `Ok(false)` when it does not exist.
fn real_dir(p: &Path) -> Result<bool, DnaError> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_symlink() => Err(unsafe_dir(p, "is a symlink")),
        Ok(m) if m.is_dir() => Ok(true),
        Ok(_) => Err(unsafe_dir(p, "is not a directory")),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// `<repo>/.mirante/memory`, checked: both levels real directories (no symlinks), inside
/// the canonical repo.
pub fn memory_dir(repo: &Path) -> Result<PathBuf, DnaError> {
    let root = repo.canonicalize()?;
    let mut dir = root.clone();
    for seg in MEMORY_DIR.split('/') {
        dir.push(seg);
        if !real_dir(&dir)? {
            return Err(unsafe_dir(&dir, "does not exist (enable repo memory first)"));
        }
    }
    let canonical = dir.canonicalize()?;
    if !canonical.starts_with(&root) {
        return Err(unsafe_dir(&dir, "resolves outside the repository"));
    }
    Ok(canonical)
}

/// Where the note `slug` lives (unchecked; for display and tests).
pub fn note_path(repo: &Path, slug: &str) -> PathBuf {
    repo.join(MEMORY_DIR).join(format!("{slug}.md"))
}

/// Creates `.mirante/memory/` and its `README.md`. Idempotent: an existing README is kept.
pub fn init(repo: &Path) -> Result<PathBuf, DnaError> {
    let root = repo.canonicalize()?;
    let mut dir = root.clone();
    for seg in MEMORY_DIR.split('/') {
        dir.push(seg);
        if !real_dir(&dir)? {
            match DirBuilder::new().mode(0o755).create(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
            // Raced with someone else creating it: accept only a real directory.
            real_dir(&dir)?;
        }
    }
    let dir = memory_dir(&root)?;
    match create_in(&dir, "README.md", README) {
        Ok(()) => {}
        Err(DnaError::Io(_)) if real_file(&dir.join("README.md")) => {}
        Err(e) => return Err(e),
    }
    Ok(dir)
}

fn real_file(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file())
}

/// Writes `text` to a fresh temp file in `dir` (never following a symlink) and syncs it.
fn write_temp(dir: &Path, name: &str, text: &str) -> Result<PathBuf, DnaError> {
    let tmp = dir.join(format!(".{name}.tmp-{}", uuid::Uuid::now_v7()));
    let mut f =
        OpenOptions::new().write(true).create_new(true).custom_flags(libc::O_NOFOLLOW).mode(0o644).open(&tmp)?;
    let written = f.write_all(text.as_bytes()).and_then(|()| f.sync_all());
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(tmp)
}

/// Creates `dir/name` with `text`; fails if anything (a file, a symlink) is already there.
fn create_in(dir: &Path, name: &str, text: &str) -> Result<(), DnaError> {
    let tmp = write_temp(dir, name, text)?;
    let linked = std::fs::hard_link(&tmp, dir.join(name));
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(DnaError::Io(format!("{name} already exists"))),
        Err(e) => Err(e.into()),
    }
}

fn check_slug(slug: &str) -> Result<(), DnaError> {
    if is_slug(slug) {
        Ok(())
    } else {
        Err(DnaError::Note(format!("{slug:?} is not a note name (a-z, 0-9 and -, at most 64)")))
    }
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
    let dir = memory_dir(repo)?;
    create_in(&dir, &format!("{slug}.md"), text)
}

/// The text of the note `slug`: a regular file (never through a symlink), at most
/// [`MAX_NOTE_BYTES`], UTF-8.
pub fn read_note(repo: &Path, slug: &str) -> Result<String, DnaError> {
    check_slug(slug)?;
    let path = memory_dir(repo)?.join(format!("{slug}.md"));
    read_file(&path)
}

fn read_file(path: &Path) -> Result<String, DnaError> {
    // O_NONBLOCK: a planted FIFO must not hang the reader; it is refused below.
    let f = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path);
    let f: File = match f {
        Ok(f) => f,
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => return Err(unsafe_dir(path, "is a symlink")),
        Err(e) => return Err(e.into()),
    };
    let meta = f.metadata()?;
    if !meta.is_file() {
        return Err(unsafe_dir(path, "is not a regular file"));
    }
    if meta.len() > MAX_NOTE_BYTES as u64 {
        return Err(DnaError::Note(format!("{} is larger than {MAX_NOTE_BYTES} bytes", path.display())));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    f.take(MAX_NOTE_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_NOTE_BYTES {
        return Err(DnaError::Note(format!("{} is larger than {MAX_NOTE_BYTES} bytes", path.display())));
    }
    String::from_utf8(bytes).map_err(|_| DnaError::Note(format!("{} is not UTF-8", path.display())))
}

/// Replaces the note `slug` with `text`, only if its current content hashes (SHA-256 hex)
/// to `expect_hash`, else [`DnaError::Changed`]. Atomic: temp file, then `rename`.
pub fn update_note(repo: &Path, slug: &str, expect_hash: &str, text: &str) -> Result<(), DnaError> {
    check_slug(slug)?;
    check_size(text)?;
    let current = read_note(repo, slug)?;
    if sha256_hex(current.as_bytes()) != expect_hash {
        return Err(DnaError::Changed);
    }
    let dir = memory_dir(repo)?;
    let name = format!("{slug}.md");
    let tmp = write_temp(&dir, &name, text)?;
    std::fs::rename(&tmp, dir.join(&name)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

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
}
