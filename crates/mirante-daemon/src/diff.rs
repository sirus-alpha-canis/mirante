//! `task.diff`: what a task branch changed against its base, read with git. Read-only: it
//! never writes to the repo or the worktree. Uncommitted changes count (the agent may not
//! have committed yet); untracked files are listed by name.
//!
//! The worktree is agent-written: git runs through the hardened runner
//! ([`mirante_core::git`]), diffs never run an external diff driver or textconv, and every
//! output is bounded (the patch and the file lists are streamed and cut, the process killed).
use std::io::Read;
use std::path::Path;
use std::process::Stdio;

use mirante_core::git::Git;
use mirante_ipc::types::{DiffFile, TaskDiff};

/// Largest patch returned; the rest is cut (`truncated: true`).
pub const PATCH_CAP: usize = 512 * 1024;
/// Most entries in `files` and in `untracked` (`files_truncated: true` past it).
pub const FILES_CAP: usize = 2000;
/// Most bytes read from one file listing (numstat, ls-files) before it is cut.
const LIST_CAP: usize = 4 * 1024 * 1024;
/// Most stderr bytes kept for an error message.
const STDERR_CAP: u64 = 16 * 1024;

/// Options every `git diff` here takes: no external driver, no textconv, no color, no
/// rename detection.
const DIFF_OPTS: &[&str] = &["--no-ext-diff", "--no-textconv", "--no-color", "--no-renames"];

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Git::new(dir).args(args).output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// At most `cap` bytes of stdout, and whether there was more. A command with more output
/// is killed and reaped once `cap + 1` bytes are read: nothing past the cap is buffered.
pub fn read_capped(git: &Git, cap: usize) -> Result<(Vec<u8>, bool), String> {
    let mut child =
        git.command().stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("git: {e}"))?;
    let stderr = child.stderr.take();
    let errors = std::thread::spawn(move || {
        let mut kept = Vec::new();
        if let Some(mut e) = stderr {
            let _ = (&mut e).take(STDERR_CAP).read_to_end(&mut kept);
            let _ = std::io::copy(&mut e, &mut std::io::sink());
        }
        kept
    });
    let mut out = Vec::new();
    let read = child.stdout.take().map(|s| s.take(cap as u64 + 1).read_to_end(&mut out));
    let more = out.len() > cap;
    if more {
        out.truncate(cap);
        let _ = child.kill();
    }
    let status = child.wait().map_err(|e| format!("git: {e}"))?;
    let stderr = errors.join().unwrap_or_default();
    if let Some(Err(e)) = read {
        return Err(format!("git: {e}"));
    }
    if !more && !status.success() {
        return Err(format!("git: {} ({status})", String::from_utf8_lossy(&stderr).trim()));
    }
    Ok((out, more))
}

/// The NUL-terminated records of a `-z` listing. When the listing was cut (`cut`), its
/// last record may be partial and is dropped. At most [`FILES_CAP`]; the flag says
/// whether any record was left out.
fn records(bytes: &[u8], cut: bool) -> (Vec<String>, bool) {
    let mut all: Vec<&[u8]> = bytes.split(|b| *b == 0).collect();
    // The text after the last NUL: empty when whole, possibly partial when cut.
    all.pop();
    let mut out: Vec<String> = all.into_iter().map(|r| String::from_utf8_lossy(r).into_owned()).collect();
    let over = out.len() > FILES_CAP;
    out.truncate(FILES_CAP);
    (out, cut || over)
}

