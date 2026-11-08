//! Test-only: fake executables (`#!/bin/sh` scripts) for tests that time a child process.
//!
//! On macOS the first exec of a freshly written file can take seconds (XProtect/Gatekeeper
//! scan it), which a test with a tight timeout reads as a hang. [`write_sh`] runs the new
//! script once, untimed, before returning: a guard line makes that warm-up exit before the
//! body, so the body (which may hang, log its argv or need stdin) only runs when the test
//! runs it.
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// Set only for the warm-up run; the script exits on it before its body.
const WARM_UP: &str = "MIRANTE_TEST_SCRIPT_WARM_UP";

/// Writes `#!/bin/sh` + `body` to `path` with `mode`, then runs it once (untimed, body
/// skipped) so the test's own runs are not charged for the first-exec scan. Panics on error.
pub fn write_sh(path: &Path, body: &str, mode: u32) {
    std::fs::write(path, format!("#!/bin/sh\n[ -n \"${WARM_UP}\" ] && exit 0\n{body}\n")).expect("write script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod script");
    let warm = Command::new(path)
        .env(WARM_UP, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("warm up script");
    assert!(warm.success(), "warm-up of {} failed: {warm}", path.display());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn the_warm_up_skips_the_body_and_a_real_run_does_not() {
        let d = crate::test_support::TempDir::new("test-script");
        let log = d.path().join("ran");
        let bin = d.path().join("fake");
        let t = Instant::now();
        write_sh(&bin, &format!("echo \"$@\" >> '{}'\nsleep 30", log.display()), 0o700);
        assert!(t.elapsed() < Duration::from_secs(20), "the warm-up ran the body: {:?}", t.elapsed());
        assert!(!log.exists(), "the warm-up ran the body");
        assert_eq!(std::fs::metadata(&bin).unwrap().permissions().mode() & 0o777, 0o700);

        write_sh(&bin, &format!("echo \"$@\" >> '{}'", log.display()), 0o755);
        assert!(Command::new(&bin).args(["a", "b"]).status().unwrap().success());
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "a b\n");
    }
}
