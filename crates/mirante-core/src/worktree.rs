//! Git worktrees per task, via the `git` CLI. Layout: `<root>/<repo-name>/<branch>`, with
//! `root` = `~/.mirante/worktrees` by default. The branch name is the user's choice (no
//! forced prefix), and the base is the repo's current branch unless one is given: never
//! an assumed `main`. Removing a worktree keeps its branch.
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::error::{CoreError, Result};
use crate::git::Git;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// `None` for a detached HEAD.
    pub branch: Option<String>,
    pub head: String,
    /// `create` made the branch (it did not exist before). Always false from `list`.
    pub created_branch: bool,
}

/// Runs a hardened git in `dir` ([`crate::git`]).
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    run(Git::new(dir).args(args), args)
}

fn run(cmd: Git, args: &[&str]) -> Result<String> {
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(CoreError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Directories that tools the user runs inside every session (MCP servers, editors) create
/// in each checkout. Untracked files under them alone never block an archive. Kept
/// short on purpose: each entry is a directory whose loss costs nothing (a cache).
pub const TOOL_DIRS: &[&str] = &[".serena/"];

/// The folder under the root that holds workspace tasks (`crate::workspace`). A repository
/// with this name cannot hold a single-repo task's worktrees (they would share the folder).
pub const WORKSPACES: &str = "_workspaces";

/// Changed paths named in a dirty-worktree refusal.
const DIRTY_LISTED: usize = 10;

#[derive(Debug, PartialEq, Eq)]
enum Dirt {
    Clean,
    /// Untracked files, every one under a [`TOOL_DIRS`] entry, and nothing else.
    ToolDirsOnly,
    /// `XY path` per change (git's porcelain codes), in git's order.
    Changes(Vec<String>),
}

/// What `git status` sees in the checkout `path`: tracked changes, staged ones and every
/// untracked file (not ignored ones: `git worktree remove` deletes those itself). A
/// checkout with submodules is never treated as tool-dirs-only (a forced removal would
/// take them along).
fn dirt(path: &Path) -> Result<Dirt> {
    let out = git(path, &["status", "--porcelain=v1", "-z", "--untracked-files=all", "--ignore-submodules=none"])?;
    let mut changes = Vec::new();
    let mut tool_only = true;
    let mut fields = out.split('\0').filter(|f| !f.is_empty());
    while let Some(entry) = fields.next() {
        let (xy, file) = entry.split_at(entry.len().min(3));
        let xy = xy.trim();
        if xy.starts_with('R') || xy.starts_with('C') {
            fields.next(); // the rename's source path
        }
        tool_only &= xy == "??" && TOOL_DIRS.iter().any(|d| file.starts_with(d));
        changes.push(format!("{xy} {file}"));
    }
    Ok(match changes.is_empty() {
        true => Dirt::Clean,
        false if tool_only && !path.join(".gitmodules").exists() => Dirt::ToolDirsOnly,
        false => Dirt::Changes(changes),
    })
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
    let ok = Git::global().args(["check-ref-format", "--branch", name]).output()?.status.success();
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

/// Why `repo` cannot hold a task's worktree, in a person's words: not a git repository, or
/// one without a commit yet (a new branch needs a base). `None` when it can.
pub fn checkout_problem(repo: &Path) -> Option<String> {
    let inside = Git::new(repo).args(["rev-parse", "--is-inside-work-tree"]).output();
    if !inside.is_ok_and(|o| o.status.success() && o.stdout.starts_with(b"true")) {
        return Some(format!(
            "{} is not a git repository: run `git init` there and commit once, or pass the path of a checkout",
            repo.display()
        ));
    }
    let head = Git::new(repo).args(["rev-parse", "--verify", "--quiet", "HEAD"]).output();
    if !head.is_ok_and(|o| o.status.success()) {
        return Some(format!(
            "{} has no commit yet: commit once (git commit --allow-empty -m init) so a task branch has a base",
            repo.display()
        ));
    }
    None
}

fn branch_exists(repo: &Path, branch: &str) -> Result<bool> {
    let out = Git::new(repo)
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
            out.push(Worktree { path, branch, head, created_branch: false });
        }
    }
    out
}

/// Canonical form of `path`, also when it (or its tail) does not exist: `.` and `..` are
/// resolved lexically, then the deepest existing ancestor is canonicalized (symlinks such
/// as macOS `/var` -> `/private/var`) and the missing components are appended.
pub(crate) fn resolve(path: &Path) -> Result<PathBuf> {
    if let Ok(p) = std::fs::canonicalize(path) {
        return Ok(p);
    }
    let abs = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
    let mut lexical = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                lexical.pop();
            }
            other => lexical.push(other),
        }
    }
    let mut missing = Vec::new();
    let mut base = lexical.as_path();
    loop {
        if let Ok(canon) = std::fs::canonicalize(base) {
            return Ok(missing.iter().rev().fold(canon, |acc: PathBuf, part| acc.join(part)));
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_os_string());
                base = parent;
            }
            _ => return Ok(lexical),
        }
    }
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