/// `git diff --numstat -z` records (`added\tremoved\tpath`; a rename, when detection is on,
/// is `added\tremoved\t` followed by the source and destination paths as records of their
/// own). A binary file (`-\t-\tpath`) has no counts.
pub fn parse_numstat(records: &[String]) -> Vec<DiffFile> {
    let mut out = Vec::new();
    let mut it = records.iter();
    while let Some(r) = it.next() {
        let mut parts = r.splitn(3, '\t');
        let (Some(a), Some(rm), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let path = match path.is_empty() {
            false => path.to_string(),
            true => match (it.next(), it.next()) {
                (Some(_from), Some(to)) => to.clone(),
                _ => break,
            },
        };
        out.push(DiffFile { path, added: a.parse().ok(), removed: rm.parse().ok() });
    }
    out
}

/// `bytes` as text of at most `cap` bytes, never splitting a character: an incomplete
/// trailing sequence (left by a cut) is dropped, other invalid bytes are replaced.
fn text_within(bytes: Vec<u8>, cap: usize) -> String {
    let mut s = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) if e.utf8_error().error_len().is_none() => {
            let valid = e.utf8_error().valid_up_to();
            let mut b = e.into_bytes();
            b.truncate(valid);
            String::from_utf8_lossy(&b).into_owned()
        }
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };
    if s.len() > cap {
        let mut cut = cap;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

/// The `owner/repo` part of a GitHub remote: `git@github.com:o/r.git`,
/// `https://github.com/o/r(.git)`, `ssh://git@github.com/o/r.git`, and the SSH host
/// aliases used for several accounts (`git@github.com-<alias>:o/r.git`). A URL carrying
/// credentials (`https://user:token@github.com/...`) never matches.
fn github_path(remote: &str) -> Option<&str> {
    let host_ok = |h: &str| {
        h == "github.com"
            || h.strip_prefix("github.com-")
                .is_some_and(|a| !a.is_empty() && a.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)))
    };
    if let Some(rest) = remote.strip_prefix("https://github.com/") {
        return Some(rest);
    }
    if let Some(rest) = remote.strip_prefix("ssh://git@") {
        let (host, path) = rest.split_once('/')?;
        return host_ok(host).then_some(path);
    }
    let (host, path) = remote.strip_prefix("git@")?.split_once(':')?;
    host_ok(host).then_some(path)
}

/// GitHub's "open a pull request" page for `branch` against `base`, when `remote` is a
/// GitHub remote ([`github_path`]; an SSH alias maps to `github.com`). Anything else is
/// `None`.
pub fn github_compare_url(remote: &str, base: &str, branch: &str) -> Option<String> {
    let path = github_path(remote)?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    let safe =
        |s: &str| !matches!(s, "" | "." | "..") && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
    if parts.next().is_some() || !safe(owner) || !safe(repo) {
        return None;
    }
    let enc = |s: &str| -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect()
    };
    Some(format!("https://github.com/{owner}/{repo}/compare/{}...{}?expand=1", enc(base), enc(branch)))
}

