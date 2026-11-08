//! Anchor checks against git: is the cited code still there, moved, changed or gone?
//!
//! Every call runs through the hardened [`Git`]: the daemon runs these in checkouts that
//! agents can write, outside their sandbox. Anchors are validated before any of their
//! fields reaches git; revisions follow `--end-of-options`; paths are passed as
//! `<commit>:<path>` object names on `cat-file`'s stdin, or with `--literal-pathspecs`,
//! never as globs. One [`check_all`] runs a fixed number of git processes: `rev-parse` per
//! distinct cited commit, one `diff` per distinct commit, then one `cat-file --batch-check`
//! and one `cat-file --batch` for every blob it reads.
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use serde::{Deserialize, Serialize};

use super::anchor::{check_path, fingerprint, locate_with, slice, Anchor, Located};
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
    #[cfg(test)]
    SPAWNS.with(|n| n.set(n.get() + 1));
    let out = Git::new(repo).args(args).output().map_err(|e| DnaError::Git(format!("git: {e}")))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(DnaError::Git(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())))
    }
}

/// Runs hardened git in `repo` with `input` on stdin (written from another thread, so a
/// large input cannot deadlock against a full stdout pipe).
fn git_with_input(repo: &Path, args: &[&str], input: Vec<u8>) -> Result<Vec<u8>, DnaError> {
    #[cfg(test)]
    SPAWNS.with(|n| n.set(n.get() + 1));
    let fail = |e: std::io::Error| DnaError::Git(format!("git: {e}"));
    let mut cmd = Git::new(repo).args(args).command();
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(fail)?;
    let mut stdin = child.stdin.take().ok_or_else(|| DnaError::Git("git: no stdin".into()))?;
    let out = std::thread::scope(|s| {
        s.spawn(move || {
            let _ = stdin.write_all(&input);
        });
        child.wait_with_output()
    })
    .map_err(fail)?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(DnaError::Git(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())))
    }
}

fn is_full_sha(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// The text of each `(commit, path)` blob, in order: `Ok(None)` when the name does not
/// resolve (no such path at that commit, or no such commit), an error when it is larger
/// than [`MAX_CITED_FILE`], not a regular file, or not UTF-8. Two git processes in all:
/// `cat-file --batch-check` for every name, then `cat-file --batch` for the blobs to read.
fn read_blobs(repo: &Path, names: &[(String, String)]) -> Result<Vec<Result<Option<String>, DnaError>>, DnaError> {
    for (_, path) in names {
        check_path(path)?; // no control characters (one name per line), nothing sensitive
    }
    read_objects(repo, names)
}

/// [`read_blobs`] without the citable-path check: for Mirante's own files (`.mirante/…`).
/// Every path must be free of control characters.
fn read_objects(repo: &Path, names: &[(String, String)]) -> Result<Vec<Result<Option<String>, DnaError>>, DnaError> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let mut input = Vec::new();
    for (commit, path) in names {
        if path.chars().any(char::is_control) {
            return Err(DnaError::Git(format!("{path:?} has a control character")));
        }
        if !is_full_sha(commit) {
            return Err(DnaError::Git(format!("{commit:?} is not a full sha")));
        }
        input.extend_from_slice(format!("{commit}:{path}\n").as_bytes());
    }
    let out = git_with_input(repo, &["cat-file", "--batch-check"], input)?;
    let headers: Vec<&[u8]> = out.split(|b| *b == b'\n').take(names.len()).collect();
    if headers.len() != names.len() {
        return Err(DnaError::Git("unexpected cat-file --batch-check output".into()));
    }
    let mut results: Vec<Result<Option<String>, DnaError>> = Vec::with_capacity(names.len());
    let mut wanted: Vec<(usize, String, usize)> = Vec::new();
    for (i, ((_, path), header)) in names.iter().zip(headers).enumerate() {
        let header = String::from_utf8_lossy(header);
        let fields: Vec<&str> = header.split(' ').collect();
        let result = match fields[..] {
            [sha, kind, size] if is_full_sha(sha) => match (kind, size.parse::<u64>()) {
                ("blob", Ok(n)) if n <= MAX_CITED_FILE => {
                    wanted.push((i, sha.to_string(), n as usize));
                    Ok(None) // filled in below
                }
                ("blob", Ok(_)) => Err(DnaError::Git(format!("{path} is larger than {MAX_CITED_FILE} bytes"))),
                _ => Err(DnaError::Git(format!("{path} is not a file"))),
            },
            _ if header.ends_with(" missing") => Ok(None),
            _ => Err(DnaError::Git(format!("cannot read {path}: {header}"))),
        };
        results.push(result);
    }
    if wanted.is_empty() {
        return Ok(results);
    }
    let input: Vec<u8> = wanted.iter().flat_map(|(_, sha, _)| format!("{sha}\n").into_bytes()).collect();
    let out = git_with_input(repo, &["cat-file", "--batch"], input)?;
    let mut rest: &[u8] = &out;
    let short = || DnaError::Git("unexpected cat-file --batch output".into());
    for (i, sha, size) in wanted {
        let nl = rest.iter().position(|b| *b == b'\n').ok_or_else(short)?;
        if !rest[..nl].starts_with(sha.as_bytes()) || rest.len() < nl + 1 + size {
            return Err(short());
        }
        let body = &rest[nl + 1..nl + 1 + size];
        rest = rest.get(nl + 2 + size..).unwrap_or_default();
        let path = &names[i].1;
        results[i] =
            String::from_utf8(body.to_vec()).map(Some).map_err(|_| DnaError::Git(format!("{path} is a binary file")));
    }
    Ok(results)
}

