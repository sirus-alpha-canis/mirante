//! `task.files` / `task.file`: a task's worktree files and one file's contents, for the
//! cockpit's side pane and quick-open (person only; see `task_file`).
//!
//! `repo.files`: a repository's files matching what a person typed, for the cockpit's
//! "+ context" picker (attach a file to a goal by path). git runs hardened (see
//! `mirante_core::git`), without the hub lock and with a time limit; only paths come back,
//! never contents.
//!
//! Only the files committed at the checkout's `HEAD`: an agent's worktree is made from it,
//! so an untracked or merely staged file would not be there for the agent to read. Only a
//! project's checkout root is listed (never a folder inside it, nor an agent's worktree).
//! Files that usually hold secrets (`.env`, `*.pem`, `*.key`, `id_*` keys) rank last; the
//! cockpit also flags them. Committed symlinks and submodules are not offered.
use std::path::Path;
use std::time::Duration;

use mirante_core::git::Git;
use mirante_core::model::{Lifecycle, TaskRepo};
use mirante_core::workspace;
use mirante_core::worktree_file as wf;
use mirante_ipc::types::{FileImage, FileKind, FileView, RepoFiles, TaskFile, TaskFiles};

use super::{Hub, HubError, Result};

/// Paths returned when the request names no `limit`, and the most it may ask for.
pub const FILES_DEFAULT: usize = 30;
pub const FILES_MAX: usize = 200;
/// Paths read from git at most (a monorepo lists the first ones only).
const SCAN_MAX: usize = 200_000;
/// Longest a listing may take (the hub's default `git_read_limit`).
pub const GIT_LIMIT: Duration = Duration::from_secs(5);

/// The path of one `git ls-tree -r` entry (`<mode> <type> <object>\t<path>`) when it is a
/// regular file: a symlink (mode 120000) can point anywhere on the machine and a
/// submodule (160000) is not a file, so neither is offered.
fn regular_file(entry: &str) -> Option<String> {
    let (meta, path) = entry.split_once('\t')?;
    let mode = meta.split(' ').next()?;
    matches!(mode, "100644" | "100755").then(|| path.to_string())
}

/// A path whose file usually holds a secret: `.env` (and `.env.*`), `*.pem`, `*.key`, or an
/// ssh key (`id_rsa`, `id_ed25519`, their `.pub`).
pub fn may_hold_secrets(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"].iter().any(|k| name == *k || name == format!("{k}.pub"))
}

/// How well `path` matches `q` (lowercase): lower is better, `None` is no match. The file
/// name starting with it, then containing it, then the path containing it, then its
/// letters in order anywhere in the path.
pub fn rank(path: &str, q: &str) -> Option<u8> {
    if q.is_empty() {
        return Some(4);
    }
    let p = path.to_lowercase();
    let name = p.rsplit('/').next().unwrap_or(&p);
    if name.starts_with(q) {
        return Some(0);
    }
    if name.contains(q) {
        return Some(1);
    }
    if p.contains(q) {
        return Some(2);
    }
    let mut it = p.chars();
    q.chars().all(|c| it.any(|x| x == c)).then_some(3)
}

/// The best `limit` matches of `query` among `paths`: files that may hold secrets last,
/// then by rank, then shorter, then by name.
pub fn best(paths: impl IntoIterator<Item = String>, query: &str, limit: usize) -> Vec<String> {
    let q = query.trim().to_lowercase();
    let mut hits: Vec<(bool, u8, String)> =
        paths.into_iter().filter_map(|p| rank(&p, &q).map(|r| (may_hold_secrets(&p), r, p))).collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.len().cmp(&b.2.len())).then(a.2.cmp(&b.2)));
    hits.into_iter().take(limit).map(|(_, _, p)| p).collect()
}

