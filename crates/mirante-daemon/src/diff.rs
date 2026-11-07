//! `task.diff`: what a task branch changed against its base, read with git. Read-only: it
//! never writes to the repo or the worktree. Uncommitted changes count (the agent may not
//! have committed yet); untracked files are listed by name.
//!
//! The worktree is agent-written: git runs through the hardened runner
//! ([`mirante_core::git`]) and diffs never run an external diff driver or textconv.
use std::path::Path;

use mirante_core::git::Git;
use mirante_ipc::types::{DiffFile, TaskDiff};

/// Largest patch returned; the rest is cut (`truncated: true`).
pub const PATCH_CAP: usize = 512 * 1024;

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Git::new(dir).args(args).output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `git diff --numstat` lines; a binary file (`-\t-\tpath`) has no counts.
pub fn parse_numstat(text: &str) -> Vec<DiffFile> {
    text.lines()
        .filter_map(|l| {
            let mut parts = l.splitn(3, '\t');
            let (a, r, path) = (parts.next()?, parts.next()?, parts.next()?);
            Some(DiffFile { path: path.to_string(), added: a.parse().ok(), removed: r.parse().ok() })
        })
        .collect()
}

/// GitHub's "open a pull request" page for `branch` against `base`, when `remote` is a
/// GitHub URL (`git@github.com:o/r.git`, `https://github.com/o/r(.git)`,
/// `ssh://git@github.com/o/r.git`). Anything else is `None`.
pub fn github_compare_url(remote: &str, base: &str, branch: &str) -> Option<String> {
    let path = remote
        .strip_prefix("git@github.com:")
        .or_else(|| remote.strip_prefix("https://github.com/"))
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    let safe = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
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
    let diff = |extra: &[&str]| {
        let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color", "--no-renames"];
        args.extend(extra);
        args.extend(["--end-of-options", &merge_base, "--"]);
        git(worktree, &args)
    };
    let files = parse_numstat(&diff(&["--numstat"])?);
    let untracked =
        git(worktree, &["ls-files", "--others", "--exclude-standard"])?.lines().map(str::to_string).collect();
    // `--default-prefix`: git's `a/` `b/` whatever `diff.noprefix` / `diff.mnemonicPrefix` say.
    let mut patch = diff(&["--default-prefix"])?;
    let truncated = patch.len() > PATCH_CAP;
    if truncated {
        let mut cut = PATCH_CAP;
        while !patch.is_char_boundary(cut) {
            cut -= 1;
        }
        patch.truncate(cut);
    }
    let compare_url =
        git(repo, &["remote", "get-url", "origin"]).ok().and_then(|u| github_compare_url(u.trim(), &base, branch));
    let remote_ref = format!("refs/remotes/origin/{branch}");
    let pushed = git(worktree, &["rev-parse", "--verify", "--quiet", "--end-of-options", &remote_ref]).is_ok();
    Ok(TaskDiff { base, merge_base, files, untracked, patch, truncated, compare_url, pushed })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_lines_parse_with_binary_files_uncounted() {
        let files = parse_numstat("3\t1\tsrc/a.rs\n-\t-\tlogo.png\n");
        assert_eq!(
            files,
            vec![
                DiffFile { path: "src/a.rs".into(), added: Some(3), removed: Some(1) },
                DiffFile { path: "logo.png".into(), added: None, removed: None },
            ]
        );
    }

    #[test]
    fn compare_urls_only_for_github_remotes() {
        let want = Some("https://github.com/acme/app/compare/main...feat/x%23y?expand=1".to_string());
        assert_eq!(github_compare_url("git@github.com:acme/app.git", "main", "feat/x#y"), want);
        assert_eq!(github_compare_url("https://github.com/acme/app", "main", "feat/x#y"), want);
        assert_eq!(github_compare_url("ssh://git@github.com/acme/app.git/", "main", "feat/x#y"), want);
        assert_eq!(github_compare_url("git@gitlab.com:acme/app.git", "main", "x"), None);
        assert_eq!(github_compare_url("https://github.com/acme/app/extra", "main", "x"), None);
        assert_eq!(github_compare_url("https://github.com/ac me/app", "main", "x"), None);
    }
}