/// One mutex per repository (keyed by its canonical git common dir).
type RepoLocks = Arc<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>>;

/// Clones share the per-repo locks: `git worktree add/remove/prune` of one repository run
/// one at a time (they edit the same `.git/worktrees` metadata and config).
#[derive(Debug, Clone)]
pub struct WorktreeManager {
    root: PathBuf,
    locks: RepoLocks,
}

impl WorktreeManager {
    pub fn new(root: &Path) -> Self {
        WorktreeManager { root: root.to_path_buf(), locks: Arc::default() }
    }

    /// The lock of the repository `repo` belongs to (the same for its main checkout and
    /// every worktree of it).
    fn repo_lock(&self, repo: &Path) -> Result<Arc<Mutex<()>>> {
        let common = git(repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        let key = std::fs::canonicalize(&common).unwrap_or_else(|_| PathBuf::from(common));
        let mut locks = self.locks.lock().unwrap_or_else(|p| p.into_inner());
        Ok(locks.entry(key).or_default().clone())
    }

    /// `~/.mirante/worktrees`.
    /// Fails when `HOME` is unset or empty rather than falling back to the current directory.
    pub fn default_root() -> Result<PathBuf> {
        root_from_home(std::env::var_os("HOME"))
    }

    pub fn path_for(&self, repo: &Path, branch: &str) -> Result<PathBuf> {
        let name = repo_name(repo)?;
        if name == WORKSPACES {
            return Err(CoreError::Git {
                args: "worktree add".into(),
                stderr: format!("a repository named {WORKSPACES} shares its folder with workspace tasks: rename it"),
            });
        }
        Ok(self.root.join(name).join(branch))
    }

    /// Creates `<root>/<repo>/<branch>`. An existing branch is checked out as is; a new one
    /// starts from `base`, or from the repo's current branch when `base` is `None`.
    pub fn create(&self, repo: &Path, branch: &str, base: Option<&str>) -> Result<Worktree> {
        let path = self.path_for(repo, branch)?;
        self.create_at(repo, &path, branch, base)
    }

    /// The folder workspace tasks live in: `<root>/_workspaces`.
    pub fn workspaces(&self) -> PathBuf {
        self.root.join(WORKSPACES)
    }

    /// The root this manager creates worktrees under.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The levels of `path` below the root (each a plain name). Refused when `path` is not
    /// under the root (as recorded, or once the root's own symlinks are resolved).
    pub fn rel_levels(&self, path: &Path) -> Result<Vec<String>> {
        let outside =
            || CoreError::OutsideRoot { path: path.display().to_string(), root: self.root.display().to_string() };
        let canon_root = std::fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone());
        let rel = path.strip_prefix(&self.root).or_else(|_| path.strip_prefix(&canon_root)).map_err(|_| outside())?;
        let mut out = Vec::new();
        for c in rel.components() {
            match c {
                Component::Normal(n) => out.push(n.to_str().ok_or_else(outside)?.to_string()),
                _ => return Err(outside()),
            }
        }
        if out.is_empty() {
            return Err(outside());
        }
        Ok(out)
    }

