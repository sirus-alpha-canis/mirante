//! `repo.files`: a repository's files matching what a person typed, for the cockpit's
//! "+ context" picker (attach a file to a goal by path). git runs hardened (see
//! `mirante_core::git`), without the hub lock; only paths come back, never contents.
use std::path::Path;

use mirante_core::git::Git;
use mirante_ipc::types::RepoFiles;

use super::{Hub, HubError, Result};

/// Paths returned when the request names no `limit`, and the most it may ask for.
pub const FILES_DEFAULT: usize = 30;
pub const FILES_MAX: usize = 200;
/// Paths read from git at most (a monorepo lists the first ones only).
const SCAN_MAX: usize = 200_000;

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

/// The best `limit` matches of `query` among `paths`: by rank, then shorter, then by name.
pub fn best(paths: impl IntoIterator<Item = String>, query: &str, limit: usize) -> Vec<String> {
    let q = query.trim().to_lowercase();
    let mut hits: Vec<(u8, String)> = paths.into_iter().filter_map(|p| rank(&p, &q).map(|r| (r, p))).collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.len().cmp(&b.1.len())).then(a.1.cmp(&b.1)));
    hits.into_iter().take(limit).map(|(_, p)| p).collect()
}

impl Hub {
    /// Tracked and untracked (not ignored) files of the repo at `p.repo` matching `p.query`.
    pub fn repo_files(&self, p: &RepoFiles) -> Result<Vec<String>> {
        let repo = std::fs::canonicalize(&p.repo).map_err(|e| HubError::Invalid(format!("repo {}: {e}", p.repo)))?;
        let out = Git::new(Path::new(&repo))
            .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
            .output()
            .map_err(|e| HubError::Conflict(format!("git: {e}")))?;
        if !out.status.success() {
            return Err(HubError::Invalid(format!("{} is not a git checkout", repo.display())));
        }
        let paths = out
            .stdout
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .take(SCAN_MAX)
            .filter_map(|s| std::str::from_utf8(s).ok().map(str::to_string));
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

    #[test]
    fn lists_tracked_and_untracked_files_but_not_ignored_ones() {
        let k = Kit::new();
        std::fs::write(k.repo.join(".gitignore"), "secret.env\n").unwrap();
        std::fs::write(k.repo.join("secret.env"), "x").unwrap();
        std::fs::create_dir_all(k.repo.join("src")).unwrap();
        std::fs::write(k.repo.join("src/readme_notes.md"), "x").unwrap();
        let repo = k.repo.to_string_lossy().into_owned();
        let q = |query: &str, limit| {
            k.hub.repo_files(&RepoFiles { repo: repo.clone(), query: query.into(), limit }).unwrap()
        };
        assert_eq!(q("readme", None), ["README", "src/readme_notes.md"]);
        assert!(q("secret", None).is_empty(), "ignored files are not offered");
        assert_eq!(q("", Some(1)).len(), 1);
        let none = k.hub.repo_files(&RepoFiles { repo: "/definitely/not/here".into(), query: "a".into(), limit: None });
        assert!(matches!(none, Err(HubError::Invalid(_))), "{none:?}");
        let plain = k.dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let plain = RepoFiles { repo: plain.to_string_lossy().into_owned(), query: "a".into(), limit: None };
        let r = k.hub.repo_files(&plain);
        assert!(matches!(r, Err(HubError::Invalid(ref m)) if m.contains("not a git checkout")), "{r:?}");
    }
}