/// The diff of `worktree` (on `branch`) against its fork point from the repo checkout's
/// current branch, which is the base `task.create` uses by default (a detached checkout
/// falls back to its commit).
pub fn task_diff(repo: &Path, worktree: &Path, branch: &str) -> Result<TaskDiff, String> {
    let base = match git(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        Ok(b) => b.trim().to_string(),
        Err(_) => git(repo, &["rev-parse", "HEAD"])?.trim().to_string(),
    };
    let merge_base = git(worktree, &["merge-base", "--end-of-options", "HEAD", &base])?.trim().to_string();
    let wt = Git::new(worktree);
    let against = ["--end-of-options", merge_base.as_str(), "--"];

    let numstat = wt.clone().args(["diff", "--numstat", "-z"]).args(DIFF_OPTS).args(against);
    let (bytes, cut) = read_capped(&numstat, LIST_CAP)?;
    let (recs, files_cut) = records(&bytes, cut);
    let files = parse_numstat(&recs);

    let others = wt.clone().args(["ls-files", "-z", "--others", "--exclude-standard"]);
    let (bytes, cut) = read_capped(&others, LIST_CAP)?;
    let (untracked, untracked_cut) = records(&bytes, cut);

    // `--default-prefix`: git's `a/` `b/` whatever `diff.noprefix` / `diff.mnemonicPrefix` say.
    let patch = wt.clone().arg("diff").args(DIFF_OPTS).arg("--default-prefix").args(against);
    let (bytes, truncated) = read_capped(&patch, PATCH_CAP)?;
    let patch = text_within(bytes, PATCH_CAP);

    let compare_url =
        git(repo, &["remote", "get-url", "origin"]).ok().and_then(|u| github_compare_url(u.trim(), &base, branch));
    let remote_ref = format!("refs/remotes/origin/{branch}");
    let ahead = match git(worktree, &["rev-parse", "--verify", "--quiet", "--end-of-options", &remote_ref]) {
        Ok(_) => git(worktree, &["rev-list", "--count", &format!("{remote_ref}..HEAD")])
            .ok()
            .and_then(|n| n.trim().parse().ok()),
        Err(_) => None,
    };
    Ok(TaskDiff {
        base,
        merge_base,
        files,
        untracked,
        files_truncated: files_cut || untracked_cut,
        patch,
        truncated,
        compare_url,
        pushed: ahead == Some(0),
        ahead,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_ipc::test_dir::TestDir;

    fn recs(s: &str) -> Vec<String> {
        records(s.as_bytes(), false).0
    }

    #[test]
    fn numstat_records_parse_with_binary_files_uncounted_renames_and_unicode() {
        let numstat =
            ["3\t1\tsrc/a.rs", "-\t-\tlogo.png", "2\t0\t", "old name.rs", "caf\u{e9} \u{fc}.rs", ""].join("\0");
        let files = parse_numstat(&recs(&numstat));
        assert_eq!(
            files,
            vec![
                DiffFile { path: "src/a.rs".into(), added: Some(3), removed: Some(1) },
                DiffFile { path: "logo.png".into(), added: None, removed: None },
                DiffFile { path: "caf\u{e9} \u{fc}.rs".into(), added: Some(2), removed: Some(0) },
            ]
        );
        let odd = parse_numstat(&recs("1\t0\ta\tb\nc.txt\0"));
        assert_eq!(odd[0].path, "a\tb\nc.txt");
    }

    #[test]
    fn listings_drop_a_partial_last_record_and_cap_the_count() {
        assert_eq!(records(b"a\0b\0", false), (vec!["a".into(), "b".into()], false));
        assert_eq!(records(b"a\0b\0par", true), (vec!["a".into(), "b".into()], true));
        let many: Vec<u8> = (0..=FILES_CAP).flat_map(|i| format!("f{i}\0").into_bytes()).collect();
        let (got, cut) = records(&many, false);
        assert_eq!((got.len(), cut), (FILES_CAP, true));
        let exact: Vec<u8> = (0..FILES_CAP).flat_map(|i| format!("f{i}\0").into_bytes()).collect();
        assert!(!records(&exact, false).1);
    }

    #[test]
    fn a_cut_never_splits_a_character() {
        let s = "a\u{e9}".as_bytes().to_vec(); // 61 c3 a9
        assert_eq!(text_within(s[..2].to_vec(), 10), "a");
        assert_eq!(text_within(s.clone(), 2), "a");
        assert_eq!(text_within(s, 3), "a\u{e9}");
    }

    #[test]
    fn a_capped_read_kills_the_process_instead_of_draining_it() {
        let dir = TestDir::new();
        let init = std::process::Command::new("git").arg("-C").arg(dir.path()).args(["init", "-q"]).status();
        assert!(init.unwrap().success());
        // 8 MiB: far more than a pipe buffers, so an unkilled git would block and `wait` hang.
        std::fs::write(dir.path().join("big"), vec![b'x'; 8 << 20]).unwrap();
        let oid = Git::new(dir.path()).args(["hash-object", "-w", "big"]).output().unwrap();
        let oid = String::from_utf8(oid.stdout).unwrap();
        let show = Git::new(dir.path()).args(["cat-file", "blob", oid.trim()]);
        assert_eq!(read_capped(&show, 1024).unwrap(), (vec![b'x'; 1024], true));
        let ls = Git::new(dir.path()).args(["ls-files", "-z", "--others"]);
        assert_eq!(read_capped(&ls, 100).unwrap(), (b"big\0".to_vec(), false));
        let bad = Git::new(dir.path()).args(["rev-parse", "--verify", "nope"]);
        assert!(read_capped(&bad, 10).is_err());
    }

    #[test]
    fn compare_urls_only_for_github_remotes() {
        let want = Some("https://github.com/acme/app/compare/main...feat/x%23y?expand=1".to_string());
        assert_eq!(github_compare_url("git@github.com:acme/app.git", "main", "feat/x#y"), want);
        assert_eq!(github_compare_url("https://github.com/acme/app", "main", "feat/x#y"), want);
        assert_eq!(github_compare_url("ssh://git@github.com/acme/app.git/", "main", "feat/x#y"), want);
        assert_eq!(github_compare_url("git@github.com-personal:acme/app.git", "main", "feat/x#y"), want);
        assert_eq!(github_compare_url("ssh://git@github.com-work/acme/app.git", "main", "feat/x#y"), want);
        assert_eq!(github_compare_url("git@github.com-:acme/app.git", "main", "x"), None);
        assert_eq!(github_compare_url("git@github.company.com:acme/app.git", "main", "x"), None);
        assert_eq!(github_compare_url("git@gitlab.com:acme/app.git", "main", "x"), None);
        assert_eq!(github_compare_url("https://github.com/acme/app/extra", "main", "x"), None);
        assert_eq!(github_compare_url("https://github.com/ac me/app", "main", "x"), None);
        assert_eq!(github_compare_url("https://x-access-token:ghp_x@github.com/a/b.git", "main", "x"), None);
        assert_eq!(github_compare_url("https://github.com/../app", "main", "x"), None);
        assert_eq!(github_compare_url("https://github.com/acme/.", "main", "x"), None);
        assert_eq!(github_compare_url("git@github.com:./..", "main", "x"), None);
    }
}
