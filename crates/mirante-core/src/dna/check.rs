//! Anchor checks against git: is the cited code still there, moved, changed or gone?
//!
//! Every call runs through the hardened [`Git`]: the daemon runs these in checkouts that
//! agents can write, outside their sandbox. Paths are passed as `<commit>:<path>` object
//! names, or with `--literal-pathspecs`, never as globs.
use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::anchor::{check_path, fingerprint, locate, slice, Anchor, Located};
use super::{DnaError, MAX_CITED_FILE};
use crate::git::Git;

/// What a check found for one anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AnchorState {
    Unchecked,
    Fresh,
    Relocated { start: u32, end: u32 },
    Stale { reason: String },
    Unverifiable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub anchor: Anchor,
    pub state: AnchorState,
}

/// Runs hardened git in `repo`; stdout on success, else `DnaError::Git` with stderr.
fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>, DnaError> {
    let out = Git::new(repo).args(args).output().map_err(|e| DnaError::Git(format!("git: {e}")))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(DnaError::Git(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())))
    }
}

/// The full sha of the commit `rev` names.
pub fn rev_parse(repo: &Path, rev: &str) -> Result<String, DnaError> {
    let spec = format!("{rev}^{{commit}}");
    let out = git(repo, &["rev-parse", "--verify", "--quiet", "--end-of-options", &spec])
        .map_err(|_| DnaError::Git(format!("{rev:?} is not a commit in this repository")))?;
    let sha = String::from_utf8_lossy(&out).trim().to_string();
    if matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(sha)
    } else {
        Err(DnaError::Git(format!("unexpected rev-parse output for {rev:?}")))
    }
}

fn commit_exists(repo: &Path, commit: &str) -> bool {
    rev_parse(repo, commit).is_ok()
}

/// The text of `path` at `commit` (a full sha), `None` when the commit has no such path.
/// Errors: the commit does not exist, the file is larger than [`MAX_CITED_FILE`], it is not
/// a regular file, or it is not UTF-8.
pub fn blob(repo: &Path, commit: &str, path: &str) -> Result<Option<String>, DnaError> {
    check_path(path)?;
    let object = format!("{commit}:{path}");
    let size = match git(repo, &["cat-file", "-s", &object]) {
        Ok(out) => String::from_utf8_lossy(&out).trim().parse::<u64>().map_err(|_| DnaError::Git("bad size".into()))?,
        Err(e) if commit_exists(repo, commit) => {
            return if e.to_string().contains("does not exist") || e.to_string().contains("but not in") {
                Ok(None)
            } else {
                Err(e)
            }
        }
        Err(_) => return Err(DnaError::Git(format!("commit {commit} does not exist"))),
    };
    if size > MAX_CITED_FILE {
        return Err(DnaError::Git(format!("{path} is larger than {MAX_CITED_FILE} bytes")));
    }
    let out = git(repo, &["cat-file", "blob", &object]).map_err(|_| DnaError::Git(format!("{path} is not a file")))?;
    String::from_utf8(out).map(Some).map_err(|_| DnaError::Git(format!("{path} is a binary file")))
}

/// Whether `path` has uncommitted changes (or is untracked) in `worktree`.
pub fn is_dirty(worktree: &Path, path: &str) -> Result<bool, DnaError> {
    check_path(path)?;
    let out = git(worktree, &["--literal-pathspecs", "status", "--porcelain=v1", "-z", "--", path])?;
    Ok(!out.is_empty())
}

/// A new anchor over committed code: validates path and range, fingerprints the blob.
pub fn cite(
    repo: &Path,
    commit: &str,
    path: &str,
    symbol: Option<&str>,
    start: u32,
    end: u32,
) -> Result<Anchor, DnaError> {
    check_path(path)?;
    let commit = rev_parse(repo, commit)?;
    let text = blob(repo, &commit, path)?
        .ok_or_else(|| DnaError::Path(path.to_string(), "not in the cited commit (commit it first)"))?;
    let lines = slice(&text, start, end).ok_or_else(|| {
        DnaError::Anchor(format!("{path}:L{start}-{end}"), "lines are outside the file at the cited commit")
    })?;
    let a = Anchor {
        path: path.to_string(),
        symbol: symbol.map(str::to_string),
        start,
        end,
        commit,
        fingerprint: fingerprint(&lines),
    };
    // Every grammar rule (symbol charset, range length) in one place.
    Anchor::parse(&a.render())
}