impl Hub {
    /// Committed files of the checkout at `p.repo` matching `p.query` (see the module doc).
    pub fn repo_files(&self, p: &RepoFiles) -> Result<Vec<String>> {
        let repo = std::fs::canonicalize(&p.repo).map_err(|e| HubError::Invalid(format!("repo {}: {e}", p.repo)))?;
        let limit = self.git_read_limit;
        let timed_out = |e: std::io::Error| HubError::Conflict(format!("git: {e}"));
        let top = match Git::new(&repo).args(["rev-parse", "--show-toplevel"]).output_timeout(limit) {
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return Err(timed_out(e)),
            r => r.ok(),
        };
        let top = top
            .filter(|o| o.status.success())
            .and_then(|o| std::fs::canonicalize(String::from_utf8_lossy(&o.stdout).trim()).ok());
        let Some(top) = top else {
            return Err(HubError::Invalid(format!("{} is not a git checkout", repo.display())));
        };
        if top != repo {
            return Err(HubError::Invalid(format!(
                "{} is inside the checkout {}: name the checkout itself",
                repo.display(),
                top.display()
            )));
        }
        // An agent's worktree is refused (`checkout` says so).
        self.checkout(&repo.to_string_lossy())?;
        let out = Git::new(Path::new(&repo))
            .args(["ls-tree", "-r", "-z", "HEAD"])
            .output_timeout(limit)
            .map_err(timed_out)?;
        if !out.status.success() {
            // No commit yet: nothing an agent's worktree would have.
            return Ok(Vec::new());
        }
        let paths = out
            .stdout
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .take(SCAN_MAX)
            .filter_map(|s| std::str::from_utf8(s).ok())
            .filter_map(regular_file);
        Ok(best(paths, &p.query, p.limit.unwrap_or(FILES_DEFAULT).clamp(1, FILES_MAX)))
    }
}

/// The root a task path is read under, and the path's levels below it. The root is
/// Mirante's worktrees root and the levels include the worktree's own (its repo, branch, or
/// workspace and repo name): the descriptor walk then refuses a worktree folder an agent
/// swapped for a symlink, at any level.
struct Resolved {
    root: std::path::PathBuf,
    segs: Vec<String>,
    /// How many of `segs` lead to the worktree (or workspace) folder.
    base: usize,
}

impl Hub {
    /// A live (not archived) task and, for a workspace task, its repositories.
    fn files_task(&self, reference: &str) -> Result<(mirante_core::model::Task, Vec<TaskRepo>)> {
        let st = self.lock();
        let t = st.resolve(reference)?;
        if t.lifecycle == Lifecycle::Archived {
            return Err(HubError::NotFound(format!("task {} is archived; its worktree is gone", t.branch)));
        }
        let repos = if t.workspace_path.is_some() { st.store.task_repos(&t.id)? } else { Vec::new() };
        Ok((t, repos))
    }

    /// Files of a task's worktree (tracked and untracked, never ignored, never under
    /// `.git`) matching `p.query`; a workspace task's are `<repo>/…`, plus the workspace's
    /// own files. git runs hardened, without the hub lock and with a time limit.
    pub fn task_files(&self, p: &TaskFiles) -> Result<Vec<String>> {
        let (t, repos) = self.files_task(&p.task)?;
        let limit = p.limit.unwrap_or(FILES_DEFAULT).clamp(1, FILES_MAX);
        let mut all = Vec::new();
        match &t.workspace_path {
            None => all.extend(self.worktree_files(&self.checked_worktree(&t).map_err(HubError::Conflict)?)?),
            Some(ws) => {
                for r in &self.checked_repos(&t, &repos).map_err(HubError::Conflict)? {
                    let files = self.worktree_files(Path::new(&r.worktree_path))?;
                    all.extend(files.into_iter().map(|f| format!("{}/{f}", r.name)));
                }
                for own in workspace::OWN_FILES {
                    if std::fs::symlink_metadata(Path::new(ws).join(own)).is_ok_and(|m| m.is_file()) {
                        all.push(own.to_string());
                    }
                }
            }
        }
        // git lists untracked files first: a stable order before ranking.
        all.sort();
        all.dedup();
        all.truncate(SCAN_MAX);
        Ok(best(all, &p.query, limit))
    }

