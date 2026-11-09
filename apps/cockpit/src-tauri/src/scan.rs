//! "Add project" given a folder that holds repositories (not one itself): the git checkouts
//! in it, two levels deep at most. Read-only and bounded: no shell, no git process, symlinks
//! never followed, hidden and build folders skipped, a cap on folders visited and repositories
//! found, and a time limit; `truncated` says one of them cut the walk short.
use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

/// What a git checkout is: its own repository (a `.git` folder), or a linked worktree of
/// another one (a `.git` file pointing into `<main>/.git/worktrees/<name>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoKind {
    Repo,
    Worktree,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FoundRepo {
    pub path: String,
    pub name: String,
    pub kind: RepoKind,
    /// A worktree's main repository (its path), when the `.git` file says.
    pub main_repo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Scan {
    /// The folder itself is a checkout (nothing below it is looked at then).
    pub root: Option<RepoKind>,
    /// Sorted by path.
    pub repos: Vec<FoundRepo>,
    /// A cap or the time limit stopped the walk before it was done.
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub depth: usize,
    pub max_dirs: usize,
    pub max_repos: usize,
    pub time: Duration,
}

pub const LIMITS: Limits = Limits { depth: 2, max_dirs: 500, max_repos: 200, time: Duration::from_secs(3) };

/// Folders never looked into (besides hidden ones): dependencies and build output.
const SKIPPED: &[&str] = &["node_modules", "target", "dist", "build"];

/// The checkout at `dir`, if it is one (its `.git` not followed if a symlink).
fn checkout(dir: &Path) -> Option<(RepoKind, Option<String>)> {
    let git = dir.join(".git");
    let meta = std::fs::symlink_metadata(&git).ok()?;
    if meta.is_dir() {
        return Some((RepoKind::Repo, None));
    }
    if !meta.is_file() || meta.len() > 4096 {
        return None;
    }
    let text = std::fs::read_to_string(&git).ok()?;
    let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let target = if Path::new(target).is_absolute() { PathBuf::from(target) } else { dir.join(target) };
    // `<main>/.git/worktrees/<name>`: a linked worktree. Anything else (a submodule's
    // `<super>/.git/modules/<name>`) is a repository of its own.
    // Lexically resolved (`../main/.git/…` from a relative gitdir), nothing read.
    let mut parts: Vec<Component> = Vec::new();
    for c in target.components() {
        match c {
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir => {}
            c => parts.push(c),
        }
    }
    let at = parts.windows(2).rposition(|w| w[0].as_os_str() == ".git" && w[1].as_os_str() == "worktrees");
    match at {
        Some(i) => {
            let main: PathBuf = parts[..i].iter().collect();
            Some((RepoKind::Worktree, main.to_str().map(str::to_owned)))
        }
        None => Some((RepoKind::Repo, None)),
    }
}

/// Validates `path` as an absolute, existing folder with no `..` (the webview's input).
fn folder(path: &str) -> Result<PathBuf, String> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(format!("{path} is not an absolute path"));
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(format!("{path} has \"..\" in it"));
    }
    match std::fs::metadata(p) {
        Ok(m) if m.is_dir() => Ok(p.to_path_buf()),
        Ok(_) => Err(format!("{path} is a file, not a folder")),
        Err(_) => Err(format!("{path} does not exist")),
    }
}

