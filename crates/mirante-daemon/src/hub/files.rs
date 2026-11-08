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
use mirante_ipc::types::RepoFiles;

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
}