/// A file of `.mirante/memory` at a commit: its name, and its text or why it was not read.
pub type MemoryFile = (String, Result<String, DnaError>);

/// The note files of `.mirante/memory` as committed at `commit` (a full sha): each regular
/// file directly in that directory (symlinks, submodules and subdirectories skipped), by
/// name, with its text or why it could not be read. At most `limit` entries are listed.
/// Two or three git processes: one `ls-tree`, then [`read_blobs`].
pub fn memory_files_at(repo: &Path, commit: &str, limit: usize) -> Result<Vec<MemoryFile>, DnaError> {
    if !is_full_sha(commit) {
        return Err(DnaError::Git(format!("{commit:?} is not a full sha")));
    }
    let dir = format!("{}/", super::MEMORY_DIR);
    let out = git(repo, &["ls-tree", "-z", "--full-tree", "--end-of-options", commit, "--", &dir])?;
    let mut names = Vec::new();
    for entry in out.split(|b| *b == 0).filter(|e| !e.is_empty()).take(limit) {
        let entry = String::from_utf8_lossy(entry);
        let Some((meta, path)) = entry.split_once('\t') else { continue };
        let regular = matches!(meta.split(' ').collect::<Vec<_>>()[..], ["100644" | "100755", "blob", _]);
        match path.strip_prefix(&dir) {
            Some(name) if regular && !name.is_empty() && !name.contains('/') => names.push(name.to_string()),
            _ => {}
        }
    }
    // A name with a control character is never read (one name per line on cat-file's stdin).
    let readable = |n: &String| !n.chars().any(char::is_control);
    let wanted: Vec<(String, String)> =
        names.iter().filter(|n| readable(n)).map(|n| (commit.to_string(), format!("{dir}{n}"))).collect();
    let mut read = read_objects(repo, &wanted)?.into_iter();
    Ok(names
        .into_iter()
        .map(|n| {
            let text = if readable(&n) {
                read.next()
                    .unwrap_or(Ok(None))
                    .and_then(|t| t.ok_or_else(|| DnaError::Git(format!("{n} is missing at {commit}"))))
            } else {
                Err(DnaError::Note("the file name has a control character".into()))
            };
            (n, text)
        })
        .collect())
}