/// The checkouts in `path` (see the module doc).
pub fn scan(path: &str, limits: Limits) -> Result<Scan, String> {
    let root = folder(path)?;
    if let Some((kind, _)) = checkout(&root) {
        return Ok(Scan { root: Some(kind), repos: Vec::new(), truncated: false });
    }
    let started = Instant::now();
    let mut repos = Vec::new();
    let mut truncated = false;
    let mut visited = 0usize;
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::from([(root, 0)]);
    'walk: while let Some((dir, depth)) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            if started.elapsed() > limits.time || visited >= limits.max_dirs || repos.len() >= limits.max_repos {
                truncated = true;
                break 'walk;
            }
            // `file_type` does not follow symlinks: a link is never a folder here.
            let Ok(ft) = entry.file_type() else { continue };
            if !ft.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with('.') || SKIPPED.contains(&name) {
                continue;
            }
            visited += 1;
            let child = entry.path();
            match checkout(&child) {
                Some((kind, main_repo)) => repos.push(FoundRepo {
                    path: child.to_string_lossy().into_owned(),
                    name: name.to_owned(),
                    kind,
                    main_repo,
                }),
                None if depth + 1 < limits.depth => queue.push_back((child, depth + 1)),
                None => {}
            }
        }
    }
    repos.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Scan { root: None, repos, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("mirante-scan-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            // macOS: /var → /private/var, so paths compare as the walk reports them.
            Tmp(p.canonicalize().unwrap())
        }
        fn repo(&self, rel: &str) -> PathBuf {
            let d = self.0.join(rel);
            std::fs::create_dir_all(d.join(".git")).unwrap();
            d
        }
        fn worktree(&self, rel: &str, main: &Path) -> PathBuf {
            let d = self.0.join(rel);
            std::fs::create_dir_all(&d).unwrap();
            let name = d.file_name().unwrap().to_str().unwrap().to_owned();
            std::fs::write(d.join(".git"), format!("gitdir: {}/.git/worktrees/{name}\n", main.display())).unwrap();
            d
        }
        fn dir(&self, rel: &str) -> PathBuf {
            let d = self.0.join(rel);
            std::fs::create_dir_all(&d).unwrap();
            d
        }
        fn s(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn paths(s: &Scan) -> Vec<String> {
        s.repos.iter().map(|r| r.path.clone()).collect()
    }

    #[test]
    fn finds_repositories_and_worktrees_two_levels_deep() {
        let t = Tmp::new("deep");
        let main = t.repo("shop");
        let wt = t.worktree("shop-fix", &main);
        let nested = t.repo("team/api");
        t.repo("team/api/inner"); // inside a checkout: not looked at
        t.repo("a/b/too-deep");
        t.dir("docs");
        let s = scan(t.s(), LIMITS).unwrap();
        assert_eq!(s.root, None);
        assert!(!s.truncated);
        assert_eq!(
            paths(&s),
            vec![
                main.to_str().unwrap().to_owned(),
                wt.to_str().unwrap().to_owned(),
                nested.to_str().unwrap().to_owned()
            ]
        );
        let w = s.repos.iter().find(|r| r.name == "shop-fix").unwrap();
        assert_eq!(w.kind, RepoKind::Worktree);
        assert_eq!(w.main_repo.as_deref(), main.to_str());
        let r = s.repos.iter().find(|r| r.name == "shop").unwrap();
        assert_eq!((r.kind, r.main_repo.clone()), (RepoKind::Repo, None));
        assert_eq!(serde_json::to_value(RepoKind::Worktree).unwrap(), "worktree");
    }

    #[test]
    fn a_checkout_is_reported_as_itself_and_not_walked() {
        let t = Tmp::new("self");
        let main = t.repo("shop");
        t.repo("shop/vendor/lib");
        let s = scan(main.to_str().unwrap(), LIMITS).unwrap();
        assert_eq!((s.root, s.repos.len()), (Some(RepoKind::Repo), 0));
        let wt = t.worktree("wt", &main);
        assert_eq!(scan(wt.to_str().unwrap(), LIMITS).unwrap().root, Some(RepoKind::Worktree));
    }

    #[test]
    fn skips_hidden_dependency_and_build_folders() {
        let t = Tmp::new("skip");
        for d in [".cache/r", "node_modules/r", "target/r", "dist/r", "build/r"] {
            t.repo(d);
        }
        t.repo("ok");
        let s = scan(t.s(), LIMITS).unwrap();
        assert_eq!(s.repos.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["ok"]);
    }

    #[test]
    fn never_follows_symlinks() {
        let t = Tmp::new("link");
        let elsewhere = Tmp::new("link-target");
        elsewhere.repo("secret");
        std::os::unix::fs::symlink(elsewhere.0.join("secret"), t.0.join("linked-repo")).unwrap();
        std::os::unix::fs::symlink(&elsewhere.0, t.0.join("linked-dir")).unwrap();
        // A `.git` that is a link is no checkout either.
        let d = t.dir("fake");
        std::os::unix::fs::symlink(elsewhere.0.join("secret/.git"), d.join(".git")).unwrap();
        let s = scan(t.s(), LIMITS).unwrap();
        assert!(s.repos.is_empty(), "{:?}", s.repos);
    }

    #[test]
    fn caps_folders_and_repositories_and_says_so() {
        let t = Tmp::new("caps");
        for i in 0..12 {
            t.repo(&format!("r{i:02}"));
        }
        let few = Limits { max_repos: 5, ..LIMITS };
        let s = scan(t.s(), few).unwrap();
        assert_eq!((s.repos.len(), s.truncated), (5, true));
        let narrow = Limits { max_dirs: 3, ..LIMITS };
        let s = scan(t.s(), narrow).unwrap();
        assert_eq!((s.repos.len(), s.truncated), (3, true));
        let none = Limits { time: Duration::ZERO, ..LIMITS };
        std::thread::sleep(Duration::from_millis(2));
        assert!(scan(t.s(), none).unwrap().truncated);
        let s = scan(t.s(), LIMITS).unwrap();
        assert_eq!((s.repos.len(), s.truncated), (12, false));
    }

    #[test]
    fn refuses_relative_dotdot_missing_and_file_paths() {
        let t = Tmp::new("bad");
        std::fs::write(t.0.join("f"), "x").unwrap();
        assert!(scan("relative/dir", LIMITS).unwrap_err().contains("absolute"));
        assert!(scan(&format!("{}/../x", t.s()), LIMITS).unwrap_err().contains(".."));
        assert!(scan(&format!("{}/nope", t.s()), LIMITS).unwrap_err().contains("does not exist"));
        assert!(scan(&format!("{}/f", t.s()), LIMITS).unwrap_err().contains("file"));
    }

    #[test]
    fn a_relative_gitdir_and_a_submodule_are_read_right() {
        let t = Tmp::new("rel");
        let main = t.repo("main");
        let d = t.dir("wt");
        std::fs::write(d.join(".git"), "gitdir: ../main/.git/worktrees/wt\n").unwrap();
        let sub = t.dir("sub");
        std::fs::write(sub.join(".git"), format!("gitdir: {}/.git/modules/sub\n", main.display())).unwrap();
        let s = scan(t.s(), LIMITS).unwrap();
        let wt = s.repos.iter().find(|r| r.name == "wt").unwrap();
        assert_eq!(wt.kind, RepoKind::Worktree);
        assert_eq!(wt.main_repo.as_deref(), main.to_str());
        assert_eq!(s.repos.iter().find(|r| r.name == "sub").unwrap().kind, RepoKind::Repo);
    }
}