    /// One worktree's files: tracked regular files (by their index mode: never a symlink or
    /// a submodule) and untracked, not ignored ones that are regular files on disk.
    fn worktree_files(&self, wt: &Path) -> Result<Vec<String>> {
        let limit = self.git_read_limit;
        let run = |args: &[&str]| -> Result<Vec<u8>> {
            let out =
                Git::new(wt).args(args).output_timeout(limit).map_err(|e| HubError::Conflict(format!("git: {e}")))?;
            if !out.status.success() {
                return Err(HubError::Conflict(format!(
                    "git ls-files in {}: {}",
                    wt.display(),
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            Ok(out.stdout)
        };
        let names = |b: &[u8]| -> Vec<String> {
            b.split(|c| *c == 0)
                .filter(|s| !s.is_empty())
                .take(SCAN_MAX)
                .filter_map(|s| std::str::from_utf8(s).ok().map(str::to_string))
                .collect()
        };
        let tracked = run(&["ls-files", "-z", "-s"])?;
        // `<mode> <object> <stage>\t<path>`, the same shape `regular_file` reads.
        let mut out: Vec<String> = names(&tracked).iter().filter_map(|e| regular_file(e)).collect();
        out.dedup();
        let untracked = run(&["ls-files", "-z", "-o", "--exclude-standard"])?;
        out.extend(
            names(&untracked)
                .into_iter()
                .filter(|p| std::fs::symlink_metadata(wt.join(p)).is_ok_and(|m| m.file_type().is_file())),
        );
        Ok(out)
    }

    /// Where `path` lives for task `t`: its worktree, or for a workspace task the selected
    /// repo its first level names (or one of the workspace's own files). Read from
    /// Mirante's worktrees root down (see [`Resolved`]).
    fn resolve_file(&self, t: &mirante_core::model::Task, repos: &[TaskRepo], segs: Vec<&str>) -> Result<Resolved> {
        let levels = |p: &str| self.worktrees.rel_levels(Path::new(p)).map_err(|e| HubError::Invalid(e.to_string()));
        let at = |base: Vec<String>, rest: &[&str]| {
            let n = base.len();
            let mut segs = base;
            segs.extend(rest.iter().map(|s| s.to_string()));
            Ok(Resolved { root: self.worktrees.root().to_path_buf(), segs, base: n })
        };
        let Some(ws) = &t.workspace_path else {
            return at(levels(&t.worktree_path)?, &segs);
        };
        if segs.len() == 1 && workspace::OWN_FILES.contains(&segs[0]) {
            return at(levels(ws)?, &segs);
        }
        match repos.iter().find(|r| r.name == segs[0]) {
            Some(r) if segs.len() > 1 => {
                // The recorded worktree is the workspace's `<name>` folder (else refused).
                workspace::repo_dir(&self.worktrees, Path::new(ws), r).map_err(|e| HubError::Invalid(e.to_string()))?;
                at(levels(ws)?, &segs)
            }
            _ => Err(HubError::Invalid(format!(
                "{}: not a file of the task's repositories ({})",
                segs.join("/"),
                repos.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", ")
            ))),
        }
    }

    /// One file of the task's worktree, read by descriptor walk (see
    /// `mirante_core::worktree_file`): never through a symlink, never under `.git`, capped,
    /// sniffed. A file that may hold secrets is not read until `reveal`.
    pub fn task_file(&self, p: &TaskFile) -> Result<FileView> {
        let segs = wf::check_rel(&p.path).map_err(|e| HubError::Invalid(e.to_string()))?;
        let (t, repos) = self.files_task(&p.task)?;
        let at = self.resolve_file(&t, &repos, segs)?;
        let all: Vec<&str> = at.segs.iter().map(String::as_str).collect();
        // Errors name the path as the person gave it, not Mirante's levels above it.
        let base = at.segs[..at.base].join("/");
        let shown = |m: String| m.strip_prefix(&base).map(|r| r.trim_start_matches('/').to_string()).unwrap_or(m);
        let read = wf::read_in(&at.root, &all, wf::CAPS).map_err(|e| match e {
            wf::FileError::Missing(m) => HubError::NotFound(format!("{} does not exist", shown(m))),
            wf::FileError::Symlink(m) => HubError::Invalid(wf::FileError::Symlink(shown(m)).to_string()),
            other => HubError::Invalid(other.to_string()),
        })?;
        // Joined only now, after a read through descriptors succeeded.
        let root = std::fs::canonicalize(&at.root).map_err(|e| HubError::NotFound(e.to_string()))?;
        let abs = at.segs.iter().fold(root, |a, s| a.join(s)).to_string_lossy().into_owned();
        let mut v = FileView {
            path: p.path.clone(),
            abs,
            size: read.size,
            kind: FileKind::Binary,
            text: None,
            truncated: false,
            image: None,
            lang: None,
        };
        if may_hold_secrets(&p.path) && !p.reveal {
            v.kind = FileKind::SecretHidden;
            return Ok(v);
        }
        match read.kind {
            wf::Kind::Text { text, truncated, lang } => {
                v.kind = FileKind::Text;
                v.lang = lang.map(str::to_string);
                if !p.stat {
                    v.text = Some(text);
                    v.truncated = truncated;
                }
            }
            wf::Kind::Image { mime, bytes } => {
                v.kind = FileKind::Image;
                if !p.stat {
                    v.image = Some(FileImage { mime: mime.into(), b64: wf::base64(&bytes) });
                }
            }
            wf::Kind::Binary => v.kind = FileKind::Binary,
            wf::Kind::TooLarge => v.kind = FileKind::TooLarge,
        }
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::Kit;
    use super::*;

    fn owned(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ranks_the_file_name_first_then_the_path_then_letters_in_order() {
        let paths = ["src/app/login.ts", "docs/login-flow.md", "src/user_login.rs", "loginx/readme", "lib/l_o_g.rs"];
        let got = best(owned(&paths), "Login", 10);
        assert_eq!(got, ["src/app/login.ts", "docs/login-flow.md", "src/user_login.rs", "loginx/readme"]);
        assert_eq!(best(owned(&paths), "ulr", 10), ["src/user_login.rs"]);
        assert_eq!(best(owned(&paths), "", 2).len(), 2);
        assert_eq!(rank("a/b", "zz"), None);
    }

    fn git(repo: &std::path::Path, args: &[&str]) {
        let ok = std::process::Command::new("git").arg("-C").arg(repo).args(args).output().unwrap();
        assert!(ok.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ok.stderr));
    }

    /// Review 4: an agent's worktree is made from the checkout's HEAD, so only committed
    /// files are offered (an untracked or merely staged one would not be there).
    #[test]
    fn lists_committed_files_only() {
        let k = Kit::new();
        std::fs::create_dir_all(k.repo.join("src")).unwrap();
        std::fs::write(k.repo.join("src/readme_notes.md"), "x").unwrap();
        std::fs::write(k.repo.join("readme_draft.md"), "x").unwrap();
        std::fs::write(k.repo.join("readme_staged.md"), "x").unwrap();
        git(&k.repo, &["add", "src/readme_notes.md"]);
        git(&k.repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "notes"]);
        git(&k.repo, &["add", "readme_staged.md"]);
        let repo = k.repo.to_string_lossy().into_owned();
        let q = |query: &str, limit| {
            k.hub.repo_files(&RepoFiles { repo: repo.clone(), query: query.into(), limit }).unwrap()
        };
        assert_eq!(q("readme", None), ["README", "src/readme_notes.md"]);
        assert_eq!(q("", Some(1)).len(), 1);
        let none = k.hub.repo_files(&RepoFiles { repo: "/definitely/not/here".into(), query: "a".into(), limit: None });
        assert!(matches!(none, Err(HubError::Invalid(_))), "{none:?}");
        let plain = k.dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let plain = RepoFiles { repo: plain.to_string_lossy().into_owned(), query: "a".into(), limit: None };
        let r = k.hub.repo_files(&plain);
        assert!(matches!(r, Err(HubError::Invalid(ref m)) if m.contains("not a git checkout")), "{r:?}");
    }

    /// Review (minor): only a project's checkout, never a folder inside one or an agent's
    /// worktree.
    #[test]
    fn only_a_checkout_root_is_listed() {
        let k = Kit::new();
        std::fs::create_dir_all(k.repo.join("sub")).unwrap();
        let sub =
            RepoFiles { repo: k.repo.join("sub").to_string_lossy().into_owned(), query: String::new(), limit: None };
        let r = k.hub.repo_files(&sub);
        assert!(matches!(r, Err(HubError::Invalid(ref m)) if m.contains("checkout")), "{r:?}");
        let t = k.create(mirante_core::model::AgentKind::Claude, "wt").task;
        let wt = RepoFiles { repo: t.worktree_path.clone(), query: String::new(), limit: None };
        let r = k.hub.repo_files(&wt);
        assert!(matches!(r, Err(HubError::Invalid(ref m)) if m.contains("worktree")), "{r:?}");
    }

    /// A committed symlink is not a file to read (it can point anywhere on the machine),
    /// and a submodule is not a file at all: neither is offered.
    #[test]
    fn committed_symlinks_are_not_offered() {
        let k = Kit::new();
        std::os::unix::fs::symlink("/etc/hosts", k.repo.join("hosts_link")).unwrap();
        std::fs::write(k.repo.join("hosts_notes.md"), "x").unwrap();
        git(&k.repo, &["add", "hosts_link", "hosts_notes.md"]);
        git(&k.repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "links"]);
        let r = k.hub.repo_files(&RepoFiles {
            repo: k.repo.to_string_lossy().into_owned(),
            query: "hosts".into(),
            limit: None,
        });
        assert_eq!(r.unwrap(), ["hosts_notes.md"]);
    }

    /// A listing that takes too long is an error, through the hub (the cockpit's picker
    /// says so instead of waiting).
    #[test]
    fn a_slow_listing_times_out_through_the_hub() {
        let mut k = Kit::new();
        std::sync::Arc::get_mut(&mut k.hub).unwrap().set_git_read_limit(Duration::ZERO);
        let r = k.hub.repo_files(&RepoFiles {
            repo: k.repo.to_string_lossy().into_owned(),
            query: String::new(),
            limit: None,
        });
        assert!(matches!(r, Err(HubError::Conflict(ref m)) if m.contains("longer")), "{r:?}");
    }

    /// Review (minor): tracked files that usually hold secrets come last.
    #[test]
    fn files_that_may_hold_secrets_rank_last() {
        let paths = ["config/.env", "deploy/server.pem", "keys/id_ed25519", "src/env.rs", "envoy.yaml"];
        assert_eq!(best(owned(&paths), "env", 10), ["envoy.yaml", "src/env.rs", "config/.env"]);
        assert_eq!(best(owned(&paths), "", 10)[..2], ["envoy.yaml", "src/env.rs"]);
        for p in ["config/.env", ".env.local", "deploy/server.pem", "keys/id_ed25519", "id_rsa.pub", "a/b.key"] {
            assert!(may_hold_secrets(p), "{p}");
        }
        for p in ["src/env.rs", "docs/identity.md", "keyboard.ts", "README"] {
            assert!(!may_hold_secrets(p), "{p}");
        }
    }

    // ---- task.files / task.file (the cockpit's side pane) ----

    fn tf(k: &Kit, task: &str, path: &str, reveal: bool) -> Result<FileView> {
        k.hub.task_file(&TaskFile { task: task.into(), path: path.into(), reveal, stat: false })
    }

    #[test]
    fn task_files_lists_tracked_and_untracked_not_ignored_in_the_worktree() {
        let k = Kit::new();
        let t = k.create(mirante_core::model::AgentKind::Claude, "wt").task;
        let wt = std::path::Path::new(&t.worktree_path);
        std::fs::write(wt.join(".gitignore"), "ignored.log\n").unwrap();
        std::fs::write(wt.join("ignored.log"), "x").unwrap();
        std::fs::write(wt.join("new_untracked.rs"), "x").unwrap();
        std::os::unix::fs::symlink("/etc/hosts", wt.join("new_link")).unwrap();
        let got = k.hub.task_files(&TaskFiles { task: t.id.clone(), query: String::new(), limit: None }).unwrap();
        assert!(got.contains(&"README".to_string()), "{got:?}");
        assert!(got.contains(&"new_untracked.rs".to_string()), "{got:?}");
        assert!(got.contains(&".gitignore".to_string()), "{got:?}");
        assert!(!got.iter().any(|p| p == "ignored.log" || p == "new_link" || p.starts_with(".git/")), "{got:?}");
        let q = k.hub.task_files(&TaskFiles { task: t.id, query: "untr".into(), limit: Some(1) }).unwrap();
        assert_eq!(q, ["new_untracked.rs"]);
    }

    #[test]
    fn task_files_for_a_workspace_task_prefix_each_repo_name() {
        let k = Kit::new();
        let t = ws_task(&k);
        let got = k.hub.task_files(&TaskFiles { task: t.id.clone(), query: "readme".into(), limit: None }).unwrap();
        assert_eq!(got, ["one/README.md", "two/README.md"]);
        let own = k.hub.task_files(&TaskFiles { task: t.id, query: "MIRANTE_WORK".into(), limit: None }).unwrap();
        assert_eq!(own, ["MIRANTE_WORKSPACE.md"]);
    }

    #[test]
    fn task_file_reads_inside_the_worktree_and_returns_its_abs_path() {
        let k = Kit::new();
        let t = k.create(mirante_core::model::AgentKind::Claude, "wt").task;
        let wt = std::fs::canonicalize(&t.worktree_path).unwrap();
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::write(wt.join("src/main.rs"), "fn main() {}\n").unwrap();
        let v = tf(&k, &t.id, "src/main.rs", false).unwrap();
        assert_eq!(v.kind, FileKind::Text);
        assert_eq!(v.text.as_deref(), Some("fn main() {}\n"));
        assert_eq!(v.lang.as_deref(), Some("rust"));
        assert_eq!(v.abs, wt.join("src/main.rs").to_string_lossy());
        assert_eq!(v.size, 13);
        // stat: the path, no contents.
        let s =
            k.hub.task_file(&TaskFile { task: t.id.clone(), path: "src/main.rs".into(), reveal: false, stat: true });
        let s = s.unwrap();
        assert_eq!((s.kind, s.text, s.abs), (FileKind::Text, None, v.abs));
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend([1u8, 2, 3]);
        std::fs::write(wt.join("logo.png"), &png).unwrap();
        let img = tf(&k, &t.id, "logo.png", false).unwrap();
        assert_eq!(img.kind, FileKind::Image);
        assert_eq!(img.image.unwrap().mime, "image/png");
    }

    #[test]
    fn task_file_reads_a_workspace_repo_and_the_workspace_files() {
        let k = Kit::new();
        let t = ws_task(&k);
        let v = tf(&k, &t.id, "two/README.md", false).unwrap();
        assert_eq!(v.text.as_deref(), Some("two\n"));
        assert!(v.abs.ends_with("/two/README.md"), "{}", v.abs);
        assert_eq!(tf(&k, &t.id, "MIRANTE_WORKSPACE.md", false).unwrap().kind, FileKind::Text);
        // Not a selected repo, nor one of the workspace's own files.
        for bad in ["three/README.md", "stray.txt", "two"] {
            assert!(tf(&k, &t.id, bad, false).is_err(), "{bad}");
        }
    }

    #[test]
    fn task_file_refuses_traversal_symlink_and_dot_git() {
        let k = Kit::new();
        let t = k.create(mirante_core::model::AgentKind::Claude, "wt").task;
        let wt = std::path::Path::new(&t.worktree_path);
        std::os::unix::fs::symlink("/etc", wt.join("etc")).unwrap();
        std::os::unix::fs::symlink("/etc/hosts", wt.join("hosts")).unwrap();
        for bad in ["../app/README", "/etc/hosts", "etc/hosts", "hosts", ".git", ".git/HEAD", "a/../README"] {
            let r = tf(&k, &t.id, bad, false);
            assert!(r.is_err(), "{bad}: {r:?}");
        }
        let r = tf(&k, &t.id, "hosts", false);
        assert!(matches!(r, Err(HubError::Invalid(ref m)) if m.contains("symlink")), "{r:?}");
        let r = tf(&k, &t.id, "missing.rs", false);
        assert!(matches!(r, Err(HubError::NotFound(_))), "{r:?}");
    }

    #[test]
    fn secret_looking_files_are_hidden_until_reveal() {
        let k = Kit::new();
        let t = k.create(mirante_core::model::AgentKind::Claude, "wt").task;
        std::fs::write(std::path::Path::new(&t.worktree_path).join(".env"), "TOKEN=abc\n").unwrap();
        let hidden = tf(&k, &t.id, ".env", false).unwrap();
        assert_eq!((hidden.kind, hidden.text), (FileKind::SecretHidden, None));
        let shown = tf(&k, &t.id, ".env", true).unwrap();
        assert_eq!(shown.text.as_deref(), Some("TOKEN=abc\n"));
    }

    #[test]
    fn an_archived_task_has_no_files() {
        let k = Kit::new();
        let t = k.create(mirante_core::model::AgentKind::Claude, "wt").task;
        k.hub.archive(&t.id).unwrap();
        assert!(matches!(tf(&k, &t.id, "README", false), Err(HubError::NotFound(_))));
        let l = k.hub.task_files(&TaskFiles { task: t.id, query: String::new(), limit: None });
        assert!(matches!(l, Err(HubError::NotFound(_))), "{l:?}");
    }

    #[test]
    fn agents_are_refused_task_file_and_task_files() {
        use crate::access::check;
        use crate::hub::dna::Identity;
        use mirante_ipc::rpc::code;
        use mirante_ipc::types::method;
        let agent = Identity::Agent { task: Some("t".into()) };
        for m in [method::TASK_FILE, method::TASK_FILES] {
            assert_eq!(check(m, &agent).unwrap_err().code, code::FORBIDDEN, "{m}");
            assert!(check(m, &Identity::Person).is_ok());
        }
    }

    /// A fake home folder holding a credentials file, outside every worktree.
    fn fake_home(k: &Kit) -> std::path::PathBuf {
        let home = k.dir.path().join("fakehome");
        std::fs::create_dir_all(home.join(".aws")).unwrap();
        std::fs::write(home.join(".aws/credentials"), "aws_secret_access_key=SECRET\n").unwrap();
        git(&home, &["init", "-q", "-b", "main"]);
        git(&home, &["add", "."]);
        git(&home, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "home"]);
        home
    }

    /// Review I1: the workspace folder is the agent's working directory, so it can swap a
    /// repo's folder for a symlink (to the home folder, to another task's worktree). The
    /// side pane, git and the archive must refuse it, not follow it.
    #[test]
    fn a_workspace_repo_swapped_for_a_symlink_is_refused_everywhere() {
        let k = Kit::new();
        let t = ws_task(&k);
        let ws = std::path::PathBuf::from(t.workspace_path.clone().unwrap());
        let home = fake_home(&k);
        // The agent: `rm -rf two && ln -s ~ two` (the worktree's own commit is clean).
        std::fs::remove_dir_all(ws.join("two")).unwrap();
        std::os::unix::fs::symlink(&home, ws.join("two")).unwrap();
        let r = tf(&k, &t.id, "two/.aws/credentials", true);
        assert!(r.is_err(), "the side pane read through the swap: {r:?}");
        let r = tf(&k, &t.id, "two/README.md", false);
        assert!(r.is_err(), "{r:?}");
        let l = k.hub.task_files(&TaskFiles { task: t.id.clone(), query: "cred".into(), limit: None });
        assert!(l.is_err(), "git listed the swap's target: {l:?}");
        let d = k.hub.task_diff(&t.id);
        assert!(d.is_err(), "git diffed the swap's target: {d:?}");
        let a = k.hub.archive(&t.id);
        assert!(a.is_err(), "archived through the swap: {a:?}");
        assert!(home.join(".aws/credentials").exists(), "the target is untouched");
        // The workspace's own files are still read (not through the swapped folder).
        assert_eq!(tf(&k, &t.id, "MIRANTE_WORKSPACE.md", false).unwrap().kind, FileKind::Text);
    }

    /// Review I1, archive: a repo folder swapped for a link to another task's (clean)
    /// worktree must not get that worktree removed.
    #[test]
    fn archiving_never_removes_the_worktree_a_swapped_folder_points_to() {
        let k = Kit::new();
        let t = ws_task(&k);
        // The victim: another task's clean worktree of the same repository.
        let other = ws_task_on(&k, "victim");
        let victim = std::path::PathBuf::from(other.workspace_path.clone().unwrap()).join("one");
        let ws = std::path::PathBuf::from(t.workspace_path.clone().unwrap());
        std::fs::remove_dir_all(ws.join("one")).unwrap();
        std::os::unix::fs::symlink(&victim, ws.join("one")).unwrap();
        assert!(k.hub.archive(&t.id).is_err());
        assert!(victim.join("README.md").exists(), "the other task's worktree stays");
    }

    /// The same for a single-repo task whose worktree folder was swapped.
    #[test]
    fn a_single_worktree_swapped_for_a_symlink_is_refused() {
        let k = Kit::new();
        let t = k.create(mirante_core::model::AgentKind::Claude, "wt").task;
        let home = fake_home(&k);
        std::fs::remove_dir_all(&t.worktree_path).unwrap();
        std::os::unix::fs::symlink(&home, &t.worktree_path).unwrap();
        assert!(tf(&k, &t.id, ".aws/credentials", true).is_err());
        assert!(k.hub.task_files(&TaskFiles { task: t.id.clone(), query: String::new(), limit: None }).is_err());
        assert!(k.hub.task_diff(&t.id).is_err());
        assert!(k.hub.archive(&t.id).is_err());
        assert!(home.join(".aws/credentials").exists());
    }

    /// A workspace task over `one` and `two` of a folder that also holds `three`.
    fn ws_task(k: &Kit) -> mirante_core::model::Task {
        let g = k.dir.path().join("grp");
        for name in ["one", "two", "three"] {
            let r = g.join(name);
            std::fs::create_dir_all(&r).unwrap();
            git(&r, &["init", "-q", "-b", "main"]);
            std::fs::write(r.join("README.md"), format!("{name}\n")).unwrap();
            git(&r, &["add", "."]);
            git(&r, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
        }
        ws_task_on(k, "ws")
    }

    /// Another workspace task over `one` and `two` of the folder [`ws_task`] made.
    fn ws_task_on(k: &Kit, branch: &str) -> mirante_core::model::Task {
        let g = std::fs::canonicalize(k.dir.path().join("grp")).unwrap();
        k.hub
            .create_task(mirante_ipc::types::CreateTask {
                group: None,
                repo: g.to_string_lossy().into_owned(),
                repos: ["one", "two"].iter().map(|n| g.join(n).to_string_lossy().into_owned()).collect(),
                agent: mirante_core::model::AgentKind::Claude,
                branch: branch.into(),
                base: None,
                goal: "g".into(),
                title: None,
            })
            .unwrap()
            .task
    }
}
