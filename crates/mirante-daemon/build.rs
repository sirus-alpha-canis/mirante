//! Embeds the build identifier (`MIRANTE_BUILD`): `<commit>[-dirty].<unix seconds>`, which
//! `mirante-daemon --version` prints and `daemon.status` reports, so the app and
//! `mirante doctor` can tell a running daemon from a newer binary on disk.
//!
//! Deterministic-friendly: `MIRANTE_BUILD_ID` is used verbatim when set, and the time is
//! `SOURCE_DATE_EPOCH` when set. Without git the commit is `unknown`.
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(dir).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}

fn main() {
    println!("cargo:rerun-if-env-changed=MIRANTE_BUILD_ID");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    // Whatever goes into the daemon: its own sources and the crates it is built from.
    for p in ["build.rs", "Cargo.toml", "src", "../mirante-core/src", "../mirante-ipc/src", "../../Cargo.lock"] {
        println!("cargo:rerun-if-changed={p}");
    }
    if let Ok(id) = std::env::var("MIRANTE_BUILD_ID") {
        if !id.trim().is_empty() {
            println!("cargo:rustc-env=MIRANTE_BUILD={}", id.trim());
            return;
        }
    }
    // A new commit (or a staged change) is a new build too.
    if let Some(git_dir) = git(&dir, &["rev-parse", "--absolute-git-dir"]) {
        let git_dir = Path::new(&git_dir);
        let refs = git(&dir, &["symbolic-ref", "-q", "HEAD"]);
        // Only files that exist: cargo counts a missing one as changed on every build.
        for p in
            ["HEAD", "index", "packed-refs"].into_iter().map(|f| git_dir.join(f)).chain(refs.map(|r| git_dir.join(r)))
        {
            if p.exists() {
                println!("cargo:rerun-if-changed={}", p.display());
            }
        }
    }
    let commit = git(&dir, &["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&dir, &["--no-optional-locks", "status", "--porcelain", "--untracked-files=no"])
        .is_some_and(|s| !s.is_empty());
    let secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
    println!("cargo:rustc-env=MIRANTE_BUILD={commit}{}.{secs}", if dirty { "-dirty" } else { "" });
}
