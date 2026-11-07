//! Git worktrees per task, via the `git` CLI. Layout: `<root>/<repo-name>/<branch>`, with
//! `root` = `~/.mirante/worktrees` by default. The branch name is the user's choice (no
//! forced prefix), and the base is the repo's current branch unless one is given: never
//! an assumed `main`. Removing a worktree keeps its branch.
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{CoreError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// `None` for a detached HEAD.
    pub branch: Option<String>,
    pub head: String,
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    if !out.status.success() {
        return Err(CoreError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

fn invalid(name: &str, reason: &str) -> CoreError {
    CoreError::InvalidBranch { name: name.to_string(), reason: reason.to_string() }
}

/// Accepts any name git accepts as a branch (`git check-ref-format --branch`), except
/// names that are not literal branch names: leading `-`, `HEAD`, `@`, and `@{...}` shorthands.
pub fn validate_branch_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(invalid(name, "empty"));
    }
    if name.starts_with('-') {
        return Err(invalid(name, "starts with '-'"));
    }
    if name == "HEAD" || name == "@" || name.contains("@{") {
        return Err(invalid(name, "not a literal branch name"));
    }
    let ok = Command::new("git").args(["check-ref-format", "--branch", name]).output()?.status.success();
    if !ok {
        return Err(invalid(name, "rejected by git check-ref-format"));
    }
    Ok(())
}

/// A base ref given by the user: non-empty and never option-like (leading `-`).
fn validate_base(base: &str) -> Result<()> {
    if base.trim().is_empty() {
        return Err(invalid(base, "empty base"));
    }
    if base.starts_with('-') {
        return Err(invalid(base, "base starts with '-'"));
    }
    Ok(())
}

/// The branch checked out in `repo` (fails on a detached HEAD).
pub fn current_branch(repo: &Path) -> Result<String> {
    git(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]).map_err(|_| CoreError::Git {
        args: "symbolic-ref HEAD".into(),
        stderr: "repository is on a detached HEAD; choose a base branch".into(),
    })
}

/// Directory name of the repository's main worktree (works from any of its worktrees).
pub fn repo_name(repo: &Path) -> Result<String> {
    let common = git(repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let common = PathBuf::from(common);
    let top = if common.file_name().is_some_and(|n| n == ".git") {
        common.parent().unwrap_or(&common).to_path_buf()
    } else {
        common
    };
    top.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| CoreError::Git {
        args: "rev-parse --git-common-dir".into(),
        stderr: "no repository name".into(),
    })
}

fn branch_exists(repo: &Path, branch: &str) -> Result<bool> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show-ref", "--verify", "--quiet", "--end-of-options", &format!("refs/heads/{branch}")])
        .output()?;
    Ok(out.status.success())
}

pub fn parse_porcelain(text: &str) -> Vec<Worktree> {
    let mut out = Vec::new();
    for block in text.split("\n\n").filter(|b| !b.trim().is_empty()) {
        let mut path = None;
        let mut head = String::new();
        let mut branch = None;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(h) = line.strip_prefix("HEAD ") {
                head = h.to_string();
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
            }
        }
        if let Some(path) = path {
            out.push(Worktree { path, branch, head });
        }
    }
    out
}

fn root_from_home(home: Option<std::ffi::OsString>) -> Result<PathBuf> {
    match home.filter(|h| !h.is_empty()) {
        Some(home) => Ok(PathBuf::from(home).join(".mirante/worktrees")),
        None => Err(CoreError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "HOME is not set; cannot locate ~/.mirante/worktrees",
        ))),
    }
}

#[derive(Debug, Clone)]
pub struct WorktreeManager {
    root: PathBuf,
}

impl WorktreeManager {
    pub fn new(root: &Path) -> Self {
        WorktreeManager { root: root.to_path_buf() }
    }

    /// `~/.mirante/worktrees`.
    /// Fails when `HOME` is unset or empty rather than falling back to the current directory.
    pub fn default_root() -> Result<PathBuf> {
        root_from_home(std::env::var_os("HOME"))
    }

    pub fn path_for(&self, repo: &Path, branch: &str) -> Result<PathBuf> {
        Ok(self.root.join(repo_name(repo)?).join(branch))
    }

