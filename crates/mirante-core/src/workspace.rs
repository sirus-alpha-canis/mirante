//! Workspace tasks: one task across several repositories of a folder (a "group", the folder
//! a person added to Projects). Each selected repository gets its own worktree, all on one
//! branch name, under `<root>/_workspaces/<slug>/<repo-name>/`; the agent runs in the
//! workspace folder `<root>/_workspaces/<slug>/`, which is not a git repository. The
//! group's other repositories are named to the agent as read-only.
//!
//! Every path from a client is checked here before anything is made: the group is a real
//! folder outside Mirante's worktrees, and each selected repository is the top level of a
//! git checkout inside it once symlinks are resolved (so a symlink cannot reach outside).
//! Every git call goes through the hardened runner ([`crate::git`]).
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::{CoreError, Result};
use crate::git::Git;
use crate::model::TaskRepo;
use crate::worktree::{self, Worktree, WorktreeManager};

/// Most repositories one workspace task may change.
pub const MAX_REPOS: usize = 50;
/// What the workspace holds, for the agent and the person.
pub const FILE: &str = "MIRANTE_WORKSPACE.md";
/// The files Mirante writes in a workspace folder: [`FILE`], `AGENTS.md` (Codex reads it from
/// its cwd) and `CLAUDE.md` (Claude reads it from its cwd; it imports [`FILE`]).
pub const OWN_FILES: [&str; 3] = [FILE, "AGENTS.md", "CLAUDE.md"];

/// How deep the group is looked into for read-only repositories (the cockpit's scan too).
const SCAN_DEPTH: usize = 2;
const SCAN_DIRS: usize = 500;
const SCAN_REPOS: usize = 200;
/// Folders never looked into (besides hidden ones): dependencies and build output.
const SKIPPED: &[&str] = &["node_modules", "target", "dist", "build"];

fn bad(msg: impl Into<String>) -> CoreError {
    CoreError::InvalidWorkspace(msg.into())
}

/// A repository the person selected, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selected {
    /// Its folder name in the workspace (the checkout's own name; `-2`… on a clash).
    pub name: String,
    /// Its checkout, canonical.
    pub repo: PathBuf,
}

/// The group folder, canonical: an absolute path to a directory outside `worktrees_root`.
pub fn validate_group(group: &str, worktrees_root: &Path) -> Result<PathBuf> {
    if !Path::new(group).is_absolute() {
        return Err(bad(format!("the folder {group} is not an absolute path")));
    }
    let canon = std::fs::canonicalize(group).map_err(|e| bad(format!("the folder {group}: {e}")))?;
    if !canon.is_dir() {
        return Err(bad(format!("{} is not a folder", canon.display())));
    }
    let root = std::fs::canonicalize(worktrees_root).unwrap_or_else(|_| worktrees_root.to_path_buf());
    if canon.starts_with(&root) {
        return Err(bad(format!("{} is inside Mirante's worktrees", canon.display())));
    }
    Ok(canon)
}

