//! [`SafeDir`] on Windows: not supported yet, so every operation is refused.
//!
//! The Unix implementation's guarantees (nothing follows a symlink, nothing clobbers, reads
//! refuse hard links) need handle-relative NT calls that are not written yet. Until then
//! [`SafeDir::open`] and [`SafeDir::create`] fail without touching the disk, and the repo
//! memory and the skills library report that error instead of working without those
//! guarantees.
use std::os::windows::io::{AsRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};

use super::{Entry, Result, SafeError};

const UNSUPPORTED: &str = "SafeDir is not supported on Windows yet";

fn unsupported<T>() -> Result<T> {
    Err(SafeError::Io(UNSUPPORTED.into()))
}

/// POSIX file-type bits, so [`Entry::mode`] reads the same on every platform.
const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;

impl Entry {
    pub fn is_file(&self) -> bool {
        self.mode & S_IFMT == S_IFREG
    }

    pub fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }
}

/// An exclusive lock on a directory, released on drop.
pub struct Lock(());

/// A directory opened through handles (see the module docs).
#[derive(Debug)]
pub struct SafeDir {
    handle: OwnedHandle,
    path: PathBuf,
}

impl SafeDir {
    /// The existing directory `root/segs…`, `None` when a level does not exist.
    pub fn open(_root: &Path, _segs: &[&str]) -> Result<Option<SafeDir>> {
        unsupported()
    }

    /// The directory `root/segs…`, making missing levels (`mode` is ignored on Windows).
    pub fn create(_root: &Path, _segs: &[&str], _mode: u32) -> Result<SafeDir> {
        unsupported()
    }

    /// Where it was opened (for display).
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn raw(&self) -> RawHandle {
        self.handle.as_raw_handle()
    }

    pub fn try_clone(&self) -> Result<SafeDir> {
        unsupported()
    }

    pub fn open_dir(&self, _name: &str) -> Result<Option<SafeDir>> {
        unsupported()
    }

    pub fn ensure_dir(&self, _name: &str, _mode: u32) -> Result<SafeDir> {
        unsupported()
    }

    pub fn create_dir_new(&self, _name: &str, _mode: u32) -> Result<SafeDir> {
        unsupported()
    }

    pub fn entries(&self, _limit: usize) -> Result<Vec<Entry>> {
        unsupported()
    }

    pub fn stat(&self, _name: &str) -> Result<Option<Entry>> {
        unsupported()
    }

    pub fn read_bytes(&self, _name: &str, _max: usize) -> Result<Vec<u8>> {
        unsupported()
    }

    pub fn read_file(&self, _name: &str, _max: usize) -> Result<String> {
        unsupported()
    }

    pub fn sync(&self) -> Result<()> {
        unsupported()
    }

    pub fn lock(&self) -> Result<Lock> {
        unsupported()
    }

    pub fn unlink(&self, _name: &str) -> Result<()> {
        unsupported()
    }

    pub fn write_new(&self, _name: &str, _bytes: &[u8], _mode: u32) -> Result<()> {
        unsupported()
    }

    pub fn write_temp(&self, _name: &str, _bytes: &[u8], _mode: u32) -> Result<String> {
        unsupported()
    }

    pub fn create_file(&self, _name: &str, _bytes: &[u8], _mode: u32) -> Result<()> {
        unsupported()
    }

    pub fn rename(&self, _from: &str, _to: &str) -> Result<()> {
        unsupported()
    }

    pub fn rename_new(&self, _from: &str, _to: &str) -> Result<()> {
        unsupported()
    }

    pub fn remove_tree(&self, _name: &str) -> Result<()> {
        unsupported()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn safedir_is_refused_on_windows_until_part_2() {
        let d = TempDir::new("safedir-win");
        let refused = SafeError::Io(UNSUPPORTED.into());
        assert_eq!(SafeDir::open(d.path(), &[]).unwrap_err(), refused);
        assert_eq!(SafeDir::create(d.path(), &["x"], 0o700).unwrap_err(), refused);
        assert!(!d.path().join("x").exists());
    }
}