    /// Creates `<root>/<repo>/<branch>`. An existing branch is checked out as is; a new one
    /// starts from `base`, or from the repo's current branch when `base` is `None`.
    pub fn create(&self, repo: &Path, branch: &str, base: Option<&str>) -> Result<Worktree> {
        validate_branch_name(branch)?;
        if let Some(b) = base {
            validate_base(b)?;
        }
        let path = self.path_for(repo, branch)?;
        if path.exists() {
            return Err(CoreError::Git {
                args: "worktree add".into(),
                stderr: format!("{} already exists", path.display()),
            });
        }
        let exists = branch_exists(repo, branch)?;
        if exists && base.is_some() {
            return Err(invalid(branch, "branch already exists; a base only applies to a new branch"));
        }
        // Resolve the base before touching the filesystem so a detached HEAD leaves nothing behind.
        let base = match (exists, base) {
            (true, _) => None,
            (false, Some(b)) => Some(b.to_string()),
            (false, None) => Some(current_branch(repo)?),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let path_str = path.to_string_lossy().into_owned();
        match base {
            None => git(repo, &["worktree", "add", "--end-of-options", &path_str, branch])?,
            Some(base) => git(repo, &["worktree", "add", "-b", branch, "--end-of-options", &path_str, &base])?,
        };
        let head = git(&path, &["rev-parse", "HEAD"])?;
        Ok(Worktree { path, branch: Some(branch.to_string()), head })
    }

    /// Removes the worktree directory and its git metadata. The branch is kept. Refuses a
    /// dirty worktree (git's own check) so uncommitted work is never discarded silently.
    pub fn remove(&self, repo: &Path, path: &Path) -> Result<()> {
        let root = std::fs::canonicalize(&self.root)?;
        let path = std::fs::canonicalize(path)?;
        if path == root || !path.starts_with(&root) {
            return Err(CoreError::OutsideRoot { path: path.display().to_string(), root: root.display().to_string() });
        }
        git(repo, &["worktree", "remove", "--end-of-options", &path.to_string_lossy()])?;
        Ok(())
    }

    /// Worktrees of `repo` that live under this manager's root.
    pub fn list(&self, repo: &Path) -> Result<Vec<Worktree>> {
        let root = std::fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone());
        let all = parse_porcelain(&git(repo, &["worktree", "list", "--porcelain"])?);
        Ok(all
            .into_iter()
            .filter(|w| {
                let p = std::fs::canonicalize(&w.path).unwrap_or_else(|_| w.path.clone());
                p.starts_with(&root)
            })
            .collect())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    /// A repo whose only branch is `trunk` (proves nothing assumes `main`).
    fn repo(dir: &TempDir) -> PathBuf {
        let repo = dir.path().join("my-app");
        std::fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| {
            let ok = Command::new("git").arg("-C").arg(&repo).args(args).output().unwrap().status.success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q", "-b", "trunk"]);
        std::fs::write(repo.join("README.md"), "hi\n").unwrap();
        run(&["add", "."]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "init",
        ]);
        repo
    }

    #[test]
    fn validates_branch_names() {
        for ok in ["feat-login", "pedro/fix-123", "v2.1"] {
            validate_branch_name(ok).unwrap();
        }
        for bad in ["", " ", "-x", "HEAD", "@{-1}", "a..b", "has space", "ends.lock", "x~1", "a:b"] {
            assert!(matches!(validate_branch_name(bad), Err(CoreError::InvalidBranch { .. })), "{bad:?}");
        }
    }

    #[test]
    fn creates_from_current_branch_without_assuming_main() {
        let dir = TempDir::new("wt-create");
        let repo = repo(&dir);
        assert_eq!(current_branch(&repo).unwrap(), "trunk");
        let mgr = WorktreeManager::new(&dir.path().join("worktrees"));
        let wt = mgr.create(&repo, "pedro/feature-x", None).unwrap();
        assert_eq!(wt.path, dir.path().join("worktrees/my-app/pedro/feature-x"));
        assert!(wt.path.join("README.md").is_file());
        assert_eq!(current_branch(&wt.path).unwrap(), "pedro/feature-x");
        assert_eq!(wt.head, git(&repo, &["rev-parse", "trunk"]).unwrap());
        assert_eq!(repo_name(&wt.path).unwrap(), "my-app");
    }

    #[test]
    fn explicit_base_and_existing_branch() {
        let dir = TempDir::new("wt-base");
        let repo = repo(&dir);
        git(&repo, &["branch", "release"]).unwrap();
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        mgr.create(&repo, "from-release", Some("release")).unwrap();
        let existing = mgr.create(&repo, "release", None).unwrap();
        assert_eq!(existing.branch.as_deref(), Some("release"));
        assert!(mgr.create(&repo, "nope", Some("no-such-base")).is_err());
    }

    #[test]
    fn rejects_invalid_names_and_taken_paths() {
        let dir = TempDir::new("wt-reject");
        let repo = repo(&dir);
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        assert!(matches!(mgr.create(&repo, "bad name", None), Err(CoreError::InvalidBranch { .. })));
        mgr.create(&repo, "one", None).unwrap();
        assert!(mgr.create(&repo, "one", None).is_err());
    }

    #[test]
    fn remove_keeps_the_branch_and_list_shows_only_managed_worktrees() {
        let dir = TempDir::new("wt-remove");
        let repo = repo(&dir);
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        let a = mgr.create(&repo, "a", None).unwrap();
        mgr.create(&repo, "b", None).unwrap();
        let listed: Vec<Option<String>> = mgr.list(&repo).unwrap().into_iter().map(|w| w.branch).collect();
        assert_eq!(listed, vec![Some("a".to_string()), Some("b".to_string())]);
        mgr.remove(&repo, &a.path).unwrap();
        assert!(!a.path.exists());
        assert!(branch_exists(&repo, "a").unwrap());
        assert_eq!(mgr.list(&repo).unwrap().len(), 1);
    }

    #[test]
    fn remove_refuses_a_dirty_worktree() {
        let dir = TempDir::new("wt-dirty");
        let repo = repo(&dir);
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        let a = mgr.create(&repo, "a", None).unwrap();
        std::fs::write(a.path.join("new.txt"), "work\n").unwrap();
        assert!(matches!(mgr.remove(&repo, &a.path), Err(CoreError::Git { .. })));
        assert!(a.path.exists());
    }

    #[test]
    fn rejects_option_like_base_and_base_for_existing_branch() {
        let dir = TempDir::new("wt-base-guard");
        let repo = repo(&dir);
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        for bad in ["--orphan", "-x", ""] {
            assert!(matches!(mgr.create(&repo, "n", Some(bad)), Err(CoreError::InvalidBranch { .. })), "{bad:?}");
        }
        assert!(matches!(validate_branch_name("@"), Err(CoreError::InvalidBranch { .. })));
        git(&repo, &["branch", "release"]).unwrap();
        assert!(matches!(mgr.create(&repo, "release", Some("trunk")), Err(CoreError::InvalidBranch { .. })));
        assert!(!dir.path().join("wt").exists());
    }

    #[test]
    fn detached_head_without_base_is_a_clear_error() {
        let dir = TempDir::new("wt-detached");
        let repo = repo(&dir);
        git(&repo, &["checkout", "-q", "--detach"]).unwrap();
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        let err = mgr.create(&repo, "n", None).unwrap_err();
        assert!(err.to_string().contains("detached HEAD; choose a base branch"), "{err}");
        mgr.create(&repo, "n", Some("trunk")).unwrap();
    }

    #[test]
    fn remove_refuses_paths_outside_the_root() {
        let dir = TempDir::new("wt-outside");
        let repo = repo(&dir);
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        let elsewhere = WorktreeManager::new(&dir.path().join("other"));
        let a = elsewhere.create(&repo, "a", None).unwrap();
        std::fs::create_dir_all(dir.path().join("wt")).unwrap();
        let err = mgr.remove(&repo, &a.path).unwrap_err();
        assert!(matches!(err, CoreError::OutsideRoot { .. }), "{err}");
        assert!(a.path.exists());
        assert!(matches!(mgr.remove(&repo, &dir.path().join("wt")), Err(CoreError::OutsideRoot { .. })));
    }

    #[test]
    fn missing_home_is_an_error_not_a_dot_fallback() {
        assert!(root_from_home(None).is_err());
        assert!(root_from_home(Some("".into())).is_err());
        assert_eq!(root_from_home(Some("/h".into())).unwrap(), PathBuf::from("/h/.mirante/worktrees"));
    }

    #[test]
    fn parses_porcelain_including_detached() {
        let text = "worktree /r\nHEAD aaa\nbranch refs/heads/trunk\n\nworktree /w/x\nHEAD bbb\ndetached\n";
        let w = parse_porcelain(text);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].branch.as_deref(), Some("trunk"));
        assert_eq!(w[1], Worktree { path: "/w/x".into(), branch: None, head: "bbb".into() });
    }
}
