//! `mirante memory check --offline`: every active note's anchors checked against a
//! revision straight from the files and git, with no daemon (the "Memory CI" seed). Uses
//! the same core as the daemon: `dna::index::scan` (safe reads of `.mirante/memory`) and
//! `dna::check::check_all` (hardened git).
use std::path::{Path, PathBuf};

use mirante_core::dna::check::{check_all, rev_parse, AnchorState};
use mirante_core::dna::index::scan;
use mirante_core::git::Git;

use crate::render::check_line;

/// What a check found: one printable line per anchor (and per broken note), and counts.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub lines: Vec<String>,
    pub notes: usize,
    pub anchors: usize,
    pub stale: usize,
    pub unverifiable: usize,
    /// Notes whose file does not parse.
    pub invalid: usize,
}

impl Report {
    /// A check that should fail a CI job: a stale or unverifiable anchor, or a broken note.
    pub fn failed(&self) -> bool {
        self.stale + self.unverifiable + self.invalid > 0
    }

    pub fn summary(&self, rev: &str) -> String {
        format!(
            "{} notes, {} anchors checked at {rev}: {} stale, {} unverifiable, {} invalid",
            self.notes, self.anchors, self.stale, self.unverifiable, self.invalid
        )
    }
}

/// The checkout `dir` is in (git's top level), through the hardened git.
pub fn toplevel(dir: &Path) -> Result<PathBuf, String> {
    let out = Git::new(dir).args(["rev-parse", "--show-toplevel"]).output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!("{} is not inside a git checkout", dir.display()));
    }
    let top = String::from_utf8_lossy(&out.stdout).trim_end_matches(['\n', '\r']).to_string();
    std::fs::canonicalize(&top).map_err(|e| format!("{top}: {e}"))
}

/// Checks the active notes of the checkout `repo` against `rev`. Returns the report and
/// the full sha `rev` names.
pub fn check(repo: &Path, rev: &str) -> Result<(Report, String), String> {
    let sha = rev_parse(repo, rev).map_err(|e| e.to_string())?;
    let found = scan(repo).map_err(|e| e.to_string())?;
    let mut r = Report::default();
    for bad in &found.invalid {
        r.invalid += 1;
        r.lines.push(format!("invalid  {}  {}", mirante_core::dna::untrusted::line(&bad.file, 200), bad.error));
    }
    let active: Vec<_> = found.notes.iter().filter(|n| n.parse_error.is_none() && n.status == "active").collect();
    let anchors: Vec<_> = active.iter().flat_map(|n| n.anchors.iter().cloned()).collect();
    let checked = check_all(repo, &sha, &anchors).map_err(|e| e.to_string())?;
    let mut it = checked.into_iter();
    for n in &active {
        r.notes += 1;
        for _ in &n.anchors {
            let Some(c) = it.next() else { break };
            r.anchors += 1;
            let a = &c.anchor;
            let (state, reason) = match &c.state {
                AnchorState::Unchecked => ("unchecked", None),
                AnchorState::Fresh => ("fresh", None),
                AnchorState::Relocated { start, end } => ("relocated", Some(format!("now at lines {start}-{end}"))),
                AnchorState::Stale { reason } => {
                    r.stale += 1;
                    ("stale", Some(reason.clone()))
                }
                AnchorState::Unverifiable { reason } => {
                    r.unverifiable += 1;
                    ("unverifiable", Some(reason.clone()))
                }
            };
            r.lines.push(check_line(state, &n.slug, &a.path, a.start, a.end, reason.as_deref()));
        }
    }
    Ok((r, sha))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_core::dna::check::cite;
    use mirante_core::dna::note::{render, Kind, Note, Status};
    use mirante_ipc::test_dir::TestDir;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=T", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn commit(dir: &Path, path: &str, text: &str) {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
        git(dir, &["add", "-A", "--", path]);
        git(dir, &["commit", "-q", "-m", path]);
    }

    fn note(repo: &Path, slug: &str, status: Status, path: &str) {
        let a = cite(repo, "HEAD", path, None, 1, 1).unwrap();
        let n = Note {
            id: "0193b7c2-5f1e-7c3a-9a10-3f2b8d1e4c55".into(),
            title: slug.into(),
            kind: Kind::Fact,
            status,
            created: "2026-11-07".into(),
            source: "cli".into(),
            anchors: vec![a],
            body: "b\n".into(),
        };
        commit(repo, &format!(".mirante/memory/{slug}.md"), &render(&n).unwrap());
    }

    #[test]
    fn stale_fails_retired_is_skipped_and_broken_notes_fail() {
        let d = TestDir::new();
        let repo = d.path().join("r");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "trunk"]);
        commit(&repo, "a.rs", "one\n");
        commit(&repo, "b.rs", "two\n");
        note(&repo, "a-note", Status::Active, "a.rs");
        note(&repo, "b-note", Status::Retired, "b.rs");
        assert_eq!(toplevel(&repo.join(".mirante")).unwrap(), repo.canonicalize().unwrap());

        let (r, _) = check(&repo, "HEAD").unwrap();
        assert_eq!((r.notes, r.anchors, r.failed()), (1, 1, false), "{r:?}");
        assert_eq!(r.lines, ["fresh  a-note  a.rs:1-1"]);

        commit(&repo, "a.rs", "uno\n");
        commit(&repo, "b.rs", "dos\n");
        let (r, sha) = check(&repo, "HEAD").unwrap();
        assert_eq!((r.stale, r.failed()), (1, true), "{r:?}");
        assert!(r.lines[0].starts_with("stale  a-note  a.rs:1-1"), "{:?}", r.lines);
        assert!(r.summary(&sha[..7]).contains("1 stale"));

        std::fs::write(repo.join(".mirante/memory/c-note.md"), "no frontmatter").unwrap();
        let (r, _) = check(&repo, "HEAD~2").unwrap();
        assert_eq!((r.stale, r.invalid, r.failed()), (0, 1, true), "{r:?}");
        assert!(r.lines.iter().any(|l| l.starts_with("invalid  c-note.md")), "{:?}", r.lines);
        assert!(check(&repo, "no-such-rev").is_err());
        assert!(toplevel(d.path()).is_err());
    }
}