#[cfg(test)]
thread_local! {
    /// git processes this thread started through this module (tests count them).
    static SPAWNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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
    let read = read_blobs(repo, &[(commit.to_string(), path.to_string())])?.pop().unwrap_or(Ok(None));
    match read {
        Ok(None) if !commit_exists(repo, commit) => Err(DnaError::Git(format!("commit {commit} does not exist"))),
        other => other,
    }
}

/// Whether `path` has uncommitted changes (or is untracked) in `worktree`.
pub fn is_dirty(worktree: &Path, path: &str) -> Result<bool, DnaError> {
    check_path(path)?;
    let out = git(
        worktree,
        &["--literal-pathspecs", "status", "--porcelain=v1", "-z", "--ignore-submodules=all", "--", path],
    )?;
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

/// States of `anchors` against `rev`. An anchor that fails [`Anchor::validate`] is
/// Unverifiable and never reaches git. One `diff --name-only -z <commit> <rev>` per
/// distinct commit, then every blob read in one batch: each anchor's cited lines at its own
/// commit (re-verified against the fingerprint, so a hand-edited anchor is Unverifiable:
/// "anchor does not match its commit"), plus the file at `rev` for changed paths. A commit
/// that no longer exists → Unverifiable.
pub fn check_all(repo: &Path, rev: &str, anchors: &[Anchor]) -> Result<Vec<Checked>, DnaError> {
    let head = rev_parse(repo, rev)?;
    let invalid: Vec<Option<String>> = anchors.iter().map(|a| a.validate().err().map(|e| e.to_string())).collect();
    let valid = || anchors.iter().zip(&invalid).filter(|(_, e)| e.is_none()).map(|(a, _)| a);
    // Per cited commit: the paths changed between it and `head`, or None if it is gone.
    let mut changed: HashMap<&str, Option<HashSet<String>>> = HashMap::new();
    for a in valid() {
        if changed.contains_key(a.commit.as_str()) {
            continue;
        }
        let paths = if commit_exists(repo, &a.commit) {
            let out = git(
                repo,
                &[
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-renames",
                    "--name-only",
                    "-z",
                    "--end-of-options",
                    &a.commit,
                    &head,
                ],
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
    // Every blob to read, once.
    let mut names: Vec<(String, String)> = Vec::new();
    let mut slot: HashMap<(String, String), usize> = HashMap::new();
    let mut want = |commit: &str, path: &str| {
        let key = (commit.to_string(), path.to_string());
        if !slot.contains_key(&key) {
            slot.insert(key.clone(), names.len());
            names.push(key);
        }
    };
    for a in valid() {
        if let Some(Some(paths)) = changed.get(a.commit.as_str()) {
            want(&a.commit, &a.path);
            if paths.contains(&a.path) {
                want(&head, &a.path);
            }
        }
    }
    let blobs = read_blobs(repo, &names)?;
    let read = |commit: &str, path: &str| blobs[slot[&(commit.to_string(), path.to_string())]].clone();
    let mut out = Vec::with_capacity(anchors.len());
    for (a, invalid) in anchors.iter().zip(invalid) {
        let state = match (invalid, changed.get(a.commit.as_str())) {
            (Some(e), _) => AnchorState::Unverifiable { reason: format!("invalid anchor: {e}") },
            (None, Some(Some(paths))) => {
                check_one(a, paths.contains(&a.path), read(&a.commit, &a.path), || read(&head, &a.path))
            }
            (None, _) => {
                AnchorState::Unverifiable { reason: format!("the cited commit {} no longer exists", short(&a.commit)) }
            }
        };
        out.push(Checked { anchor: a.clone(), state });
    }
    Ok(out)
}

/// One anchor's state from its blob at its own commit and, when the path changed, at HEAD.
fn check_one(
    a: &Anchor,
    path_changed: bool,
    at_commit: Result<Option<String>, DnaError>,
    at_head: impl FnOnce() -> Result<Option<String>, DnaError>,
) -> AnchorState {
    let mismatch = || AnchorState::Unverifiable {
        reason: format!("anchor does not match its commit {} (edited by hand?)", short(&a.commit)),
    };
    let cited_text = match at_commit {
        Ok(Some(text)) => text,
        Ok(None) => return mismatch(),
        Err(e) => return AnchorState::Unverifiable { reason: e.to_string() },
    };
    let Some(cited) = slice(&cited_text, a.start, a.end).filter(|w| fingerprint(w) == a.fingerprint) else {
        return mismatch();
    };
    if !path_changed {
        return AnchorState::Fresh;
    }
    match at_head() {
        Ok(None) => AnchorState::Stale { reason: "file deleted or renamed".into() },
        Ok(Some(text)) => match locate_with(a, Some(&cited), &text) {
            Located::Same => AnchorState::Fresh,
            Located::Moved { start, end } => AnchorState::Relocated { start, end },
            Located::Changed { reason, .. } => AnchorState::Stale { reason },
        },
        Err(e) => AnchorState::Stale { reason: e.to_string() },
    }
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
    fn a_planted_replace_ref_cannot_make_a_stale_anchor_fresh() {
        let r = repo_with(&[("a.rs", "one\n")]);
        let c1 = head(&r);
        let a = cite(&r, &c1, "a.rs", None, 1, 1).unwrap();
        commit_file(&r, "a.rs", "two\n");
        let c2 = head(&r);
        plain(&r, &["replace", &c2, &c1]);
        let rep = check_all(&r, "HEAD", &[a]).unwrap();
        assert!(matches!(rep[0].state, AnchorState::Stale { .. }), "{:?}", rep[0].state);
    }

    #[test]
    fn an_invalid_anchor_is_unverifiable_and_never_reaches_git_argv() {
        let r = repo_with(&[("a.rs", "x\n")]);
        let a = cite(&r, &head(&r), "a.rs", None, 1, 1).unwrap();
        let evil = Anchor { commit: format!("--output={}", r.join("PWNED").display()), ..a.clone() };
        let rep = check_all(&r, "HEAD", &[evil, a]).unwrap();
        assert!(matches!(&rep[0].state, AnchorState::Unverifiable { reason } if reason.contains("invalid")));
        assert_eq!(rep[1].state, AnchorState::Fresh);
        assert!(!r.join("PWNED").exists());
    }

    #[test]
    fn check_all_reads_blobs_in_one_batch() {
        let files: Vec<(String, String)> = (0..6).map(|i| (format!("f{i}.rs"), format!("line {i}\n"))).collect();
        let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
        let r = repo_with(&refs);
        let c = head(&r);
        let anchors: Vec<Anchor> = files.iter().map(|(p, _)| cite(&r, &c, p, None, 1, 1).unwrap()).collect();
        commit_file(&r, "f0.rs", "changed\n");
        SPAWNS.with(|n| n.set(0));
        let rep = check_all(&r, "HEAD", &anchors).unwrap();
        assert!(matches!(rep[0].state, AnchorState::Stale { .. }));
        assert!(rep[1..].iter().all(|c| c.state == AnchorState::Fresh));
        // rev-parse HEAD, rev-parse the cited commit, one diff, then one batch-check and one batch.
        assert!(SPAWNS.with(|n| n.get()) <= 5, "{} git calls", SPAWNS.with(|n| n.get()));
    }

    #[test]
    fn submodule_changes_do_not_make_a_path_dirty() {
        let sub = repo_with(&[("s.rs", "s\n")]);
        let r = repo_with(&[("a.rs", "x\n")]);
        let url = sub.to_str().unwrap();
        plain(&r, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", url, "sub"]);
        plain(&r, &["commit", "-q", "-m", "sub"]);
        std::fs::write(r.join("sub/s.rs"), "edited\n").unwrap();
        assert!(!is_dirty(&r, "sub").unwrap());
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