    /// `path` (a checkout under the root) when no level of it below the root is a symlink
    /// and it is a real directory: what Mirante made, not a folder an agent swapped for a
    /// link to somewhere else (another task's worktree, the home folder). `Ok(None)` when a
    /// level does not exist (deleted by hand). Nothing is followed to decide.
    pub fn real_path(&self, path: &Path) -> Result<Option<PathBuf>> {
        let levels = self.rel_levels(path)?;
        let mut at = self.root.clone();
        for name in &levels {
            at.push(name);
            match std::fs::symlink_metadata(&at) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.into()),
                Ok(m) if m.file_type().is_symlink() => {
                    return Err(CoreError::InvalidWorkspace(format!(
                        "{} is a symlink, not the folder Mirante made: refused",
                        at.display()
                    )))
                }
                Ok(m) if !m.is_dir() => {
                    return Err(CoreError::InvalidWorkspace(format!(
                        "{} is not a folder Mirante made: refused",
                        at.display()
                    )))
                }
                Ok(_) => {}
            }
        }
        Ok(Some(at))
    }

    /// [`WorktreeManager::create`] at `path`, which must lie under the root.
    pub fn create_at(&self, repo: &Path, path: &Path, branch: &str, base: Option<&str>) -> Result<Worktree> {
        validate_branch_name(branch)?;
        if let Some(b) = base {
            validate_base(b)?;
        }
        let root = resolve(&self.root)?;
        let resolved = resolve(path)?;
        if resolved == root || !resolved.starts_with(&root) {
            return Err(CoreError::OutsideRoot { path: path.display().to_string(), root: root.display().to_string() });
        }
        let lock = self.repo_lock(repo)?;
        let _held = lock.lock().unwrap_or_else(|p| p.into_inner());
        let path = path.to_path_buf();
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
        // `worktree add` checks files out: it keeps the repository's filters (Git LFS) but
        // runs no hook (see `crate::git` for the residual risk).
        let add = |args: &[&str]| run(Git::checkout(repo).args(args), args);
        match base {
            None => add(&["worktree", "add", "--end-of-options", &path_str, branch])?,
            Some(base) => add(&["worktree", "add", "-b", branch, "--end-of-options", &path_str, &base])?,
        };
        let head = git(&path, &["rev-parse", "HEAD"])?;
        Ok(Worktree { path, branch: Some(branch.to_string()), head, created_branch: !exists })
    }

    /// Ok when [`WorktreeManager::remove`] would not refuse `path` for uncommitted work (a
    /// missing directory is fine); else the [`CoreError::DirtyWorktree`] it would give.
    pub fn check_clean(&self, path: &Path) -> Result<()> {
        let Some(path) = self.real_path(path)? else {
            return Ok(());
        };
        match dirt(&path)? {
            Dirt::Clean | Dirt::ToolDirsOnly => Ok(()),
            Dirt::Changes(all) => {
                let more = all.len().saturating_sub(DIRTY_LISTED);
                let changes = all.into_iter().take(DIRTY_LISTED).collect();
                Err(CoreError::DirtyWorktree { path: path.display().to_string(), changes, more })
            }
        }
    }

    /// Undoes `create` (e.g. when the task could not be recorded): removes the worktree,
    /// then deletes its branch only if `create` made it and it still points at the commit
    /// it started from (an atomic compare-and-delete), so no work is ever lost.
    pub fn discard(&self, repo: &Path, wt: &Worktree) -> Result<()> {
        let lock = self.repo_lock(repo)?;
        let _held = lock.lock().unwrap_or_else(|p| p.into_inner());
        self.remove_locked(repo, &wt.path)?;
        if let (true, Some(branch)) = (wt.created_branch, &wt.branch) {
            let r#ref = format!("refs/heads/{branch}");
            let now = git(repo, &["rev-parse", "--verify", "--quiet", "--end-of-options", &r#ref]);
            if now.is_ok_and(|h| h == wt.head) {
                git(repo, &["update-ref", "-d", &r#ref, &wt.head])?;
            }
        }
        Ok(())
    }

    /// Removes the worktree directory and its git metadata. The branch is kept. Refuses a
    /// dirty worktree ([`CoreError::DirtyWorktree`], listing what changed) so uncommitted
    /// work is never discarded silently. The one exception: untracked files that all lie in
    /// [`TOOL_DIRS`] (caches the user's own tools write into every checkout, e.g. Serena's
    /// `.serena/`) do not count, and the removal is then forced (git would refuse it).
    /// If the directory is already gone (deleted by hand), the path is checked against the
    /// root lexically and `git worktree prune` drops the stale metadata. A level of `path`
    /// below the root that is a symlink is refused ([`WorktreeManager::real_path`]): an
    /// agent could have swapped its folder for a link to another task's worktree.
    pub fn remove(&self, repo: &Path, path: &Path) -> Result<()> {
        let lock = self.repo_lock(repo)?;
        let _held = lock.lock().unwrap_or_else(|p| p.into_inner());
        self.remove_locked(repo, path)
    }

    /// [`WorktreeManager::remove`] with the repo lock already held.
    fn remove_locked(&self, repo: &Path, given: &Path) -> Result<()> {
        let root = resolve(&self.root)?;
        let path = resolve(given)?;
        if path == root || !path.starts_with(&root) {
            return Err(CoreError::OutsideRoot { path: path.display().to_string(), root: root.display().to_string() });
        }
        if self.real_path(given)?.is_some() {
            let force = match dirt(&path)? {
                Dirt::Clean => false,
                Dirt::ToolDirsOnly => true,
                Dirt::Changes(all) => {
                    let more = all.len().saturating_sub(DIRTY_LISTED);
                    let changes = all.into_iter().take(DIRTY_LISTED).collect();
                    return Err(CoreError::DirtyWorktree { path: path.display().to_string(), changes, more });
                }
            };
            let target = path.to_string_lossy();
            let mut args = vec!["worktree", "remove"];
            if force {
                args.push("--force");
            }
            args.extend(["--end-of-options", &target]);
            // The worktree may carry its own config (`config.worktree`): its filters too.
            run(Git::new(repo).neutralize_filters_of(&path).args(&args), &args)?;
        } else {
            git(repo, &["worktree", "prune"])?;
        }
        Ok(())
    }

    /// Every checkout directory under the root (canonical paths): a directory holding a
    /// `.git` entry, not descended into. Whatever repository it belongs to.
    pub fn checkouts(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![std::fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone())];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.filter_map(|e| e.ok()) {
                // Not following symlinks: only real directories under the root count.
                if !e.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                let p = e.path();
                if p.join(".git").exists() {
                    out.push(p);
                } else {
                    stack.push(p);
                }
            }
        }
        out
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
pub(crate) mod tests {
    use super::*;
    use crate::test_support::TempDir;
    use std::process::Command;

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
    fn a_plain_folder_or_an_empty_repo_is_named_for_what_it_lacks() {
        let d = TempDir::new("checkout-problem");
        let plain = d.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let e = checkout_problem(&plain).unwrap();
        assert!(e.contains("is not a git repository") && e.contains("git init"), "{e}");
        let empty = d.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(Command::new("git").arg("-C").arg(&empty).args(["init", "-q"]).status().unwrap().success());
        let e = checkout_problem(&empty).unwrap();
        assert!(e.contains("has no commit yet"), "{e}");
        assert!(Command::new("git")
            .arg("-C")
            .arg(&empty)
            .args(["-c", "user.name=T", "-c", "user.email=t@example.com", "commit", "-q", "--allow-empty", "-m", "i"])
            .status()
            .unwrap()
            .success());
        assert_eq!(checkout_problem(&empty), None);
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
    fn discard_deletes_only_a_branch_it_created_and_nobody_moved() {
        let dir = TempDir::new("wt-discard");
        let repo = repo(&dir);
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        let fresh = mgr.create(&repo, "fresh", None).unwrap();
        assert!(fresh.created_branch);
        mgr.discard(&repo, &fresh).unwrap();
        assert!(!fresh.path.exists());
        assert!(!branch_exists(&repo, "fresh").unwrap());

        git(&repo, &["branch", "kept"]).unwrap();
        let existing = mgr.create(&repo, "kept", None).unwrap();
        assert!(!existing.created_branch);
        mgr.discard(&repo, &existing).unwrap();
        assert!(branch_exists(&repo, "kept").unwrap());

        let moved = mgr.create(&repo, "moved", None).unwrap();
        git(
            &moved.path,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "work",
            ],
        )
        .unwrap();
        mgr.discard(&repo, &moved).unwrap();
        assert!(branch_exists(&repo, "moved").unwrap());
    }

    #[test]
    fn remove_refuses_a_dirty_worktree() {
        let dir = TempDir::new("wt-dirty");
        let repo = repo(&dir);
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        let a = mgr.create(&repo, "a", None).unwrap();
        std::fs::write(a.path.join("new.txt"), "work\n").unwrap();
        std::fs::write(a.path.join("README.md"), "changed\n").unwrap();
        let err = mgr.remove(&repo, &a.path).unwrap_err();
        let CoreError::DirtyWorktree { changes, more, .. } = &err else { panic!("{err:?}") };
        assert_eq!((changes.as_slice(), *more), (&["M README.md".to_string(), "?? new.txt".to_string()][..], 0));
        let msg = err.to_string();
        assert!(msg.contains("uncommitted changes") && msg.contains("new.txt") && msg.contains("commit"), "{msg}");
        assert!(msg.contains("stash") && msg.contains("discard") && !msg.contains("--force"), "{msg}");
        assert!(a.path.exists());
    }

    #[test]
    fn a_dirty_refusal_lists_at_most_ten_paths() {
        let dir = TempDir::new("wt-dirty-many");
        let repo = repo(&dir);
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        let a = mgr.create(&repo, "a", None).unwrap();
        for i in 0..13 {
            std::fs::write(a.path.join(format!("f{i:02}.txt")), "x\n").unwrap();
        }
        let err = mgr.remove(&repo, &a.path).unwrap_err();
        let CoreError::DirtyWorktree { changes, more, .. } = &err else { panic!("{err:?}") };
        assert_eq!((changes.len(), *more), (10, 3));
        assert!(err.to_string().contains("and 3 more"), "{err}");
    }

    #[test]
    fn tool_dirs_alone_do_not_block_a_remove_but_never_hide_real_work() {
        let dir = TempDir::new("wt-tooldirs");
        let repo = repo(&dir);
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        // Only Serena's cache: removed.
        let a = mgr.create(&repo, "a", None).unwrap();
        std::fs::create_dir_all(a.path.join(".serena/cache")).unwrap();
        std::fs::write(a.path.join(".serena/project.yml"), "x\n").unwrap();
        std::fs::write(a.path.join(".serena/cache/doc.pkl"), "x\n").unwrap();
        mgr.remove(&repo, &a.path).unwrap();
        assert!(!a.path.exists());
        // Serena's cache next to an untracked file: refused, and the file names it.
        let b = mgr.create(&repo, "b", None).unwrap();
        std::fs::create_dir_all(b.path.join(".serena")).unwrap();
        std::fs::write(b.path.join(".serena/project.yml"), "x\n").unwrap();
        std::fs::write(b.path.join("notes.txt"), "mine\n").unwrap();
        let err = mgr.remove(&repo, &b.path).unwrap_err();
        assert!(
            matches!(&err, CoreError::DirtyWorktree { changes, .. } if changes.contains(&"?? notes.txt".to_string()))
        );
        assert!(b.path.join("notes.txt").exists());
        // Next to a tracked change: refused.
        let c = mgr.create(&repo, "c", None).unwrap();
        std::fs::create_dir_all(c.path.join(".serena")).unwrap();
        std::fs::write(c.path.join(".serena/project.yml"), "x\n").unwrap();
        std::fs::write(c.path.join("README.md"), "edited\n").unwrap();
        assert!(matches!(mgr.remove(&repo, &c.path), Err(CoreError::DirtyWorktree { .. })));
        assert!(c.path.exists());
        // A tracked file inside a tool dir that was changed is real work: refused.
        let d = mgr.create(&repo, "d", None).unwrap();
        std::fs::create_dir_all(d.path.join(".serena")).unwrap();
        std::fs::write(d.path.join(".serena/project.yml"), "tracked\n").unwrap();
        git(&d.path, &["add", ".serena/project.yml"]).unwrap();
        assert!(matches!(mgr.remove(&repo, &d.path), Err(CoreError::DirtyWorktree { .. })));
        assert!(d.path.exists());
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
    fn remove_of_a_deleted_directory_prunes_its_metadata() {
        let dir = TempDir::new("wt-gone");
        let repo = repo(&dir);
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        let a = mgr.create(&repo, "a", None).unwrap();
        mgr.create(&repo, "b", None).unwrap();
        std::fs::remove_dir_all(&a.path).unwrap();
        mgr.remove(&repo, &a.path).unwrap();
        let listed: Vec<Option<String>> = mgr.list(&repo).unwrap().into_iter().map(|w| w.branch).collect();
        assert_eq!(listed, vec![Some("b".to_string())]);
        assert!(!git(&repo, &["worktree", "list", "--porcelain"]).unwrap().contains("/wt/my-app/a\n"));
        assert!(branch_exists(&repo, "a").unwrap());
        // A missing path is still checked against the root, lexically (`..` cannot escape).
        let escape = dir.path().join("wt/my-app/../../elsewhere/x");
        assert!(matches!(mgr.remove(&repo, &escape), Err(CoreError::OutsideRoot { .. })));
        assert!(matches!(mgr.remove(&repo, &dir.path().join("nowhere/x")), Err(CoreError::OutsideRoot { .. })));
    }

    #[test]
    fn concurrent_creates_on_one_repo_with_different_branches_all_succeed() {
        let dir = TempDir::new("wt-concurrent");
        let repo = repo(&dir);
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let (mgr, repo) = (mgr.clone(), repo.clone());
                std::thread::spawn(move || mgr.create(&repo, &format!("par-{i}"), None).map(|w| w.path))
            })
            .collect();
        for h in handles {
            let path = h.join().unwrap().unwrap();
            assert!(path.join("README.md").is_file());
        }
        assert_eq!(mgr.list(&repo).unwrap().len(), 8);
    }

    #[test]
    fn one_lock_per_repository_shared_by_its_worktrees_and_clones() {
        let dir = TempDir::new("wt-lock");
        let repo = repo(&dir);
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        let wt = mgr.create(&repo, "a", None).unwrap();
        let other = WorktreeManager::new(&dir.path().join("wt")).clone();
        let a = mgr.repo_lock(&repo).unwrap();
        assert!(std::sync::Arc::ptr_eq(&a, &mgr.repo_lock(&wt.path).unwrap()), "a worktree locks its repository");
        assert!(std::sync::Arc::ptr_eq(&a, &mgr.clone().repo_lock(&repo.join(".")).unwrap()), "clones share locks");
        assert!(!std::sync::Arc::ptr_eq(&a, &other.repo_lock(&repo).unwrap()), "separate managers do not");
    }

    #[test]
    fn checkouts_lists_every_worktree_directory_under_the_root() {
        let dir = TempDir::new("wt-checkouts");
        let repo = repo(&dir);
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        assert!(mgr.checkouts().is_empty(), "no root yet");
        let a = mgr.create(&repo, "a", None).unwrap();
        let b = mgr.create(&repo, "pedro/b", None).unwrap();
        std::fs::create_dir_all(dir.path().join("wt/my-app/not-a-checkout/x")).unwrap();
        let mut got = mgr.checkouts();
        got.sort();
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap();
        assert_eq!(got, vec![canon(&a.path), canon(&b.path)]);
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
        assert_eq!(w[1], Worktree { path: "/w/x".into(), branch: None, head: "bbb".into(), created_branch: false });
    }

    /// Plants code-execution traps a prompt-injected agent could set in a repository: hooks,
    /// `core.fsmonitor`, a clean/smudge filter, a textconv and external diff driver. Each
    /// trap touches `<marks>/PWNED-<name>` when git runs it.
    pub(crate) fn plant_git_traps(repo: &Path, marks: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let script = |name: &str| {
            let path = marks.join(format!("trap-{name}.sh"));
            std::fs::write(&path, format!("#!/bin/sh\ntouch '{}/PWNED-{name}'\ncat\n", marks.display())).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path.display().to_string()
        };
        let hooks = marks.join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        for hook in ["post-checkout", "reference-transaction", "post-index-change", "pre-auto-gc"] {
            std::fs::copy(script(hook), hooks.join(hook)).unwrap();
        }
        let hooks = hooks.display().to_string();
        for (key, value) in [
            ("core.hooksPath", hooks),
            ("core.fsmonitor", script("fsmonitor")),
            ("core.pager", script("pager")),
            ("diff.external", script("external")),
            ("filter.evil.clean", script("clean")),
            ("filter.evil.smudge", script("smudge")),
            ("diff.evil.textconv", script("textconv")),
            ("diff.evil.command", script("diffcmd")),
        ] {
            let ok = Command::new("git").arg("-C").arg(repo).args(["config", key, &value]).status().unwrap();
            assert!(ok.success(), "git config {key}");
        }
    }

    /// The traps [`plant_git_traps`] set off.
    pub(crate) fn sprung(marks: &Path) -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(marks)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.starts_with("PWNED"))
            .collect();
        out.sort();
        out
    }

    /// Runs git in the test's own name (never through the hardened runner) with every trap
    /// disarmed, so only the code under test can set one off.
    pub(crate) fn git_disarmed(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", "-c", "filter.evil.clean=cat"])
            .args(["-c", "filter.evil.smudge=cat", "-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(ok.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ok.stderr));
    }

    #[test]
    fn worktree_lifecycle_never_runs_code_the_repo_configures() {
        let dir = TempDir::new("wt-traps");
        let repo = repo(&dir);
        let marks = dir.path().join("marks");
        std::fs::create_dir_all(&marks).unwrap();
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        plant_git_traps(&repo, &marks);
        let wt = mgr.create(&repo, "agent", None).unwrap();
        assert_eq!(sprung(&marks), Vec::<String>::new(), "worktree add");
        // What the agent leaves behind: attributes routing every file through its filter
        // and diff driver, committed, and a file rewritten with the same size.
        std::fs::write(wt.path.join(".gitattributes"), "* filter=evil diff=evil\n").unwrap();
        git_disarmed(&wt.path, &["add", ".gitattributes"]);
        git_disarmed(&wt.path, &["commit", "-q", "-m", "attrs"]);
        std::fs::write(wt.path.join("README.md"), "hi\n").unwrap();
        std::fs::write(wt.path.join("scratch"), "x\n").unwrap();
        let dirty = mgr.remove(&repo, &wt.path);
        assert!(matches!(dirty, Err(CoreError::DirtyWorktree { .. })), "{dirty:?}");
        std::fs::remove_file(wt.path.join("scratch")).unwrap();
        mgr.remove(&repo, &wt.path).unwrap();
        let gone = mgr.create(&repo, "gone", None).unwrap();
        std::fs::remove_dir_all(&gone.path).unwrap();
        mgr.remove(&repo, &gone.path).unwrap();
        mgr.list(&repo).unwrap();
        let fresh = mgr.create(&repo, "fresh", None).unwrap();
        mgr.discard(&repo, &fresh).unwrap();
        assert_eq!(sprung(&marks), Vec::<String>::new());
    }
}
