//! Size-capped rotation of `~/.mirante/daemon.log`: past [`LOG_CAP`] the file becomes
//! `daemon.log.1` (replacing an older one) and a fresh file starts.
use std::io;
use std::path::{Path, PathBuf};

pub const LOG_CAP: u64 = 10 << 20;

/// `<path>.1`.
pub fn rotated(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".1");
    PathBuf::from(s)
}

/// Renames `path` to `<path>.1` when it is larger than `cap`. `Ok(true)` when rotated; a
/// missing file is not an error.
pub fn rotate_if_over(path: &Path, cap: u64) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && m.len() > cap => {
            std::fs::rename(path, rotated(path))?;
            Ok(true)
        }
        Ok(_) => Ok(false),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_dir::TestDir;

    #[test]
    fn rotates_only_past_the_cap_and_keeps_one_old_file() {
        let d = TestDir::new();
        let log = d.path().join("daemon.log");
        assert!(!rotate_if_over(&log, 10).unwrap());
        std::fs::write(&log, b"0123456789").unwrap();
        assert!(!rotate_if_over(&log, 10).unwrap());
        std::fs::write(&log, b"0123456789a").unwrap();
        assert!(rotate_if_over(&log, 10).unwrap());
        assert!(!log.exists());
        assert_eq!(std::fs::read(rotated(&log)).unwrap(), b"0123456789a");
        std::fs::write(&log, b"second-generation").unwrap();
        assert!(rotate_if_over(&log, 10).unwrap());
        assert_eq!(std::fs::read(rotated(&log)).unwrap(), b"second-generation");
    }
}