/// States of `anchors` against `rev`. One `diff --name-only -z <commit> <rev>` per distinct
/// commit; only changed paths are read (one `cat-file` each). A commit that no longer
/// exists → Unverifiable; a blob at its own commit whose lines do not match the fingerprint
/// → Unverifiable ("anchor does not match its commit": the frontmatter was edited by hand).
pub fn check_all(repo: &Path, rev: &str, anchors: &[Anchor]) -> Result<Vec<Checked>, DnaError> {
    let head = rev_parse(repo, rev)?;
    // Per cited commit: the paths changed between it and `head`, or None if it is gone.
    let mut changed: HashMap<&str, Option<HashSet<String>>> = HashMap::new();
    for a in anchors {
        if changed.contains_key(a.commit.as_str()) {
            continue;
        }
        let paths = if commit_exists(repo, &a.commit) {
            let out = git(
                repo,
                &["diff", "--no-ext-diff", "--no-textconv", "--no-renames", "--name-only", "-z", &a.commit, &head],
            )?;
            Some(
                out.split(|b| *b == 0)
                    .filter(|p| !p.is_empty())
                    .map(|p| String::from_utf8_lossy(p).into_owned())
                    .collect(),
            )
        } else {
            None
        };
        changed.insert(&a.commit, paths);
    }
    let mut blobs: HashMap<(String, String), Result<Option<String>, DnaError>> = HashMap::new();
    let mut read = |commit: &str, path: &str| {
        blobs.entry((commit.to_string(), path.to_string())).or_insert_with(|| blob(repo, commit, path)).clone()
    };
    let mut out = Vec::with_capacity(anchors.len());
    for a in anchors {
        let state = match &changed[a.commit.as_str()] {
            None => {
                AnchorState::Unverifiable { reason: format!("the cited commit {} no longer exists", short(&a.commit)) }
            }
            Some(paths) if !paths.contains(&a.path) => match read(&a.commit, &a.path) {
                Ok(Some(text)) if slice(&text, a.start, a.end).is_some_and(|w| fingerprint(&w) == a.fingerprint) => {
                    AnchorState::Fresh
                }
                Ok(_) => AnchorState::Unverifiable {
                    reason: format!("anchor does not match its commit {} (edited by hand?)", short(&a.commit)),
                },
                Err(e) => AnchorState::Unverifiable { reason: e.to_string() },
            },
            Some(_) => match read(&head, &a.path) {
                Ok(None) => AnchorState::Stale { reason: "file deleted or renamed".into() },
                Ok(Some(text)) => match locate(a, &text) {
                    Located::Same => AnchorState::Fresh,
                    Located::Moved { start, end } => AnchorState::Relocated { start, end },
                    Located::Changed { reason, .. } => AnchorState::Stale { reason },
                },
                Err(e) => AnchorState::Stale { reason: e.to_string() },
            },
        };
        out.push(Checked { anchor: a.clone(), state });
    }
    Ok(out)
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;
    use std::process::Command;

    struct Repo(TempDir);

    impl std::ops::Deref for Repo {
        type Target = Path;
        fn deref(&self) -> &Path {
            self.0.path()
        }
    }

    /// Unhardened git, for test setup only.
    fn plain(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn commit_file(r: &Path, path: &str, text: &str) {
        let full = r.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
        plain(r, &["add", "-A"]);
        plain(r, &["commit", "-q", "-m", path]);
    }

    fn repo_with(files: &[(&str, &str)]) -> Repo {
        let r = Repo(TempDir::new("dna-check"));
        plain(&r, &["init", "-q", "-b", "main"]);
        for (p, t) in files {
            commit_file(&r, p, t);
        }
        r
    }

    fn head(r: &Path) -> String {
        rev_parse(r, "HEAD").unwrap()
    }

    #[test]
    fn fresh_relocated_stale_unverifiable() {
        let r = repo_with(&[("src/a.rs", "fn keep() {}\nfn spool() {\n  write();\n}\n")]);
        let c1 = head(&r);
        let spool = cite(&r, &c1, "src/a.rs", Some("spool"), 2, 4).unwrap(); // reads blob at c1
        let keep = cite(&r, &c1, "src/a.rs", Some("keep"), 1, 1).unwrap();
        commit_file(&r, "src/a.rs", "// moved\nfn keep() {}\nfn spool() {\n  write();\n}\n");
        let rep = check_all(&r, "HEAD", &[spool.clone(), keep.clone()]).unwrap();
        assert_eq!(rep[0].state, AnchorState::Relocated { start: 3, end: 5 });
        commit_file(&r, "src/a.rs", "fn keep() {}\nfn spool() {\n  write_twice();\n}\n");
        let rep = check_all(&r, "HEAD", &[spool.clone(), keep]).unwrap();
        assert!(matches!(rep[0].state, AnchorState::Stale { .. }));
        assert_eq!(rep[1].state, AnchorState::Fresh);
        let ghost = Anchor { commit: "1".repeat(40), ..spool };
        assert!(matches!(check_all(&r, "HEAD", &[ghost]).unwrap()[0].state, AnchorState::Unverifiable { .. }));
    }

    #[test]
    fn deleted_file_is_stale_and_unchanged_path_is_verified_at_its_commit() {
        let r = repo_with(&[("a.rs", "one\ntwo\n"), ("b.rs", "keep\n")]);
        let c = head(&r);
        let a = cite(&r, &c, "a.rs", None, 1, 2).unwrap();
        let b = cite(&r, &c, "b.rs", None, 1, 1).unwrap();
        plain(&r, &["rm", "-q", "a.rs"]);
        plain(&r, &["commit", "-q", "-m", "rm"]);
        let rep = check_all(&r, "HEAD", &[a, b.clone()]).unwrap();
        assert!(matches!(&rep[0].state, AnchorState::Stale { reason } if reason.contains("deleted")), "{:?}", rep[0]);
        assert_eq!(rep[1].state, AnchorState::Fresh);
        // Unchanged path, but the fingerprint was edited by hand: it does not match its commit.
        let forged = Anchor { fingerprint: "0123456789abcdef".into(), ..b };
        let rep = check_all(&r, "HEAD", &[forged]).unwrap();
        assert!(matches!(&rep[0].state, AnchorState::Unverifiable { reason } if reason.contains("does not match")));
    }

    #[test]
    fn cite_refuses_out_of_range_lines_missing_files_and_big_files() {
        let r = repo_with(&[("a.rs", "1\n2\n3\n4\n"), ("big.txt", &"x".repeat(MAX_CITED_FILE as usize + 1))]);
        let c = head(&r);
        assert!(cite(&r, &c, "a.rs", None, 9, 10).is_err());
        assert!(cite(&r, &c, "a.rs", None, 3, 5).is_err());
        assert!(cite(&r, &c, "missing.rs", None, 1, 1).is_err());
        assert!(cite(&r, &c, "big.txt", None, 1, 1).is_err());
        assert!(cite(&r, &c, "../a.rs", None, 1, 1).is_err());
        assert!(cite(&r, &c, "a.rs", Some("bad symbol"), 1, 1).is_err());
        assert!(cite(&r, &"2".repeat(40), "a.rs", None, 1, 1).is_err());
        let a = cite(&r, "HEAD", "a.rs", None, 2, 3).unwrap();
        assert_eq!((a.commit.as_str(), a.fingerprint.as_str()), (c.as_str(), fingerprint(&["2", "3"]).as_str()));
    }

    #[test]
    fn a_planted_fsmonitor_never_runs() {
        let r = repo_with(&[("a.rs", "x\n")]);
        plain(&r, &["config", "core.fsmonitor", "touch PWNED"]);
        let a = cite(&r, &head(&r), "a.rs", None, 1, 1).unwrap();
        check_all(&r, "HEAD", &[a]).unwrap();
        is_dirty(&r, "a.rs").unwrap();
        assert!(!r.join("PWNED").exists());
    }

    #[test]
    fn dirty_path_is_reported() {
        let r = repo_with(&[("a.rs", "x\n")]);
        assert!(!is_dirty(&r, "a.rs").unwrap());
        std::fs::write(r.join("a.rs"), "y\n").unwrap();
        assert!(is_dirty(&r, "a.rs").unwrap());
        std::fs::write(r.join("new.rs"), "n\n").unwrap();
        assert!(is_dirty(&r, "new.rs").unwrap());
        // Paths are literal, never globs: a clean `[a].rs` is clean while `a.rs` is dirty.
        commit_file(&r, "[a].rs", "glob\n");
        std::fs::write(r.join("a.rs"), "z\n").unwrap();
        assert!(!is_dirty(&r, "[a].rs").unwrap());
    }
}
