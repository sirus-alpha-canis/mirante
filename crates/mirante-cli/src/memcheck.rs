//! `mirante memory check --offline`: every active note's anchors checked straight from the
//! files and git, with no daemon (the "Memory CI" seed): the working tree's notes at HEAD,
//! or, with `--rev`, the notes committed at that revision, at that revision. Uses
//! the same core as the daemon: `dna::index::scan` (safe reads of `.mirante/memory`) and
//! `dna::check::check_all` (hardened git).
use std::path::{Path, PathBuf};

use mirante_core::dna::check::{check_all, rev_parse, AnchorState};
use mirante_core::dna::index::{scan, scan_at};
use mirante_core::git::Git;

use crate::render::{self, check_line};

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

/// The message for a shallow clone (exit 3: the check could not run).
pub const SHALLOW: &str = "shallow clone: the commits the notes cite are not in it, so nothing can be checked; \
     fetch full history (e.g. actions/checkout with fetch-depth: 0, or git fetch --unshallow)";

/// Err with [`SHALLOW`] when `repo` is a shallow clone (`rev-parse --is-shallow-repository`
/// through the hardened git): there every cited commit would read as gone.
pub fn refuse_shallow(repo: &Path) -> Result<(), String> {
    let out =
        Git::new(repo).args(["rev-parse", "--is-shallow-repository"]).output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!("git rev-parse: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    match String::from_utf8_lossy(&out.stdout).trim() {
        "false" => Ok(()),
        "true" => Err(SHALLOW.into()),
        other => Err(format!("unexpected rev-parse --is-shallow-repository output: {other:?}")),
    }
}

/// Checks the active notes of the checkout `repo`. Without `rev`: the notes as they are in
/// the working tree (uncommitted ones too, like the daemon), against HEAD. With `rev`: the
/// notes as committed at `rev` (read through git, never the working tree), against `rev`.
/// Returns the report and the full sha it checked against. A shallow clone is an error
/// ([`refuse_shallow`]), not a list of unverifiable anchors.
pub fn check(repo: &Path, rev: Option<&str>) -> Result<(Report, String), String> {
    refuse_shallow(repo)?;
    let sha = rev_parse(repo, rev.unwrap_or("HEAD")).map_err(|e| e.to_string())?;
    let found = match rev {
        None => scan(repo),
        Some(_) => scan_at(repo, &sha),
    }
    .map_err(|e| e.to_string())?;
    let mut r = Report::default();
    for bad in &found.invalid {
        r.invalid += 1;
        r.lines.push(render::invalid_line(&bad.file, &bad.error));
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

        let (r, _) = check(&repo, None).unwrap();
        assert_eq!((r.notes, r.anchors, r.failed()), (1, 1, false), "{r:?}");
        assert_eq!(r.lines, ["fresh  a-note  a.rs:1-1"]);

        commit(&repo, "a.rs", "uno\n");
        commit(&repo, "b.rs", "dos\n");
        let (r, sha) = check(&repo, None).unwrap();
        assert_eq!((r.stale, r.failed()), (1, true), "{r:?}");
        assert!(r.lines[0].starts_with("stale  a-note  a.rs:1-1"), "{:?}", r.lines);
        assert!(r.summary(&sha[..7]).contains("1 stale"));

        // Without --rev: the files as they are now (an uncommitted note too), at HEAD.
        std::fs::write(repo.join(".mirante/memory/c-note.md"), "no frontmatter").unwrap();
        let (r, _) = check(&repo, None).unwrap();
        assert_eq!((r.stale, r.invalid, r.failed()), (1, 1, true), "{r:?}");
        assert!(r.lines.iter().any(|l| l.starts_with("invalid  c-note.md")), "{:?}", r.lines);
        // With --rev: the notes as committed at that revision, checked there.
        let (r, sha) = check(&repo, Some("HEAD~2")).unwrap();
        assert_eq!((r.notes, r.stale, r.invalid, r.failed()), (1, 0, 0, false), "{r:?}");
        assert_eq!(sha.len(), 40);
        assert!(check(&repo, Some("no-such-rev")).is_err());
        assert!(toplevel(d.path()).is_err());
    }

    #[test]
    fn rev_reads_the_notes_committed_at_that_revision() {
        let d = TestDir::new();
        let repo = d.path().join("r");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "trunk"]);
        commit(&repo, "a.rs", "one\n");
        note(&repo, "a-note", Status::Active, "a.rs");
        let base = rev_parse(&repo, "HEAD").unwrap();
        // Later: the note is retired in the working tree only, and another note is added
        // and committed; a symlink and a nested file in the directory are not notes.
        commit(&repo, "b.rs", "two\n");
        note(&repo, "b-note", Status::Active, "b.rs");
        std::os::unix::fs::symlink("a-note.md", repo.join(".mirante/memory/link.md")).unwrap();
        commit(&repo, ".mirante/memory/sub/x.md", "not a note\n");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "link"]);
        std::fs::remove_file(repo.join(".mirante/memory/a-note.md")).unwrap();
        let (r, _) = check(&repo, Some(&base)).unwrap();
        assert_eq!((r.notes, r.anchors, r.invalid), (1, 1, 0), "{r:?}");
        assert_eq!(r.lines, ["fresh  a-note  a.rs:1-1"]);
        let (r, _) = check(&repo, Some("HEAD")).unwrap();
        assert_eq!((r.notes, r.invalid), (2, 0), "committed at HEAD, whatever the working tree says: {r:?}");
        let (r, _) = check(&repo, None).unwrap();
        assert_eq!(r.notes, 1, "the working tree: {r:?}");
    }

    #[test]
    fn a_shallow_clone_cannot_be_checked_and_says_how_to_fix_it() {
        let d = TestDir::new();
        let repo = d.path().join("r");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "trunk"]);
        commit(&repo, "a.rs", "one\n");
        note(&repo, "a-note", Status::Active, "a.rs");
        commit(&repo, "b.rs", "two\n");
        assert_eq!(refuse_shallow(&repo), Ok(()));
        let url = format!("file://{}", repo.display());
        let clone = d.path().join("c");
        git(d.path(), &["clone", "-q", "--depth", "1", &url, clone.to_str().unwrap()]);
        let err = check(&clone, None).unwrap_err();
        assert!(err.contains("shallow clone") && err.contains("fetch-depth: 0"), "{err}");
        assert!(refuse_shallow(&clone).is_err());
    }

    #[test]
    fn a_parse_error_is_printed_without_terminal_controls() {
        let d = TestDir::new();
        let repo = d.path().join("r");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "trunk"]);
        commit(&repo, "a.rs", "one\n");
        let evil = "---\ntitle: \"\u{1b}]0;owned\u{7}\u{1b}[2J\n---\nbody\n";
        commit(&repo, ".mirante/memory/evil.md", evil);
        let (r, _) = check(&repo, None).unwrap();
        assert_eq!(r.invalid, 1, "{r:?}");
        let line = r.lines.iter().find(|l| l.starts_with("invalid  evil.md")).expect("the invalid line");
        assert!(!line.contains(['\u{1b}', '\u{7}', '\n']), "{line:?}");
    }
}
