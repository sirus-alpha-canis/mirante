//! The hook-loss marker (`<home>/hook-lost`). `mirante-hook` appends one line to it
//! whenever a hook event did not reach the daemon live: it was spooled (daemon down, busy
//! or slow), the circuit breaker tripped, or the event was lost outright. The daemon takes
//! the marker's [`fingerprint`] when a Claude permission card is created and refuses to type
//! an answer for that card once the fingerprint changed: a hook it never saw (a newer
//! `PermissionRequest`, a `PreToolUse`) may mean the dialog on screen is another one.
//!
//! The file only grows (one short line per loss), so any loss changes its length; past
//! [`CAP`] it is rewritten to a single line, which changes the length too.
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

use crate::Paths;

/// The marker is rewritten to one line once it reaches this size.
pub const CAP: u64 = 64 << 10;

/// What identifies the marker's state: absent, or (length, modification time in ns,
/// inode). Any recorded loss changes it.
pub type Fingerprint = Option<(u64, i128, u64)>;

/// Records one loss (`what`: "spooled", "breaker", "lost"). Best effort: errors are
/// returned for the caller to ignore or report; a hook never fails because of it.
pub fn record(paths: &Paths, what: &str, ts_ms: i64) -> io::Result<()> {
    paths.ensure_home()?;
    let path = paths.hook_lost();
    let full = std::fs::metadata(&path).map(|m| m.len() >= CAP).unwrap_or(false);
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).mode(0o600);
    if full {
        opts.write(true).truncate(true);
    } else {
        opts.append(true);
    }
    let mut f = opts.open(&path)?;
    f.write_all(format!("{ts_ms} {} {what}\n", std::process::id()).as_bytes())
}

/// The marker's current state (see [`Fingerprint`]).
pub fn fingerprint(paths: &Paths) -> Fingerprint {
    let m = std::fs::metadata(paths.hook_lost()).ok()?;
    let mtime = i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec());
    Some((m.len(), mtime, m.ino()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_dir::TestDir;

    #[test]
    fn every_recorded_loss_changes_the_fingerprint() {
        let d = TestDir::new();
        let paths = Paths::new(&d.path().join("home"));
        assert_eq!(fingerprint(&paths), None);
        record(&paths, "spooled", 1).unwrap();
        let a = fingerprint(&paths);
        assert!(a.is_some());
        record(&paths, "breaker", 2).unwrap();
        let b = fingerprint(&paths);
        assert_ne!(a, b);
        assert_eq!(fingerprint(&paths), b, "reading does not change it");
    }

    #[test]
    fn a_full_marker_is_rewritten_and_still_changes() {
        let d = TestDir::new();
        let paths = Paths::new(&d.path().join("home"));
        paths.ensure_home().unwrap();
        std::fs::write(paths.hook_lost(), vec![b'x'; CAP as usize]).unwrap();
        let before = fingerprint(&paths);
        record(&paths, "lost", 3).unwrap();
        let after = fingerprint(&paths);
        assert_ne!(before, after);
        assert!(std::fs::metadata(paths.hook_lost()).unwrap().len() < 100);
    }
}