/// `git rev-parse --show-toplevel` in `dir`, canonical.
fn toplevel(dir: &Path) -> Option<PathBuf> {
    let out = Git::new(dir).args(["rev-parse", "--show-toplevel"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let top = String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string();
    std::fs::canonicalize(top).ok()
}

/// The selected repositories, checked against the group (see the module docs), named for
/// the workspace (unique; never one of [`OWN_FILES`]).
pub fn validate_selection(group: &Path, repos: &[String], worktrees_root: &Path) -> Result<Vec<Selected>> {
    if repos.is_empty() {
        return Err(bad("select at least one repository of the folder for the task to change"));
    }
    if repos.len() > MAX_REPOS {
        return Err(bad(format!("{} repositories selected: at most {MAX_REPOS}", repos.len())));
    }
    let root = std::fs::canonicalize(worktrees_root).unwrap_or_else(|_| worktrees_root.to_path_buf());
    let mut seen = HashSet::new();
    let mut names: HashSet<String> = OWN_FILES.iter().map(|f| f.to_lowercase()).collect();
    let mut out = Vec::with_capacity(repos.len());
    for r in repos {
        if !Path::new(r).is_absolute() {
            return Err(bad(format!("{r} is not an absolute path")));
        }
        let canon = std::fs::canonicalize(r).map_err(|e| bad(format!("{r}: {e}")))?;
        if canon == group || !canon.starts_with(group) {
            return Err(bad(format!("{r} is not inside the folder {}", group.display())));
        }
        if canon.starts_with(&root) {
            return Err(bad(format!("{r} is one of Mirante's own worktrees")));
        }
        if let Some(why) = worktree::checkout_problem(&canon) {
            return Err(bad(why));
        }
        if toplevel(&canon).as_deref() != Some(canon.as_path()) {
            return Err(bad(format!("{r} is not the top folder of a git checkout")));
        }
        if !seen.insert(canon.clone()) {
            return Err(bad(format!("{r} is selected twice")));
        }
        let base = canon.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repo".into());
        let mut name = base.clone();
        let mut n = 2;
        while names.contains(&name.to_lowercase()) {
            name = format!("{base}-{n}");
            n += 1;
        }
        names.insert(name.to_lowercase());
        out.push(Selected { name, repo: canon });
    }
    Ok(out)
}

/// The git checkouts in `group` (two levels deep at most; hidden and build folders skipped,
/// symlinks never followed, bounded), canonical and sorted.
pub fn group_checkouts(group: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(group.to_path_buf(), 0usize)];
    let mut visited = 0;
    while let Some((dir, depth)) = stack.pop() {
        visited += 1;
        if visited > SCAN_DIRS || out.len() >= SCAN_REPOS {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.filter_map(|e| e.ok()) {
            if !e.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || SKIPPED.contains(&name.as_str()) {
                continue;
            }
            let p = e.path();
            if std::fs::symlink_metadata(p.join(".git")).is_ok() {
                if let Ok(c) = std::fs::canonicalize(&p) {
                    out.push(c);
                }
            } else if depth + 1 < SCAN_DEPTH {
                stack.push((p, depth + 1));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The workspace folder's name for `branch`: without a leading `mirante/`, `/` as `-`.
pub fn slug(branch: &str) -> String {
    branch.strip_prefix("mirante/").unwrap_or(branch).replace('/', "-")
}

/// A value from the file system shown in the workspace file: control characters and
/// backticks replaced (a folder name cannot break out of its code span).
fn shown(s: &str) -> String {
    s.chars().map(|c| if c.is_control() || c == '`' { '?' } else { c }).collect()
}

/// What [`create`] made.
#[derive(Debug, Clone)]
pub struct Created {
    pub workspace: PathBuf,
    /// Primary first.
    pub repos: Vec<TaskRepo>,
    /// Each repo's main checkout and its worktree, for [`discard`].
    pub worktrees: Vec<(PathBuf, Worktree)>,
}

/// The text of [`FILE`]: each repo the task may change (name, worktree, branch, base, its
/// main checkout) and the read-only ones.
pub fn text(group: &Path, branch: &str, repos: &[(TaskRepo, String)], read_only: &[PathBuf]) -> String {
    let mut s = String::from("# Mirante workspace\n\n");
    s.push_str(&format!(
        "This task spans several repositories of `{}`. Each one you may change is a git worktree in \
         this folder, all on branch `{}`:\n\n",
        shown(&group.display().to_string()),
        shown(branch)
    ));
    for (r, base) in repos {
        let from = match base.as_str() {
            "" => "an existing branch, checked out as is".to_string(),
            b => format!("from `{}`", shown(b)),
        };
        s.push_str(&format!(
            "- **{}**: `{}` (branch `{}`, {from}; main checkout `{}`)\n",
            shown(&r.name),
            shown(&r.worktree_path),
            shown(&r.branch),
            shown(&r.repo_path)
        ));
    }
    s.push_str(
        "\nChange, build and commit inside those folders only, one commit per repository. This folder \
         itself is not a git repository: nothing written here is committed or reviewed.\n",
    );
    if !read_only.is_empty() {
        s.push_str(
            "\nRead-only: the folder's other repositories, for context. Read them; do not change them \
             (they are the person's own checkouts):\n\n",
        );
        for p in read_only {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            s.push_str(&format!("- {}: `{}`\n", shown(&name), shown(&p.display().to_string())));
        }
    }
    s
}

fn write_read_only(path: &Path, text: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444))?;
    Ok(())
}

/// The branch already exists in `repo`.
fn has_branch(repo: &Path, branch: &str) -> Result<bool> {
    let out = Git::new(repo)
        .args(["show-ref", "--verify", "--quiet", "--end-of-options", &format!("refs/heads/{branch}")])
        .output()?;
    Ok(out.status.success())
}

/// What a new branch in `repo` starts from, in words: its current branch, else (detached)
/// its commit.
fn base_label(repo: &Path) -> String {
    worktree::current_branch(repo).unwrap_or_else(|_| {
        Git::new(repo)
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "HEAD".into())
    })
}

/// Creates the workspace for `branch`: one worktree per selected repo (primary first) and
/// the workspace files. `base` is `None` (an existing branch is checked out as is, a new one
/// starts from the repo's current branch) or `Some("HEAD")` (always a new branch, from the
/// repo's HEAD: a branch that exists in any selected repo is refused, naming it). Nothing is
/// left behind on failure.
pub fn create(
    mgr: &WorktreeManager,
    group: &Path,
    selected: &[Selected],
    branch: &str,
    base: Option<&str>,
) -> Result<Created> {
    if base.is_some_and(|b| b != "HEAD") {
        return Err(bad(
            "a task across several repositories starts each one from its own current branch: leave the base out",
        ));
    }
    worktree::validate_branch_name(branch)?;
    let ws = mgr.workspaces().join(slug(branch));
    if std::fs::symlink_metadata(&ws).is_ok() {
        return Err(CoreError::InvalidBranch {
            name: branch.to_string(),
            reason: format!("branch already exists: its workspace folder {} is taken", ws.display()),
        });
    }
    // Every check before anything is made.
    let mut bases = Vec::with_capacity(selected.len());
    for s in selected {
        let exists = has_branch(&s.repo, branch)?;
        if exists && base.is_some() {
            return Err(CoreError::InvalidBranch {
                name: branch.to_string(),
                reason: format!("branch already exists in {}; a base only applies to a new branch", s.name),
            });
        }
        if !exists && base.is_none() && worktree::current_branch(&s.repo).is_err() {
            return Err(bad(format!("{} is on a detached HEAD: check out a branch there first", s.name)));
        }
        bases.push(if exists { String::new() } else { base_label(&s.repo) });
    }
    let read_only: Vec<PathBuf> = {
        let mine: HashSet<&PathBuf> = selected.iter().map(|s| &s.repo).collect();
        let root = std::fs::canonicalize(mgr.root()).unwrap_or_else(|_| mgr.root().to_path_buf());
        group_checkouts(group).into_iter().filter(|p| !mine.contains(p) && !p.starts_with(&root)).collect()
    };
    std::fs::create_dir_all(&ws)?;
    let mut made: Vec<(PathBuf, Worktree)> = Vec::new();
    let mut repos = Vec::new();
    let undo = |made: &[(PathBuf, Worktree)]| {
        for (repo, wt) in made.iter().rev() {
            let _ = mgr.discard(repo, wt);
        }
        let _ = std::fs::remove_dir_all(&ws);
    };
    for s in selected {
        match mgr.create_at(&s.repo, &ws.join(&s.name), branch, base) {
            Ok(wt) => {
                repos.push(TaskRepo {
                    name: s.name.clone(),
                    repo_path: s.repo.to_string_lossy().into_owned(),
                    worktree_path: wt.path.to_string_lossy().into_owned(),
                    branch: branch.to_string(),
                    created_branch: wt.created_branch,
                    head: wt.head.clone(),
                });
                made.push((s.repo.clone(), wt));
            }
            Err(e) => {
                undo(&made);
                return Err(e);
            }
        }
    }
    let labeled: Vec<(TaskRepo, String)> = repos.iter().cloned().zip(bases).collect();
    let body = text(group, branch, &labeled, &read_only);
    let written = write_read_only(&ws.join(FILE), &body)
        .and_then(|()| write_read_only(&ws.join("AGENTS.md"), &body))
        .and_then(|()| write_read_only(&ws.join("CLAUDE.md"), &format!("@{FILE}\n")));
    if let Err(e) = written {
        undo(&made);
        return Err(e);
    }
    Ok(Created { workspace: ws, repos, worktrees: made })
}

/// Undoes [`create`] (the task could not be recorded): every worktree discarded (branches
/// only if made and unmoved), then the workspace folder.
pub fn discard(mgr: &WorktreeManager, created: &Created) -> Result<()> {
    let mut first_err = None;
    for (repo, wt) in created.worktrees.iter().rev() {
        if let Err(e) = mgr.discard(repo, wt) {
            first_err.get_or_insert(e);
        }
    }
    if first_err.is_none() {
        remove_folder(&created.workspace)?;
    }
    first_err.map_or(Ok(()), Err)
}

/// Mirante's own files, then the (now empty) workspace folder. Gone already: fine.
fn remove_folder(ws: &Path) -> Result<()> {
    for f in OWN_FILES {
        match std::fs::remove_file(ws.join(f)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    match std::fs::remove_dir(ws) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Removes a workspace task's worktrees (branches kept) and its folder. Refuses before
/// removing anything when any repo has uncommitted work, or the agent left files in the
/// workspace folder itself ([`CoreError::DirtyWorkspace`], naming each).
pub fn remove(mgr: &WorktreeManager, workspace: &Path, repos: &[TaskRepo]) -> Result<()> {
    let root = worktree::resolve(&mgr.workspaces())?;
    let ws = worktree::resolve(workspace)?;
    if ws == root || !ws.starts_with(&root) {
        return Err(CoreError::OutsideRoot { path: ws.display().to_string(), root: root.display().to_string() });
    }
    let mut dirty = Vec::new();
    for r in repos {
        match mgr.check_clean(Path::new(&r.worktree_path)) {
            Ok(()) => {}
            Err(CoreError::DirtyWorktree { changes, more, .. }) => dirty.push((r.name.clone(), changes, more)),
            Err(e) => return Err(e),
        }
    }
    if let Ok(entries) = std::fs::read_dir(&ws) {
        let known: HashSet<String> =
            repos.iter().map(|r| r.name.clone()).chain(OWN_FILES.iter().map(|f| f.to_string())).collect();
        let mut extra: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !known.contains(n))
            .map(|n| format!("?? {n}"))
            .collect();
        extra.sort();
        if !extra.is_empty() {
            let more = extra.len().saturating_sub(10);
            extra.truncate(10);
            dirty.push(("the workspace folder".to_string(), extra, more));
        }
    }
    if !dirty.is_empty() {
        return Err(CoreError::DirtyWorkspace { dirty });
    }
    for r in repos {
        mgr.remove(Path::new(&r.repo_path), Path::new(&r.worktree_path))?;
    }
    remove_folder(&ws)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;
    use std::process::Command;

    fn sh(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(ok.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ok.stderr));
    }

    /// `<dir>/gol` with repos `Site-B2C` (branch main), `bff-flight` (branch trunk) and
    /// `docs`, plus a plain folder.
    fn group(dir: &TempDir) -> PathBuf {
        let g = dir.path().join("gol");
        for (name, b) in [("Site-B2C", "main"), ("bff-flight", "trunk"), ("docs", "main")] {
            let r = g.join(name);
            std::fs::create_dir_all(&r).unwrap();
            sh(&r, &["init", "-q", "-b", b]);
            std::fs::write(r.join("README.md"), format!("{name}\n")).unwrap();
            sh(&r, &["add", "."]);
            sh(&r, &["commit", "-q", "-m", "init"]);
        }
        std::fs::create_dir_all(g.join("notes")).unwrap();
        std::fs::canonicalize(g).unwrap()
    }

    fn sel(g: &Path, names: &[&str], root: &Path) -> Vec<Selected> {
        let paths: Vec<String> = names.iter().map(|n| g.join(n).to_string_lossy().into_owned()).collect();
        validate_selection(g, &paths, root).unwrap()
    }

    #[test]
    fn creates_one_worktree_per_selected_repo_on_one_branch_and_names_the_others_read_only() {
        let d = TempDir::new("ws-create");
        let g = group(&d);
        let mgr = WorktreeManager::new(&d.path().join("wt"));
        let s = sel(&g, &["Site-B2C", "bff-flight"], mgr.root());
        let c = create(&mgr, &g, &s, "mirante/fix-login", Some("HEAD")).unwrap();
        assert_eq!(c.workspace, d.path().join("wt/_workspaces/fix-login"));
        let names: Vec<&str> = c.repos.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["Site-B2C", "bff-flight"]);
        for r in &c.repos {
            let wt = Path::new(&r.worktree_path);
            assert_eq!(wt.parent().unwrap(), c.workspace);
            assert_eq!(worktree::current_branch(wt).unwrap(), "mirante/fix-login");
            assert!(r.created_branch);
        }
        let text = std::fs::read_to_string(c.workspace.join(FILE)).unwrap();
        assert!(text.contains("**Site-B2C**") && text.contains("from `main`"), "{text}");
        assert!(text.contains("**bff-flight**") && text.contains("from `trunk`"), "{text}");
        assert!(text.contains("Read-only") && text.contains("- docs: "), "{text}");
        assert!(!text.contains("notes"), "a plain folder is not a repo: {text}");
        assert_eq!(std::fs::read_to_string(c.workspace.join("AGENTS.md")).unwrap(), text);
        assert_eq!(std::fs::read_to_string(c.workspace.join("CLAUDE.md")).unwrap(), "@MIRANTE_WORKSPACE.md\n");
        // The workspace folder is not a git repository: the agent's notes there are never committed.
        assert!(toplevel(&c.workspace).is_none());
        remove(&mgr, &c.workspace, &c.repos).unwrap();
        assert!(!c.workspace.exists());
        for r in &c.repos {
            assert!(has_branch(Path::new(&r.repo_path), "mirante/fix-login").unwrap(), "branch kept");
        }
    }

    #[test]
    fn a_branch_taken_in_any_selected_repo_is_refused_before_anything_is_made() {
        let d = TempDir::new("ws-collide");
        let g = group(&d);
        let mgr = WorktreeManager::new(&d.path().join("wt"));
        sh(&g.join("bff-flight"), &["branch", "mirante/x"]);
        let s = sel(&g, &["Site-B2C", "bff-flight"], mgr.root());
        let e = create(&mgr, &g, &s, "mirante/x", Some("HEAD")).unwrap_err();
        assert!(e.to_string().contains("branch already exists in bff-flight"), "{e}");
        assert!(!mgr.workspaces().join("x").exists());
        assert!(!has_branch(&g.join("Site-B2C"), "mirante/x").unwrap());
        // The rename the clients do: -2 is free everywhere.
        let c = create(&mgr, &g, &s, "mirante/x-2", Some("HEAD")).unwrap();
        // Without a base, an existing branch is checked out as is (the single-repo rule).
        let c2 = create(&mgr, &g, &sel(&g, &["bff-flight"], mgr.root()), "mirante/x", None).unwrap();
        assert!(!c2.repos[0].created_branch);
        // A workspace folder that is taken is a taken name too.
        let e = create(&mgr, &g, &sel(&g, &["docs"], mgr.root()), "mirante/x-2", Some("HEAD")).unwrap_err();
        assert!(e.to_string().contains("branch already exists"), "{e}");
        // Another base than HEAD is refused.
        let e = create(&mgr, &g, &s, "mirante/y", Some("main")).unwrap_err();
        assert!(matches!(e, CoreError::InvalidWorkspace(_)), "{e}");
        discard(&mgr, &c).unwrap();
        assert!(!c.workspace.exists());
        assert!(!has_branch(&g.join("Site-B2C"), "mirante/x-2").unwrap(), "made and unmoved: deleted");
    }

    #[test]
    fn a_failure_halfway_leaves_nothing_behind() {
        let d = TempDir::new("ws-half");
        let g = group(&d);
        let mgr = WorktreeManager::new(&d.path().join("wt"));
        let s = sel(&g, &["Site-B2C", "bff-flight"], mgr.root());
        // bff-flight's branch is checked out in a worktree already: git refuses a second one.
        sh(&g.join("bff-flight"), &["branch", "mirante/z"]);
        let elsewhere = d.path().join("elsewhere");
        sh(&g.join("bff-flight"), &["worktree", "add", "-q", elsewhere.to_str().unwrap(), "mirante/z"]);
        let e = create(&mgr, &g, &s, "mirante/z", None).unwrap_err();
        assert!(matches!(e, CoreError::Git { .. }), "{e}");
        assert!(!mgr.workspaces().join("z").exists());
        assert!(!has_branch(&g.join("Site-B2C"), "mirante/z").unwrap(), "the first repo's new branch is undone");
    }

    #[test]
    fn archive_refuses_when_any_repo_is_dirty_naming_each_and_removes_nothing() {
        let d = TempDir::new("ws-dirty");
        let g = group(&d);
        let mgr = WorktreeManager::new(&d.path().join("wt"));
        let c = create(&mgr, &g, &sel(&g, &["Site-B2C", "bff-flight", "docs"], mgr.root()), "mirante/d", None).unwrap();
        std::fs::write(Path::new(&c.repos[0].worktree_path).join("wip.txt"), "x\n").unwrap();
        std::fs::write(Path::new(&c.repos[2].worktree_path).join("README.md"), "changed\n").unwrap();
        let e = remove(&mgr, &c.workspace, &c.repos).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("uncommitted changes in Site-B2C, docs"), "{msg}");
        assert!(msg.contains("?? wip.txt") && msg.contains("M README.md") && !msg.contains("bff-flight"), "{msg}");
        for r in &c.repos {
            assert!(Path::new(&r.worktree_path).exists(), "nothing removed");
        }
        std::fs::remove_file(Path::new(&c.repos[0].worktree_path).join("wip.txt")).unwrap();
        sh(Path::new(&c.repos[2].worktree_path), &["checkout", "--", "README.md"]);
        // Scratch the agent left in the workspace folder itself is not deleted either.
        std::fs::write(c.workspace.join("plan.md"), "notes\n").unwrap();
        let e = remove(&mgr, &c.workspace, &c.repos).unwrap_err();
        assert!(e.to_string().contains("the workspace folder") && e.to_string().contains("?? plan.md"), "{e}");
        std::fs::remove_file(c.workspace.join("plan.md")).unwrap();
        remove(&mgr, &c.workspace, &c.repos).unwrap();
        assert!(!c.workspace.exists());
        // Gone already (deleted by hand): archiving again prunes and succeeds.
        remove(&mgr, &c.workspace, &c.repos).unwrap();
    }

    #[test]
    fn selection_is_checked_against_the_group() {
        let d = TempDir::new("ws-validate");
        let g = group(&d);
        let root = d.path().join("wt");
        let v = |paths: &[PathBuf]| {
            let p: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
            validate_selection(&g, &p, &root)
        };
        assert!(v(&[]).is_err());
        assert!(v(std::slice::from_ref(&g)).is_err(), "the group itself");
        assert!(v(&[g.join("notes")]).unwrap_err().to_string().contains("not a git repository"));
        assert!(v(&[g.join("Site-B2C/..")]).is_err(), "resolves to the group");
        assert!(v(&[g.join("Site-B2C"), g.join("Site-B2C/.")]).unwrap_err().to_string().contains("twice"));
        // A subfolder of a checkout is not its top.
        std::fs::create_dir_all(g.join("Site-B2C/src")).unwrap();
        assert!(v(&[g.join("Site-B2C/src")]).unwrap_err().to_string().contains("top folder"));
        assert!(validate_selection(&g, &["Site-B2C".into()], &root).unwrap_err().to_string().contains("absolute"));
        // A repo outside the group, and a symlink inside the group that points to it.
        let outside = d.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        sh(&outside, &["init", "-q"]);
        sh(&outside, &["commit", "-q", "--allow-empty", "-m", "i"]);
        assert!(v(std::slice::from_ref(&outside)).unwrap_err().to_string().contains("not inside"));
        std::os::unix::fs::symlink(&outside, g.join("sneaky")).unwrap();
        assert!(v(&[g.join("sneaky")]).unwrap_err().to_string().contains("not inside"));
        // The symlink is not a read-only repo either (never followed).
        assert!(!group_checkouts(&g).iter().any(|p| p.ends_with("sneaky") || p.ends_with("outside")));
        // Names clash: -2; never one of Mirante's own file names.
        let deep = g.join("team/docs");
        std::fs::create_dir_all(&deep).unwrap();
        sh(&deep, &["init", "-q"]);
        sh(&deep, &["commit", "-q", "--allow-empty", "-m", "i"]);
        let names: Vec<String> = v(&[g.join("docs"), deep.clone()]).unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["docs", "docs-2"]);
        assert!(group_checkouts(&g).contains(&std::fs::canonicalize(&deep).unwrap()), "two levels deep");
        // The group: absolute, a folder, outside Mirante's worktrees.
        assert!(validate_group("gol", &root).is_err());
        assert!(validate_group(g.join("Site-B2C/README.md").to_str().unwrap(), &root).is_err());
        std::fs::create_dir_all(root.join("x")).unwrap();
        assert!(validate_group(root.join("x").to_str().unwrap(), &root).is_err());
        assert_eq!(validate_group(g.to_str().unwrap(), &root).unwrap(), g);
    }

    #[test]
    fn workspace_names_and_text_are_safe() {
        assert_eq!(slug("mirante/fix-login"), "fix-login");
        assert_eq!(slug("pedro/a/b"), "pedro-a-b");
        let r = TaskRepo {
            name: "we`ird\nname".into(),
            repo_path: "/g/x".into(),
            worktree_path: "/w/x".into(),
            branch: "b".into(),
            created_branch: true,
            head: String::new(),
        };
        let t = text(Path::new("/g"), "b", &[(r, "main".into())], &[]);
        assert!(t.contains("we?ird?name") && !t.contains("Read-only"), "{t}");
    }

    #[test]
    fn the_lifecycle_never_runs_code_the_repos_configure() {
        let d = TempDir::new("ws-traps");
        let g = group(&d);
        let marks = d.path().join("marks");
        std::fs::create_dir_all(&marks).unwrap();
        for n in ["Site-B2C", "bff-flight"] {
            worktree::tests::plant_git_traps(&g.join(n), &marks);
        }
        let mgr = WorktreeManager::new(&d.path().join("wt"));
        let s = sel(&g, &["Site-B2C", "bff-flight"], mgr.root());
        let c = create(&mgr, &g, &s, "mirante/t", Some("HEAD")).unwrap();
        remove(&mgr, &c.workspace, &c.repos).unwrap();
        assert_eq!(worktree::tests::sprung(&marks), Vec::<String>::new());
    }
}
