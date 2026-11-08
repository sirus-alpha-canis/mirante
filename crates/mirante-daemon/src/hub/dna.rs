//! DNA, the repo memory: the daemon side of `dna.*`.
//!
//! The index lives in the store and is rebuilt from `<repo>/.mirante/memory` lazily: every
//! read checks HEAD and the directory's stat signature first ([`index::needs_refresh`]).
//! git and file reads run without the hub lock; only the store writes take it.
use std::path::{Path, PathBuf};

use mirante_core::dna::index::{self, IndexedAnchor, NoteRow, RefreshReport};
use mirante_core::dna::{fsio, note, DnaError};
use mirante_core::git::Git;
use mirante_ipc::types::{
    AnchorView, CheckReport, DnaGet, DnaGraph, DnaRef, DnaSearch, DnaStatus, GraphLink, NoteSummary, NoteView,
};
use serde_json::json;

use super::{Hub, HubError, Result};
use mirante_core::dna::check::AnchorState;

/// Who is calling: a person (in-process, tests), a socket peer by pid, or a peer whose pid
/// could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    Person,
    Peer(i32),
    Unknown,
}

/// Search results by default and at most.
const SEARCH_DEFAULT: usize = 10;
const SEARCH_MAX: usize = 25;

/// A DNA failure as the hub reports it: bad input is `Invalid`, everything else (unsafe
/// directory, git, I/O, a file that changed meanwhile) is `Conflict`.
pub(crate) fn dna_err(e: DnaError) -> HubError {
    match e {
        DnaError::Anchor(..) | DnaError::Path(..) | DnaError::Note(_) => HubError::Invalid(e.to_string()),
        DnaError::Unsafe(_) | DnaError::Git(_) | DnaError::Changed | DnaError::Io(_) => {
            HubError::Conflict(e.to_string())
        }
    }
}

fn state_name(s: &AnchorState) -> &'static str {
    match s {
        AnchorState::Unchecked => "unchecked",
        AnchorState::Fresh => "fresh",
        AnchorState::Relocated { .. } => "relocated",
        AnchorState::Stale { .. } => "stale",
        AnchorState::Unverifiable { .. } => "unverifiable",
    }
}

fn anchor_view(a: &IndexedAnchor) -> AnchorView {
    let (reason, now) = match &a.state {
        AnchorState::Stale { reason } | AnchorState::Unverifiable { reason } => (Some(reason.clone()), None),
        AnchorState::Relocated { start, end } => (None, Some((*start, *end))),
        _ => (None, None),
    };
    AnchorView {
        path: a.anchor.path.clone(),
        symbol: a.anchor.symbol.clone(),
        start: a.anchor.start,
        end: a.anchor.end,
        commit: a.anchor.commit.clone(),
        state: state_name(&a.state).to_string(),
        reason,
        now_start: now.map(|n| n.0),
        now_end: now.map(|n| n.1),
    }
}

/// The worst anchor state: stale, then unverifiable, then unchecked, else fresh.
fn worst(anchors: &[IndexedAnchor]) -> &'static str {
    let has = |f: fn(&AnchorState) -> bool| anchors.iter().any(|a| f(&a.state));
    if has(|s| matches!(s, AnchorState::Stale { .. })) {
        "stale"
    } else if has(|s| matches!(s, AnchorState::Unverifiable { .. })) {
        "unverifiable"
    } else if anchors.is_empty() || has(|s| matches!(s, AnchorState::Unchecked)) {
        "unchecked"
    } else {
        "fresh"
    }
}

pub(crate) fn summary(n: &NoteRow, reviewed: bool) -> NoteSummary {
    NoteSummary {
        slug: n.slug.clone(),
        title: n.title.clone(),
        kind: n.kind.clone(),
        status: n.status.clone(),
        state: worst(&n.anchors).to_string(),
        reviewed,
        anchors: n.anchors.iter().map(anchor_view).collect(),
    }
}

fn key_of(repo: &Path) -> String {
    repo.to_string_lossy().into_owned()
}

impl Hub {
    /// The repo checkout `at` names, canonical: a task's repo, or a path inside a git work
    /// tree (its top level). Agent worktrees are refused: they are never indexed.
    pub(crate) fn dna_repo_of(&self, at: &DnaRef) -> Result<PathBuf> {
        if let Some(task) = &at.task {
            let t = self.lock().resolve(task)?;
            return Ok(PathBuf::from(t.repo_path));
        }
        let Some(repo) = &at.repo else {
            return Err(HubError::Invalid("name a repo or a task".into()));
        };
        let p = std::fs::canonicalize(repo).map_err(|e| HubError::Invalid(format!("repo {repo}: {e}")))?;
        let out = Git::new(&p).args(["rev-parse", "--show-toplevel"]).output();
        let top = match out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            _ => return Err(HubError::Invalid(format!("{repo} is not inside a git work tree"))),
        };
        let top = std::fs::canonicalize(&top).map_err(|e| HubError::Invalid(format!("repo {top}: {e}")))?;
        let worktrees = self.paths.worktrees();
        let worktrees = std::fs::canonicalize(&worktrees).unwrap_or(worktrees);
        if top.starts_with(&worktrees) {
            return Err(HubError::Invalid(format!(
                "{} is an agent worktree; repo memory lives in the repo checkout",
                top.display()
            )));
        }
        Ok(top)
    }

    /// Reindexes `repo` (git and file reads unlocked), then emits `dna.indexed`.
    pub(crate) fn dna_refresh(&self, repo: &Path) -> Result<RefreshReport> {
        let scan = index::scan(repo).map_err(dna_err)?;
        let checks = index::check_scan(repo, &scan).map_err(dna_err)?;
        let key = key_of(repo);
        let mut st = self.lock();
        let rep = index::apply(&mut st.store, &key, &scan, &checks)?;
        st.dna_sigs.insert(key.clone(), scan.signature);
        st.event(
            None,
            "dna.indexed",
            json!({ "repo": key, "notes": rep.notes, "stale": rep.stale, "unreviewed": rep.unreviewed }),
        )?;
        st.publish();
        Ok(rep)
    }

    /// Reindexes `repo` when HEAD or the memory directory changed since the last time.
    pub(crate) fn dna_ensure_fresh(&self, repo: &Path) -> Result<()> {
        let probe = index::probe(repo).map_err(dna_err)?;
        let key = key_of(repo);
        let stale = {
            let st = self.lock();
            index::needs_refresh(&st.store, &key, &probe, st.dna_sigs.get(&key).map(String::as_str))?
        };
        if stale {
            self.dna_refresh(repo)?;
        }
        Ok(())
    }

    fn dna_status_of(&self, repo: &Path) -> Result<DnaStatus> {
        let dir = fsio::memory_dir(repo).is_ok();
        if dir {
            self.dna_ensure_fresh(repo)?;
        }
        let key = key_of(repo);
        let st = self.lock();
        let row = st.store.dna_repo(&key)?;
        let c = st.store.dna_status(&key)?;
        drop(st);
        let head = match row.as_ref().and_then(|r| r.checked_head.clone()) {
            Some(h) => Some(h),
            None => mirante_core::dna::check::rev_parse(repo, "HEAD").ok(),
        };
        Ok(DnaStatus {
            repo: key,
            enabled: row.is_some_and(|r| r.enabled),
            dir,
            notes: c.notes,
            stale: c.stale,
            unreviewed: c.unreviewed,
            pending: c.pending,
            invalid: c.invalid,
            head,
        })
    }

    pub fn dna_status(&self, at: &DnaRef) -> Result<DnaStatus> {
        let repo = self.dna_repo_of(at)?;
        self.dna_status_of(&repo)
    }

    /// Creates `.mirante/memory/README.md` (kept if present) and turns injection on for new
    /// sessions of this repo's tasks.
    pub fn dna_enable(&self, at: &DnaRef, caller: &Caller) -> Result<DnaStatus> {
        let _ = caller;
        let repo = self.dna_repo_of(at)?;
        fsio::init(&repo).map_err(dna_err)?;
        {
            let mut st = self.lock();
            st.store.dna_set_enabled(&key_of(&repo), true)?;
        }
        self.dna_refresh(&repo)?;
        self.dna_status_of(&repo)
    }

    /// Turns injection off for new sessions. The files are left alone.
    pub fn dna_disable(&self, at: &DnaRef, caller: &Caller) -> Result<DnaStatus> {
        let _ = caller;
        let repo = self.dna_repo_of(at)?;
        self.lock().store.dna_set_enabled(&key_of(&repo), false)?;
        self.dna_status_of(&repo)
    }

    /// Reindexes and rechecks every anchor at the checkout's HEAD.
    pub fn dna_check(&self, at: &DnaRef) -> Result<CheckReport> {
        let repo = self.dna_repo_of(at)?;
        self.dna_refresh(&repo)
    }

    pub(crate) fn dna_summaries(&self, key: &str, rows: &[NoteRow]) -> Result<Vec<NoteSummary>> {
        let st = self.lock();
        rows.iter().map(|n| Ok(summary(n, st.store.dna_is_reviewed(key, &n.slug, &n.content_hash)?))).collect()
    }

    pub fn dna_search(&self, p: DnaSearch) -> Result<Vec<NoteSummary>> {
        let repo = self.dna_repo_of(&p.at)?;
        self.dna_ensure_fresh(&repo)?;
        let Some(fts) = index::fts_query(&p.query) else {
            return Ok(Vec::new());
        };
        let key = key_of(&repo);
        let limit = p.limit.unwrap_or(SEARCH_DEFAULT).clamp(1, SEARCH_MAX);
        let rows = self.lock().store.dna_search(&key, &fts, limit)?;
        self.dna_summaries(&key, &rows)
    }

    pub(crate) fn dna_view(&self, repo: &Path, slug: &str) -> Result<NoteView> {
        let key = key_of(repo);
        let st = self.lock();
        let n = st.store.dna_note(&key, slug)?.ok_or_else(|| HubError::NotFound(format!("no note {slug}")))?;
        if let Some(e) = &n.parse_error {
            return Err(HubError::Conflict(format!("note {slug} does not parse: {e}")));
        }
        let reviewed = st.store.dna_is_reviewed(&key, slug, &n.content_hash)?;
        let backlinks = st.store.dna_backlinks(&key, slug)?;
        Ok(NoteView {
            summary: summary(&n, reviewed),
            links: note::links(&n.body),
            backlinks,
            content_hash: n.content_hash.clone(),
            source: n.source.clone(),
            path: fsio::note_path(repo, slug).to_string_lossy().into_owned(),
            body: n.body,
        })
    }

    pub fn dna_get(&self, p: DnaGet) -> Result<NoteView> {
        let repo = self.dna_repo_of(&p.at)?;
        self.dna_ensure_fresh(&repo)?;
        self.dna_view(&repo, &p.slug)
    }

    pub fn dna_graph(&self, at: &DnaRef) -> Result<DnaGraph> {
        let repo = self.dna_repo_of(at)?;
        self.dna_ensure_fresh(&repo)?;
        let key = key_of(&repo);
        let (rows, edges) = self.lock().store.dna_graph(&key)?;
        let nodes = self.dna_summaries(&key, &rows)?;
        let links = edges
            .into_iter()
            .map(|(src, dst)| GraphLink { dangling: !nodes.iter().any(|n| n.slug == dst), src, dst })
            .collect();
        Ok(DnaGraph { nodes, links })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::Path;
    use std::process::Command;

    use mirante_core::dna::check::{cite, rev_parse};
    use mirante_core::dna::note::{render, Kind, Note, Status};
    use mirante_core::model::AgentKind;
    use mirante_ipc::types::{DnaGet, DnaRef, DnaSearch};
    use serde_json::Value;

    use super::*;
    use crate::hub::testkit::Kit;
    use crate::hub::HubError;

    impl Kit {
        pub fn repo_ref(&self) -> DnaRef {
            DnaRef { repo: Some(self.repo.to_string_lossy().into_owned()), task: None }
        }

        /// Every event (repo-level ones too) of this kind, payloads in order.
        pub fn payloads(&self, kind: &str) -> Vec<Value> {
            let st = self.hub.lock();
            st.store
                .events_since(0, 10_000)
                .unwrap()
                .into_iter()
                .filter(|e| e.kind == kind)
                .map(|e| e.payload)
                .collect()
        }
    }

    pub fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    pub fn commit_in(dir: impl AsRef<Path>, path: &str, text: &str) {
        let dir = dir.as_ref();
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
        git(dir, &["add", "-A", "--", path]);
        git(dir, &["commit", "-q", "-m", path]);
    }

    pub fn head_of(dir: impl AsRef<Path>) -> String {
        rev_parse(dir.as_ref(), "HEAD").unwrap()
    }

    pub const CODE: &str = "fn keep() {}\nfn spool() {\n  write();\n}\nfn other() {}\n";

    /// A note's text citing `path` lines `start..=end` at the repo's HEAD.
    pub fn note_text(repo: &Path, title: &str, body: &str, path: &str, start: u32, end: u32) -> String {
        let a = cite(repo, &head_of(repo), path, Some("spool"), start, end).unwrap();
        render(&Note {
            id: "0193b7c2-5f1e-7c3a-9a10-3f2b8d1e4c55".into(),
            title: title.into(),
            kind: Kind::Gotcha,
            status: Status::Active,
            created: "2026-11-07".into(),
            source: "manual".into(),
            anchors: vec![a],
            body: format!("{body}\n"),
        })
    }

    pub fn write_note(repo: &Path, slug: &str, text: &str) {
        std::fs::write(repo.join(format!(".mirante/memory/{slug}.md")), text).unwrap();
    }

    /// Enabled memory with `src/a.rs` committed and two notes on disk: `hook-spool` (links
    /// `[[other]]` and `[[gone]]`) citing a.rs L2-4 and `other` citing a.rs L5.
    pub fn with_two_notes(k: &Kit) {
        commit_in(&k.repo, "src/a.rs", CODE);
        k.hub.dna_enable(&k.repo_ref(), &Caller::Person).unwrap();
        let spool =
            note_text(&k.repo, "Hook spool fallback", "Falls back. See [[other]] and [[gone]].", "src/a.rs", 2, 4);
        write_note(&k.repo, "hook-spool", &spool);
        write_note(&k.repo, "other", &note_text(&k.repo, "Other thing", "Unrelated.", "src/a.rs", 5, 5));
    }

    #[test]
    fn enable_creates_readme_and_is_explicit() {
        let k = Kit::new();
        let st = k.hub.dna_status(&k.repo_ref()).unwrap();
        assert!(!st.enabled && !st.dir && !k.repo.join(".mirante").exists());
        let st = k.hub.dna_enable(&k.repo_ref(), &Caller::Person).unwrap();
        assert!(st.enabled && st.dir && k.repo.join(".mirante/memory/README.md").exists());
        let st = k.hub.dna_disable(&k.repo_ref(), &Caller::Person).unwrap();
        assert!(!st.enabled && k.repo.join(".mirante/memory/README.md").exists());
    }

    #[test]
    fn search_get_and_graph_reflect_files_on_disk() {
        let k = Kit::new();
        with_two_notes(&k);
        git(&k.repo, &["add", "-A"]);
        git(&k.repo, &["commit", "-q", "-m", "memory"]);
        k.hub.dna_check(&k.repo_ref()).unwrap();
        let hits = k.hub.dna_search(DnaSearch { at: k.repo_ref(), query: "spool".into(), limit: None }).unwrap();
        assert_eq!(hits.iter().map(|h| h.slug.as_str()).collect::<Vec<_>>(), ["hook-spool"]);
        assert_eq!((hits[0].state.as_str(), hits[0].reviewed), ("fresh", false));
        assert_eq!(hits[0].anchors[0].symbol.as_deref(), Some("spool"));
        let none = k.hub.dna_search(DnaSearch { at: k.repo_ref(), query: " ** ".into(), limit: None }).unwrap();
        assert!(none.is_empty());
        let n = k.hub.dna_get(DnaGet { at: k.repo_ref(), slug: "hook-spool".into() }).unwrap();
        assert!(n.body.contains("Falls back"));
        assert_eq!(n.links, ["other", "gone"]);
        assert!(n.path.ends_with(".mirante/memory/hook-spool.md"));
        let other = k.hub.dna_get(DnaGet { at: k.repo_ref(), slug: "other".into() }).unwrap();
        assert_eq!(other.backlinks, ["hook-spool"]);
        let missing = k.hub.dna_get(DnaGet { at: k.repo_ref(), slug: "nope".into() });
        assert!(matches!(missing, Err(HubError::NotFound(_))), "{missing:?}");
        let g = k.hub.dna_graph(&k.repo_ref()).unwrap();
        assert_eq!(g.nodes.len(), 2);
        let links: Vec<(&str, &str, bool)> =
            g.links.iter().map(|l| (l.src.as_str(), l.dst.as_str(), l.dangling)).collect();
        assert_eq!(links, [("hook-spool", "gone", true), ("hook-spool", "other", false)]);
    }

    #[test]
    fn get_by_task_resolves_its_repo() {
        let k = Kit::new();
        with_two_notes(&k);
        let t = k.create(AgentKind::Claude, "mem").task;
        let by_task = DnaRef { repo: None, task: Some(t.id.clone()) };
        let n = k.hub.dna_get(DnaGet { at: by_task.clone(), slug: "other".into() }).unwrap();
        assert_eq!(n.summary.title, "Other thing");
        assert!(k.hub.dna_status(&by_task).unwrap().enabled);
    }

    #[test]
    fn check_emits_dna_indexed_with_counts() {
        let k = Kit::new();
        with_two_notes(&k);
        write_note(&k.repo, "broken", "no frontmatter");
        let rep = k.hub.dna_check(&k.repo_ref()).unwrap();
        assert_eq!((rep.notes, rep.invalid.len(), rep.unreviewed), (2, 1, 2));
        let last = k.payloads("dna.indexed").pop().expect("dna.indexed");
        assert_eq!(
            (last["notes"].as_u64(), last["unreviewed"].as_u64(), last["stale"].as_u64()),
            (Some(2), Some(2), Some(0))
        );
        assert_eq!(last["repo"].as_str(), Some(k.repo.canonicalize().unwrap().to_str().unwrap()));
        let st = k.hub.dna_status(&k.repo_ref()).unwrap();
        assert_eq!((st.notes, st.invalid, st.unreviewed, st.pending), (2, 1, 2, 0));
    }

    #[test]
    fn search_refreshes_lazily_when_head_moves() {
        let k = Kit::new();
        with_two_notes(&k);
        let q = || DnaSearch { at: k.repo_ref(), query: "spool".into(), limit: None };
        assert_eq!(k.hub.dna_search(q()).unwrap()[0].state, "fresh");
        commit_in(&k.repo, "src/a.rs", &CODE.replace("write()", "flush()"));
        let hit = &k.hub.dna_search(q()).unwrap()[0];
        assert_eq!(hit.state, "stale");
        assert_eq!(hit.anchors[0].state, "stale");
        // A note written later shows up without an explicit check.
        write_note(&k.repo, "late", &note_text(&k.repo, "Late spool note", "x", "src/a.rs", 1, 1));
        assert_eq!(k.hub.dna_search(q()).unwrap().len(), 2);
    }

    #[test]
    fn worktrees_and_non_repos_are_refused() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "wt").task;
        let at = |p: &str| DnaRef { repo: Some(p.into()), task: None };
        assert!(matches!(k.hub.dna_status(&at(&t.worktree_path)), Err(HubError::Invalid(_))));
        let plain = k.dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(matches!(k.hub.dna_status(&at(plain.to_str().unwrap())), Err(HubError::Invalid(_))));
        assert!(matches!(k.hub.dna_status(&at("/definitely/not/here")), Err(HubError::Invalid(_))));
        assert!(matches!(k.hub.dna_status(&DnaRef::default()), Err(HubError::Invalid(_))));
        // A subdirectory names its repo.
        std::fs::create_dir_all(k.repo.join("sub")).unwrap();
        let st = k.hub.dna_status(&at(k.repo.join("sub").to_str().unwrap())).unwrap();
        assert_eq!(st.repo, k.repo.canonicalize().unwrap().to_string_lossy());
    }
}
