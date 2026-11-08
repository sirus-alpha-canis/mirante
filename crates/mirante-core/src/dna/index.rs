//! The daemon's regenerable index of `.mirante/memory` (notes, anchors, links, FTS5), plus
//! the two tables that are not regenerable: the proposal queue and the trust ledger of
//! content hashes a person accepted or confirmed.
use rusqlite::{params, OptionalExtension, Row, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};

use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use super::anchor::Anchor;
use super::check::{check_all, rev_parse, AnchorState};
use super::{fsio, note, sha256_hex, DnaError, MAX_NOTES, MEMORY_DIR};
use crate::error::{CoreError, Result};
use crate::model::{new_id, now_ms};
use crate::store::Store;

/// Most terms a search uses.
const MAX_QUERY_TERMS: usize = 12;

/// A safe FTS5 `MATCH` expression for free text: alphanumeric runs, lowercased, each a
/// quoted prefix term (`"term"*`), joined with `OR`, at most 12. FTS5 syntax in the input
/// (`NEAR`, `-`, `:`, quotes, `*`) is dropped. `None` when no term is left.
pub fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(MAX_QUERY_TERMS)
        .map(|t| format!("\"{}\"*", t.to_lowercase()))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

/// A `[[link]]` edge: (source slug, target slug).
pub type Link = (String, String);

/// One note as the index stores it. A note whose file does not parse keeps its slug and
/// `parse_error` (empty body, no anchors, not searchable): it is listed, never deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedNote {
    pub slug: String,
    pub id: String,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub body: String,
    pub content_hash: String,
    pub source: String,
    pub parse_error: Option<String>,
    pub anchors: Vec<Anchor>,
    pub links: Vec<String>,
}

/// An indexed anchor and what the last check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedAnchor {
    pub idx: u32,
    pub anchor: Anchor,
    pub state: AnchorState,
}

/// A note read back from the index, with its anchors in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteRow {
    pub slug: String,
    pub id: String,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub body: String,
    pub content_hash: String,
    pub source: String,
    pub parse_error: Option<String>,
    pub anchors: Vec<IndexedAnchor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRow {
    pub enabled: bool,
    pub checked_head: Option<String>,
    pub indexed_at: Option<i64>,
}

/// Counts for `dna.status`. `notes` and `stale` / `unreviewed` count valid notes only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DnaCounts {
    pub notes: usize,
    pub stale: usize,
    pub unreviewed: usize,
    pub pending: usize,
    pub invalid: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProposal {
    pub repo: String,
    pub task_id: Option<String>,
    /// `agent` or `cli`.
    pub origin: String,
    pub title: String,
    pub kind: String,
    pub body: String,
    pub anchors: Vec<Anchor>,
    pub redactions: usize,
}

/// A proposed note waiting for (or past) a person's review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: String,
    pub repo: String,
    pub task_id: Option<String>,
    pub origin: String,
    pub title: String,
    pub kind: String,
    pub body: String,
    pub anchors: Vec<Anchor>,
    pub redactions: usize,
    /// `pending`, `accepted` or `rejected`.
    pub status: String,
    /// The note's file name once accepted.
    pub slug: Option<String>,
    pub decision_note: String,
    pub created_at: i64,
    pub decided_at: Option<i64>,
}

const NOTE_COLUMNS: &str = "slug, id, title, kind, status, body, content_hash, source, parse_error";
const PROPOSAL_COLUMNS: &str = "id, repo, task_id, origin, title, kind, body, anchors, redactions, status, slug, \
     decision_note, created_at, decided_at";

fn note_from_row(r: &Row<'_>) -> rusqlite::Result<NoteRow> {
    Ok(NoteRow {
        slug: r.get(0)?,
        id: r.get(1)?,
        title: r.get(2)?,
        kind: r.get(3)?,
        status: r.get(4)?,
        body: r.get(5)?,
        content_hash: r.get(6)?,
        source: r.get(7)?,
        parse_error: r.get(8)?,
        anchors: Vec::new(),
    })
}

fn proposal_from_row(r: &Row<'_>) -> rusqlite::Result<Proposal> {
    let anchors: String = r.get(7)?;
    Ok(Proposal {
        id: r.get(0)?,
        repo: r.get(1)?,
        task_id: r.get(2)?,
        origin: r.get(3)?,
        title: r.get(4)?,
        kind: r.get(5)?,
        body: r.get(6)?,
        anchors: serde_json::from_str(&anchors)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(e)))?,
        redactions: r.get::<_, i64>(8)? as usize,
        status: r.get(9)?,
        slug: r.get(10)?,
        decision_note: r.get(11)?,
        created_at: r.get(12)?,
        decided_at: r.get(13)?,
    })
}

/// (state, reason, now_start, now_end) columns of an anchor state.
fn state_columns(s: &AnchorState) -> (&'static str, Option<&str>, Option<u32>, Option<u32>) {
    match s {
        AnchorState::Unchecked => ("unchecked", None, None, None),
        AnchorState::Fresh => ("fresh", None, None, None),
        AnchorState::Relocated { start, end } => ("relocated", None, Some(*start), Some(*end)),
        AnchorState::Stale { reason } => ("stale", Some(reason), None, None),
        AnchorState::Unverifiable { reason } => ("unverifiable", Some(reason), None, None),
    }
}

fn state_from(state: &str, reason: Option<String>, now: (Option<u32>, Option<u32>)) -> AnchorState {
    let reason = || reason.clone().unwrap_or_default();
    match (state, now) {
        ("fresh", _) => AnchorState::Fresh,
        ("relocated", (Some(start), Some(end))) => AnchorState::Relocated { start, end },
        ("stale", _) => AnchorState::Stale { reason: reason() },
        ("unverifiable", _) => AnchorState::Unverifiable { reason: reason() },
        _ => AnchorState::Unchecked,
    }
}

fn load_proposal(tx: &rusqlite::Connection, id: &str) -> Result<Proposal> {
    tx.query_row(&format!("SELECT {PROPOSAL_COLUMNS} FROM dna_proposals WHERE id = ?1"), [id], proposal_from_row)
        .optional()?
        .ok_or_else(|| CoreError::NotFound { kind: "proposal", id: id.to_string() })
}

/// Marks a pending proposal decided (the trigger refuses a second decision even if this
/// check is bypassed).
fn decide(tx: &Transaction<'_>, id: &str, accepted: bool, slug: Option<&str>, note: &str) -> Result<Proposal> {
    let p = load_proposal(tx, id)?;
    let to = if accepted { "accepted" } else { "rejected" };
    if p.status != "pending" {
        return Err(CoreError::InvalidTransition { kind: "proposal", from: p.status, to: to.into() });
    }
    tx.execute(
        "UPDATE dna_proposals SET status = ?2, slug = ?3, decision_note = ?4, decided_at = ?5 WHERE id = ?1",
        params![id, to, slug, note, now_ms()],
    )?;
    load_proposal(tx, id)
}

fn record_review(tx: &rusqlite::Connection, repo: &str, slug: &str, hash: &str, how: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO dna_reviews (repo, slug, content_hash, how, reviewed_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (repo, slug, content_hash) DO NOTHING",
        params![repo, slug, hash, how, now_ms()],
    )?;
    Ok(())
}

impl Store {
    pub fn dna_repo(&self, repo: &str) -> Result<Option<RepoRow>> {
        Ok(self
            .conn
            .query_row("SELECT enabled, checked_head, indexed_at FROM dna_repos WHERE repo = ?1", [repo], |r| {
                Ok(RepoRow { enabled: r.get::<_, i64>(0)? == 1, checked_head: r.get(1)?, indexed_at: r.get(2)? })
            })
            .optional()?)
    }

    pub fn dna_set_enabled(&mut self, repo: &str, enabled: bool) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO dna_repos (repo, enabled, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (repo) DO UPDATE SET enabled = ?2, updated_at = ?3",
            params![repo, enabled as i64, now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Replaces the repo's whole index (notes, anchors as `unchecked`, links, FTS rows).
    pub fn dna_replace_index(&mut self, repo: &str, notes: &[IndexedNote]) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for table in ["dna_notes", "dna_anchors", "dna_links", "dna_fts"] {
            tx.execute(&format!("DELETE FROM {table} WHERE repo = ?1"), [repo])?;
        }
        for n in notes {
            tx.execute(
                &format!(
                    "INSERT INTO dna_notes (repo, {NOTE_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
                ),
                params![repo, n.slug, n.id, n.title, n.kind, n.status, n.body, n.content_hash, n.source, n.parse_error],
            )?;
            for (idx, a) in n.anchors.iter().enumerate() {
                tx.execute(
                    "INSERT INTO dna_anchors (repo, slug, idx, path, symbol, line_start, line_end, commit_sha,
                         fingerprint, state) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'unchecked')",
                    params![repo, n.slug, idx as i64, a.path, a.symbol, a.start, a.end, a.commit, a.fingerprint],
                )?;
            }
            for dst in &n.links {
                tx.execute(
                    "INSERT OR IGNORE INTO dna_links (repo, src, dst) VALUES (?1, ?2, ?3)",
                    params![repo, n.slug, dst],
                )?;
            }
            if n.parse_error.is_none() {
                tx.execute(
                    "INSERT INTO dna_fts (repo, slug, title, body) VALUES (?1, ?2, ?3, ?4)",
                    params![repo, n.slug, n.title, n.body],
                )?;
            }
        }
        tx.execute(
            "INSERT INTO dna_repos (repo, enabled, indexed_at, updated_at) VALUES (?1, 0, ?2, ?2)
             ON CONFLICT (repo) DO UPDATE SET indexed_at = ?2, updated_at = ?2",
            params![repo, now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Records each anchor's checked state and the commit the repo was checked at.
    pub fn dna_set_anchor_states(
        &mut self,
        repo: &str,
        head: Option<&str>,
        states: &[(String, u32, AnchorState)],
    ) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (slug, idx, state) in states {
            let (s, reason, now_start, now_end) = state_columns(state);
            tx.execute(
                "UPDATE dna_anchors SET state = ?4, reason = ?5, now_start = ?6, now_end = ?7
                 WHERE repo = ?1 AND slug = ?2 AND idx = ?3",
                params![repo, slug, idx, s, reason, now_start, now_end],
            )?;
        }
        tx.execute(
            "INSERT INTO dna_repos (repo, enabled, checked_head, updated_at) VALUES (?1, 0, ?2, ?3)
             ON CONFLICT (repo) DO UPDATE SET checked_head = ?2, updated_at = ?3",
            params![repo, head, now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn dna_anchors_of(&self, repo: &str, slug: &str) -> Result<Vec<IndexedAnchor>> {
        let mut stmt = self.conn.prepare(
            "SELECT idx, path, symbol, line_start, line_end, commit_sha, fingerprint, state, reason, now_start, now_end
             FROM dna_anchors WHERE repo = ?1 AND slug = ?2 ORDER BY idx",
        )?;
        let rows = stmt.query_map(params![repo, slug], |r| {
            let state: String = r.get(7)?;
            Ok(IndexedAnchor {
                idx: r.get(0)?,
                anchor: Anchor {
                    path: r.get(1)?,
                    symbol: r.get(2)?,
                    start: r.get(3)?,
                    end: r.get(4)?,
                    commit: r.get(5)?,
                    fingerprint: r.get(6)?,
                },
                state: state_from(&state, r.get(8)?, (r.get(9)?, r.get(10)?)),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn with_anchors(&self, repo: &str, mut notes: Vec<NoteRow>) -> Result<Vec<NoteRow>> {
        for n in &mut notes {
            n.anchors = self.dna_anchors_of(repo, &n.slug)?;
        }
        Ok(notes)
    }

    /// Active, valid notes matching the FTS5 expression `fts` (build it with [`fts_query`]),
    /// best first; title hits weigh ten times body hits.
    pub fn dna_search(&self, repo: &str, fts: &str, limit: usize) -> Result<Vec<NoteRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {NOTE_COLUMNS} FROM dna_notes JOIN (
                 SELECT slug AS hit, bm25(dna_fts, 0, 0, 10.0, 1.0) AS rank FROM dna_fts
                 WHERE dna_fts MATCH ?2 AND repo = ?1
             ) ON slug = hit
             WHERE repo = ?1 AND status = 'active' AND parse_error IS NULL
             ORDER BY rank LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![repo, fts, limit as i64], note_from_row)?;
        self.with_anchors(repo, rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn dna_note(&self, repo: &str, slug: &str) -> Result<Option<NoteRow>> {
        let n = self
            .conn
            .query_row(
                &format!("SELECT {NOTE_COLUMNS} FROM dna_notes WHERE repo = ?1 AND slug = ?2"),
                params![repo, slug],
                note_from_row,
            )
            .optional()?;
        Ok(match n {
            Some(n) => self.with_anchors(repo, vec![n])?.pop(),
            None => None,
        })
    }

    /// Every note of the repo (valid or not), by slug.
    pub fn dna_notes(&self, repo: &str) -> Result<Vec<NoteRow>> {
        let mut stmt =
            self.conn.prepare(&format!("SELECT {NOTE_COLUMNS} FROM dna_notes WHERE repo = ?1 ORDER BY slug"))?;
        let rows = stmt.query_map([repo], note_from_row)?;
        self.with_anchors(repo, rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The valid notes (by slug) and every `[[link]]` edge (src, dst), dangling ones too.
    pub fn dna_graph(&self, repo: &str) -> Result<(Vec<NoteRow>, Vec<Link>)> {
        let nodes = self.dna_notes(repo)?.into_iter().filter(|n| n.parse_error.is_none()).collect();
        let mut stmt = self.conn.prepare("SELECT src, dst FROM dna_links WHERE repo = ?1 ORDER BY src, dst")?;
        let links = stmt.query_map([repo], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        Ok((nodes, links))
    }

    /// Slugs of the notes linking to `slug`.
    pub fn dna_backlinks(&self, repo: &str, slug: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT src FROM dna_links WHERE repo = ?1 AND dst = ?2 ORDER BY src")?;
        let rows = stmt.query_map(params![repo, slug], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn dna_status(&self, repo: &str) -> Result<DnaCounts> {
        let count =
            |sql: &str| -> Result<usize> { Ok(self.conn.query_row(sql, [repo], |r| r.get::<_, i64>(0))? as usize) };
        Ok(DnaCounts {
            notes: count("SELECT COUNT(*) FROM dna_notes WHERE repo = ?1 AND parse_error IS NULL")?,
            stale: count(
                "SELECT COUNT(DISTINCT a.slug) FROM dna_anchors a JOIN dna_notes n ON n.repo = a.repo AND n.slug = a.slug
                 WHERE a.repo = ?1 AND a.state = 'stale' AND n.parse_error IS NULL",
            )?,
            unreviewed: count(
                "SELECT COUNT(*) FROM dna_notes n WHERE n.repo = ?1 AND n.parse_error IS NULL AND NOT EXISTS (
                     SELECT 1 FROM dna_reviews r WHERE r.repo = n.repo AND r.slug = n.slug
                     AND r.content_hash = n.content_hash)",
            )?,
            pending: count("SELECT COUNT(*) FROM dna_proposals WHERE repo = ?1 AND status = 'pending'")?,
            invalid: count("SELECT COUNT(*) FROM dna_notes WHERE repo = ?1 AND parse_error IS NOT NULL")?,
        })
    }

    pub fn dna_insert_proposal(&mut self, p: &NewProposal) -> Result<Proposal> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = new_id();
        tx.execute(
            "INSERT INTO dna_proposals (id, repo, task_id, origin, title, kind, body, anchors, redactions, status,
                 created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10)",
            params![
                id,
                p.repo,
                p.task_id,
                p.origin,
                p.title,
                p.kind,
                p.body,
                serde_json::to_string(&p.anchors)?,
                p.redactions as i64,
                now_ms()
            ],
        )?;
        let out = load_proposal(&tx, &id)?;
        tx.commit()?;
        Ok(out)
    }

    pub fn dna_proposal(&self, id: &str) -> Result<Proposal> {
        load_proposal(&self.conn, id)
    }

    /// Proposals, oldest first, optionally of one repo and/or one status.
    pub fn dna_proposals(&self, repo: Option<&str>, status: Option<&str>) -> Result<Vec<Proposal>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {PROPOSAL_COLUMNS} FROM dna_proposals
             WHERE (?1 IS NULL OR repo = ?1) AND (?2 IS NULL OR status = ?2) ORDER BY created_at, id"
        ))?;
        let rows = stmt.query_map(params![repo, status], proposal_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Pending proposals of one task.
    pub fn dna_pending_count(&self, task_id: &str) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM dna_proposals WHERE task_id = ?1 AND status = 'pending'",
            [task_id],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Accepts (with the note's `slug`) or rejects a pending proposal. A decided one is
    /// `InvalidTransition`.
    pub fn dna_decide(&mut self, id: &str, accepted: bool, slug: Option<&str>, note: &str) -> Result<Proposal> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let p = decide(&tx, id, accepted, slug, note)?;
        tx.commit()?;
        Ok(p)
    }

    /// Accepts a pending proposal as the note `slug` and records its exact content as
    /// reviewed, in one transaction.
    pub fn dna_accept(&mut self, id: &str, slug: &str, note: &str, content_hash: &str) -> Result<Proposal> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let p = decide(&tx, id, true, Some(slug), note)?;
        record_review(&tx, &p.repo, slug, content_hash, "accepted")?;
        tx.commit()?;
        Ok(p)
    }

    /// Records that a person accepted or confirmed this exact content (`how`: `accepted` or
    /// `confirmed`). Idempotent.
    pub fn dna_record_review(&mut self, repo: &str, slug: &str, content_hash: &str, how: &str) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        record_review(&tx, repo, slug, content_hash, how)?;
        tx.commit()?;
        Ok(())
    }

    pub fn dna_is_reviewed(&self, repo: &str, slug: &str, content_hash: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM dna_reviews WHERE repo = ?1 AND slug = ?2 AND content_hash = ?3",
                params![repo, slug, content_hash],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Slugs of the valid, active notes with an anchor in `paths`, most matching anchors
    /// first.
    pub fn dna_notes_citing(&self, repo: &str, paths: &[String]) -> Result<Vec<String>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let json = serde_json::to_string(paths)?;
        let mut stmt = self.conn.prepare(
            "SELECT a.slug FROM dna_anchors a JOIN dna_notes n ON n.repo = a.repo AND n.slug = a.slug
             WHERE a.repo = ?1 AND a.path IN (SELECT value FROM json_each(?2))
               AND n.status = 'active' AND n.parse_error IS NULL
             GROUP BY a.slug ORDER BY COUNT(*) DESC, a.slug",
        )?;
        let rows = stmt.query_map(params![repo, json], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// A file in `.mirante/memory` that is not in the index, or is indexed without content
/// because it does not parse. Never deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvalidNote {
    pub file: String,
    pub error: String,
}

/// What reading `.mirante/memory` found (no git, no store).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scan {
    pub notes: Vec<IndexedNote>,
    pub invalid: Vec<InvalidNote>,
    /// The directory listing's stat signature when the scan started ([`signature`]).
    pub signature: String,
}

/// What a refresh indexed and found at `head`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshReport {
    pub repo: String,
    pub head: Option<String>,
    /// Valid notes indexed.
    pub notes: usize,
    pub invalid: Vec<InvalidNote>,
    /// Notes with at least one anchor in that state.
    pub stale: usize,
    pub relocated: usize,
    pub unverifiable: usize,
    pub unreviewed: usize,
    #[serde(skip)]
    pub signature: String,
}

/// The cheap facts that tell whether the index is behind ([`needs_refresh`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub head: Option<String>,
    pub signature: String,
}

/// The index key of a repo: its canonical path.
pub fn repo_key(repo: &Path) -> std::result::Result<String, DnaError> {
    Ok(repo.canonicalize()?.to_string_lossy().into_owned())
}

/// `Ok(None)` when `.mirante/memory` does not exist at all; the checked directory when it
/// does; `Err(Unsafe)` when either level is a symlink or not a directory.
fn existing_memory_dir(repo: &Path) -> std::result::Result<Option<std::path::PathBuf>, DnaError> {
    let mut p = repo.to_path_buf();
    for seg in MEMORY_DIR.split('/') {
        p.push(seg);
        match std::fs::symlink_metadata(&p) {
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
            Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
                return Err(DnaError::Unsafe(format!("{}: is a symlink or not a directory", p.display())))
            }
            Ok(_) => {}
        }
    }
    fsio::memory_dir(repo).map(Some)
}

/// A stat signature of `.mirante/memory`: every entry's name, type, size and mtime,
/// hashed. `"absent"` when the directory does not exist.
pub fn signature(repo: &Path) -> std::result::Result<String, DnaError> {
    let Some(dir) = existing_memory_dir(repo)? else {
        return Ok("absent".into());
    };
    let mut lines = Vec::new();
    for (i, entry) in std::fs::read_dir(&dir)?.enumerate() {
        if i > MAX_NOTES {
            break;
        }
        let entry = entry?;
        let m = entry.metadata()?; // does not follow a symlink
        lines.push(format!(
            "{}\0{:o}\0{}\0{}.{}",
            entry.file_name().to_string_lossy(),
            m.mode(),
            m.len(),
            m.mtime(),
            m.mtime_nsec()
        ));
    }
    lines.sort();
    Ok(sha256_hex(lines.join("\n").as_bytes()))
}

/// Reads `.mirante/memory`: at most [`MAX_NOTES`] entries; regular `*.md` files only
/// (symlinks, directories, dotfiles, other extensions and `README.md` are skipped); each
/// read without following symlinks and size-capped. A file whose name is not a slug is
/// listed as invalid; one that does not parse is indexed with its `parse_error`.
pub fn scan(repo: &Path) -> std::result::Result<Scan, DnaError> {
    let signature = signature(repo)?;
    let Some(dir) = existing_memory_dir(repo)? else {
        return Ok(Scan { notes: Vec::new(), invalid: Vec::new(), signature });
    };
    let (mut notes, mut invalid) = (Vec::new(), Vec::new());
    for (i, entry) in std::fs::read_dir(&dir)?.enumerate() {
        if i >= MAX_NOTES {
            invalid.push(InvalidNote {
                file: String::new(),
                error: format!("more than {MAX_NOTES} entries; rest skipped"),
            });
            break;
        }
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".md").map(str::to_string) else { continue };
        if name.starts_with('.') || name.eq_ignore_ascii_case("README.md") || !entry.file_type()?.is_file() {
            continue;
        }
        if !note::is_slug(&stem) {
            invalid.push(InvalidNote { file: name, error: "the file name is not a note name (a-z, 0-9 and -)".into() });
            continue;
        }
        let text = match fsio::read_note(repo, &stem) {
            Ok(t) => t,
            Err(e) => {
                invalid.push(InvalidNote { file: name, error: e.to_string() });
                continue;
            }
        };
        let content_hash = sha256_hex(text.as_bytes());
        match note::parse(&text) {
            Ok(n) => notes.push(IndexedNote {
                slug: stem.clone(),
                id: n.id,
                title: n.title,
                kind: n.kind.as_str().to_string(),
                status: n.status.as_str().to_string(),
                links: note::links(&n.body),
                body: n.body,
                content_hash,
                source: n.source,
                parse_error: None,
                anchors: n.anchors,
            }),
            Err(e) => {
                invalid.push(InvalidNote { file: name, error: e.to_string() });
                notes.push(IndexedNote {
                    slug: stem.clone(),
                    id: String::new(),
                    title: stem.clone(),
                    kind: String::new(),
                    status: "active".into(),
                    body: String::new(),
                    content_hash,
                    source: String::new(),
                    parse_error: Some(e.to_string()),
                    anchors: Vec::new(),
                    links: Vec::new(),
                });
            }
        }
    }
    notes.sort_by(|a, b| a.slug.cmp(&b.slug));
    invalid.sort_by(|a, b| a.file.cmp(&b.file));
    Ok(Scan { notes, invalid, signature })
}

/// The HEAD a scan was checked at and each anchor's state (slug, idx, state).
pub type Checks = (Option<String>, Vec<(String, u32, AnchorState)>);

/// Anchor states of a scan's notes at the repo's HEAD. git only; no store. Without a HEAD
/// commit every anchor stays `Unchecked`.
pub fn check_scan(repo: &Path, scan: &Scan) -> std::result::Result<Checks, DnaError> {
    let keys: Vec<(String, u32, Anchor)> = scan
        .notes
        .iter()
        .flat_map(|n| n.anchors.iter().enumerate().map(|(i, a)| (n.slug.clone(), i as u32, a.clone())))
        .collect();
    let Ok(head) = rev_parse(repo, "HEAD") else {
        return Ok((None, keys.into_iter().map(|(s, i, _)| (s, i, AnchorState::Unchecked)).collect()));
    };
    let anchors: Vec<Anchor> = keys.iter().map(|(_, _, a)| a.clone()).collect();
    let checked = check_all(repo, &head, &anchors)?;
    Ok((Some(head), keys.into_iter().zip(checked).map(|((s, i, _), c)| (s, i, c.state)).collect()))
}

/// Writes a scan and its checks to the index and reports the counts.
pub fn apply(store: &mut Store, key: &str, scan: &Scan, checks: &Checks) -> Result<RefreshReport> {
    let (head, states) = checks;
    store.dna_replace_index(key, &scan.notes)?;
    store.dna_set_anchor_states(key, head.as_deref(), states)?;
    let notes_with = |want: fn(&AnchorState) -> bool| {
        let mut slugs: Vec<&str> = states.iter().filter(|(_, _, s)| want(s)).map(|(s, _, _)| s.as_str()).collect();
        slugs.dedup();
        slugs.len()
    };
    Ok(RefreshReport {
        repo: key.to_string(),
        head: head.clone(),
        notes: scan.notes.iter().filter(|n| n.parse_error.is_none()).count(),
        invalid: scan.invalid.clone(),
        stale: notes_with(|s| matches!(s, AnchorState::Stale { .. })),
        relocated: notes_with(|s| matches!(s, AnchorState::Relocated { .. })),
        unverifiable: notes_with(|s| matches!(s, AnchorState::Unverifiable { .. })),
        unreviewed: store.dna_status(key)?.unreviewed,
        signature: scan.signature.clone(),
    })
}

fn dna(e: DnaError) -> CoreError {
    CoreError::Io(std::io::Error::other(e.to_string()))
}

/// Re-reads `.mirante/memory` into the index and checks every anchor at HEAD. Notes that
/// do not parse are listed, never deleted.
pub fn refresh(store: &mut Store, repo: &Path) -> Result<RefreshReport> {
    let key = repo_key(repo).map_err(dna)?;
    let scan = scan(repo).map_err(dna)?;
    let checks = check_scan(repo, &scan).map_err(dna)?;
    apply(store, &key, &scan, &checks)
}

/// The repo's HEAD and memory-directory signature, for [`needs_refresh`].
pub fn probe(repo: &Path) -> std::result::Result<Probe, DnaError> {
    Ok(Probe { head: rev_parse(repo, "HEAD").ok(), signature: signature(repo)? })
}

/// True when the index is behind: never indexed, HEAD moved since the last check, or the
/// directory listing changed since `known_signature` (kept in memory by the caller).
pub fn needs_refresh(store: &Store, key: &str, probe: &Probe, known_signature: Option<&str>) -> Result<bool> {
    let Some(row) = store.dna_repo(key)? else {
        return Ok(true);
    };
    Ok(row.indexed_at.is_none() || row.checked_head != probe.head || known_signature != Some(probe.signature.as_str()))
}

#[cfg(test)]
mod fts_tests {
    use super::*;

    #[test]
    fn fts_query_quotes_terms_and_drops_syntax() {
        assert_eq!(fts_query("hook spool").as_deref(), Some("\"hook\"* OR \"spool\"*"));
        assert_eq!(
            fts_query("NEAR(a b) OR \"x\" -y title:z").as_deref(),
            Some("\"near\"* OR \"a\"* OR \"b\"* OR \"or\"* OR \"x\"* OR \"y\"* OR \"title\"* OR \"z\"*")
        );
        assert_eq!(fts_query("  ** ").as_deref(), None);
        assert_eq!(fts_query(&"w ".repeat(50)).unwrap().matches(" OR ").count(), 11);
    }
}

#[cfg(test)]
mod store_tests {
    use super::*;
    use crate::dna::anchor::Anchor;
    use crate::dna::check::AnchorState;
    use crate::store::Store;

    const SHA: &str = "50ea8cc255941e739b13cdbe2c937abca0319834";

    fn anchor(path: &str) -> Anchor {
        Anchor::parse(&format!("{path}:L1-3@{SHA}~0123456789abcdef")).unwrap()
    }

    fn row(slug: &str, title: &str, body: &str) -> IndexedNote {
        IndexedNote {
            slug: slug.into(),
            id: format!("id-{slug}"),
            title: title.into(),
            kind: "fact".into(),
            status: "active".into(),
            body: body.into(),
            content_hash: format!("hash-{slug}"),
            source: "manual".into(),
            parse_error: None,
            anchors: vec![anchor(&format!("src/{slug}.rs"))],
            links: vec![],
        }
    }

    fn new_proposal(repo: &str, task: Option<&str>) -> NewProposal {
        NewProposal {
            repo: repo.into(),
            task_id: task.map(str::to_string),
            origin: "cli".into(),
            title: "T".into(),
            kind: "fact".into(),
            body: "B".into(),
            anchors: vec![anchor("src/a.rs")],
            redactions: 0,
        }
    }

    #[test]
    fn migration_5_creates_fts_and_the_decided_once_trigger() {
        let mut s = Store::open_in_memory().unwrap();
        assert_eq!(s.schema_version().unwrap(), 5);
        let p = s.dna_insert_proposal(&new_proposal("r", None)).unwrap();
        assert_eq!((p.status.as_str(), p.anchors.len()), ("pending", 1));
        let d = s.dna_decide(&p.id, true, Some("slug-a"), "").unwrap();
        assert_eq!((d.status.as_str(), d.slug.as_deref()), ("accepted", Some("slug-a")));
        assert!(s.dna_decide(&p.id, false, None, "").is_err());
        // The trigger is the backstop, whatever the Rust side checks.
        assert!(s.conn.execute("UPDATE dna_proposals SET status = 'rejected'", []).is_err());
        assert!(s.dna_decide("nope", true, Some("x"), "").is_err());
    }

    #[test]
    fn replace_repo_index_then_search_ranks_title_hits_first() {
        let mut s = Store::open_in_memory().unwrap();
        s.dna_replace_index(
            "r",
            &[row("hook-spool", "Hook spool fallback", "socket down"), row("other", "Other", "the hook is mentioned")],
        )
        .unwrap();
        let hits = s.dna_search("r", "\"hook\"*", 10).unwrap();
        assert_eq!(hits.iter().map(|h| h.slug.as_str()).collect::<Vec<_>>(), ["hook-spool", "other"]);
        assert!(s.dna_search("other-repo", "\"hook\"*", 10).unwrap().is_empty());
        // Replacing drops what is gone.
        s.dna_replace_index("r", &[row("other", "Other", "the hook is mentioned")]).unwrap();
        assert_eq!(s.dna_search("r", "\"hook\"*", 10).unwrap().len(), 1);
        assert!(s.dna_note("r", "hook-spool").unwrap().is_none());
    }

    #[test]
    fn reviews_ledger_marks_exact_contents_only() {
        let mut s = Store::open_in_memory().unwrap();
        s.dna_record_review("r", "a", "h1", "accepted").unwrap();
        s.dna_record_review("r", "a", "h1", "confirmed").unwrap(); // idempotent
        assert!(s.dna_is_reviewed("r", "a", "h1").unwrap());
        assert!(!s.dna_is_reviewed("r", "a", "h2").unwrap());
        assert!(!s.dna_is_reviewed("r", "b", "h1").unwrap());
        assert!(s.dna_record_review("r", "a", "h3", "planted").is_err());
    }

    #[test]
    fn anchors_states_update_and_count_stale() {
        let mut s = Store::open_in_memory().unwrap();
        let mut bad = row("bad", "bad", "");
        bad.parse_error = Some("no frontmatter".into());
        bad.anchors.clear();
        s.dna_replace_index("r", &[row("a", "A", "x"), row("b", "B", "y"), bad]).unwrap();
        s.dna_record_review("r", "a", "hash-a", "accepted").unwrap();
        let st = s.dna_status("r").unwrap();
        assert_eq!((st.notes, st.stale, st.unreviewed, st.invalid, st.pending), (2, 0, 1, 1, 0));
        let states = [
            ("a".to_string(), 0, AnchorState::Relocated { start: 4, end: 6 }),
            ("b".to_string(), 0, AnchorState::Stale { reason: "changed".into() }),
        ];
        s.dna_set_anchor_states("r", Some(SHA), &states).unwrap();
        assert_eq!(s.dna_status("r").unwrap().stale, 1);
        assert_eq!(s.dna_repo("r").unwrap().unwrap().checked_head.as_deref(), Some(SHA));
        let a = s.dna_note("r", "a").unwrap().unwrap();
        assert_eq!(a.anchors[0].state, AnchorState::Relocated { start: 4, end: 6 });
        assert_eq!(
            s.dna_note("r", "b").unwrap().unwrap().anchors[0].state,
            AnchorState::Stale { reason: "changed".into() }
        );
        assert_eq!(s.dna_notes_citing("r", &["src/b.rs".to_string()]).unwrap(), ["b"]);
    }

    #[test]
    fn repos_enable_and_proposals_filter() {
        let mut s = Store::open_in_memory().unwrap();
        assert!(s.dna_repo("r").unwrap().is_none());
        s.dna_set_enabled("r", true).unwrap();
        assert!(s.dna_repo("r").unwrap().unwrap().enabled);
        s.dna_set_enabled("r", false).unwrap();
        assert!(!s.dna_repo("r").unwrap().unwrap().enabled);
        let a = s.dna_insert_proposal(&new_proposal("r", None)).unwrap();
        s.dna_insert_proposal(&new_proposal("r2", None)).unwrap();
        s.dna_decide(&a.id, false, None, "no").unwrap();
        assert_eq!(s.dna_proposals(Some("r"), None).unwrap().len(), 1);
        assert_eq!(s.dna_proposals(None, Some("pending")).unwrap().len(), 1);
        assert_eq!(s.dna_status("r2").unwrap().pending, 1);
        assert_eq!(s.dna_proposal(&a.id).unwrap().decision_note, "no");
    }

    #[test]
    fn graph_lists_valid_notes_and_links() {
        let mut s = Store::open_in_memory().unwrap();
        let mut a = row("a", "A", "see [[b]] and [[gone]]");
        a.links = vec!["b".into(), "gone".into()];
        s.dna_replace_index("r", &[a, row("b", "B", "")]).unwrap();
        let (nodes, links) = s.dna_graph("r").unwrap();
        assert_eq!(nodes.iter().map(|n| n.slug.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(links, [("a".to_string(), "b".to_string()), ("a".to_string(), "gone".to_string())]);
        assert_eq!(s.dna_backlinks("r", "b").unwrap(), ["a"]);
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;
    use crate::dna::check::{cite, rev_parse};
    use crate::dna::note::{render, Kind, Note, Status};
    use crate::dna::{fsio, sha256_hex};
    use crate::test_support::TempDir;
    use std::path::Path;
    use std::process::Command;

    fn plain(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn commit_file(r: &Path, path: &str, text: &str) {
        let full = r.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
        plain(r, &["add", "-A", "--", path]);
        plain(r, &["commit", "-q", "-m", path]);
    }

    const CODE: &str = "fn keep() {}\nfn spool() {\n  write();\n}\nfn other() {}\n";

    /// A note citing `path` lines `start..=end` at HEAD.
    fn note_text(r: &Path, title: &str, path: &str, start: u32, end: u32) -> String {
        let a = cite(r, &rev_parse(r, "HEAD").unwrap(), path, None, start, end).unwrap();
        render(&Note {
            id: "0193b7c2-5f1e-7c3a-9a10-3f2b8d1e4c55".into(),
            title: title.into(),
            kind: Kind::Fact,
            status: Status::Active,
            created: "2026-11-07".into(),
            source: "manual".into(),
            anchors: vec![a],
            body: format!("{title} body, see [[other]].\n"),
        })
    }

    fn repo_with_memory(notes: &[(&str, &str)]) -> TempDir {
        let r = TempDir::new("dna-index");
        plain(r.path(), &["init", "-q", "-b", "main"]);
        commit_file(r.path(), "src/a.rs", CODE);
        fsio::init(r.path()).unwrap();
        for (slug, text) in notes {
            let text = if *text == "GOOD" { note_text(r.path(), slug, "src/a.rs", 2, 4) } else { text.to_string() };
            std::fs::write(r.path().join(format!(".mirante/memory/{slug}.md")), text).unwrap();
        }
        r
    }

    #[test]
    fn refresh_indexes_valid_notes_and_reports_bad_ones() {
        let r = repo_with_memory(&[("good", "GOOD"), ("bad", "no frontmatter")]);
        let mut s = Store::open_in_memory().unwrap();
        let rep = refresh(&mut s, r.path()).unwrap();
        assert_eq!((rep.notes, rep.invalid.len()), (1, 1));
        assert_eq!(rep.invalid[0].file, "bad.md");
        assert!(r.path().join(".mirante/memory/bad.md").exists());
        let key = repo_key(r.path()).unwrap();
        let good = s.dna_note(&key, "good").unwrap().unwrap();
        assert_eq!((good.anchors.len(), good.anchors[0].state.clone()), (1, AnchorState::Fresh));
        assert!(s.dna_note(&key, "bad").unwrap().unwrap().parse_error.is_some());
        assert_eq!(s.dna_search(&key, "\"spool\"* OR \"good\"*", 10).unwrap().len(), 1);
        assert_eq!(s.dna_status(&key).unwrap().invalid, 1);
        assert_eq!(rep.head, Some(rev_parse(r.path(), "HEAD").unwrap()));
    }

    #[test]
    fn refresh_skips_symlinks_dirs_and_non_md() {
        let r = repo_with_memory(&[("good", "GOOD")]);
        let dir = r.path().join(".mirante/memory");
        std::os::unix::fs::symlink("/etc/hosts", dir.join("x.md")).unwrap();
        std::fs::create_dir(dir.join("y.md")).unwrap();
        std::fs::write(dir.join("z.txt"), "text").unwrap();
        std::fs::write(dir.join("Not A Slug.md"), "x").unwrap();
        let mut s = Store::open_in_memory().unwrap();
        let rep = refresh(&mut s, r.path()).unwrap();
        assert_eq!(rep.notes, 1);
        assert_eq!(rep.invalid.iter().map(|i| i.file.as_str()).collect::<Vec<_>>(), ["Not A Slug.md"]);
        let key = repo_key(r.path()).unwrap();
        assert!(s.dna_note(&key, "x").unwrap().is_none() && s.dna_note(&key, "readme").unwrap().is_none());
    }

    #[test]
    fn refresh_of_a_repo_without_memory_is_empty_but_a_symlinked_dir_is_refused() {
        let r = TempDir::new("dna-none");
        plain(r.path(), &["init", "-q", "-b", "main"]);
        commit_file(r.path(), "src/a.rs", CODE);
        let mut s = Store::open_in_memory().unwrap();
        assert_eq!(refresh(&mut s, r.path()).unwrap().notes, 0);
        let elsewhere = TempDir::new("dna-elsewhere");
        std::os::unix::fs::symlink(elsewhere.path(), r.path().join(".mirante")).unwrap();
        assert!(refresh(&mut s, r.path()).is_err());
    }

    #[test]
    fn refresh_then_check_marks_stale_and_relocated() {
        let r = TempDir::new("dna-stale");
        plain(r.path(), &["init", "-q", "-b", "main"]);
        commit_file(r.path(), "src/a.rs", CODE);
        commit_file(r.path(), "src/b.rs", CODE);
        fsio::init(r.path()).unwrap();
        let dir = r.path().join(".mirante/memory");
        std::fs::write(dir.join("a.md"), note_text(r.path(), "a", "src/a.rs", 2, 4)).unwrap();
        std::fs::write(dir.join("b.md"), note_text(r.path(), "b", "src/b.rs", 2, 4)).unwrap();
        commit_file(r.path(), "src/a.rs", &format!("// moved\n{CODE}"));
        commit_file(r.path(), "src/b.rs", &CODE.replace("write()", "flush()"));
        let mut s = Store::open_in_memory().unwrap();
        let rep = refresh(&mut s, r.path()).unwrap();
        assert_eq!((rep.stale, rep.relocated, rep.unverifiable), (1, 1, 0));
        let key = repo_key(r.path()).unwrap();
        assert_eq!(s.dna_status(&key).unwrap().stale, 1);
        assert_eq!(
            s.dna_note(&key, "a").unwrap().unwrap().anchors[0].state,
            AnchorState::Relocated { start: 3, end: 5 }
        );
    }

    #[test]
    fn unreviewed_unless_hash_in_ledger() {
        let r = repo_with_memory(&[("good", "GOOD")]);
        let mut s = Store::open_in_memory().unwrap();
        assert_eq!(refresh(&mut s, r.path()).unwrap().unreviewed, 1);
        let key = repo_key(r.path()).unwrap();
        let text = std::fs::read_to_string(r.path().join(".mirante/memory/good.md")).unwrap();
        s.dna_record_review(&key, "good", &sha256_hex(text.as_bytes()), "accepted").unwrap();
        assert_eq!(refresh(&mut s, r.path()).unwrap().unreviewed, 0);
        // A hand edit is new content: unreviewed again.
        std::fs::write(r.path().join(".mirante/memory/good.md"), text.replace("body", "edited")).unwrap();
        assert_eq!(refresh(&mut s, r.path()).unwrap().unreviewed, 1);
    }

    #[test]
    fn needs_refresh_when_head_or_the_listing_changes() {
        let r = repo_with_memory(&[("good", "GOOD")]);
        let mut s = Store::open_in_memory().unwrap();
        let key = repo_key(r.path()).unwrap();
        assert!(needs_refresh(&s, &key, &probe(r.path()).unwrap(), None).unwrap());
        let rep = refresh(&mut s, r.path()).unwrap();
        let sig = rep.signature.clone();
        assert!(!needs_refresh(&s, &key, &probe(r.path()).unwrap(), Some(&sig)).unwrap());
        std::fs::write(r.path().join(".mirante/memory/new.md"), "x").unwrap();
        assert!(needs_refresh(&s, &key, &probe(r.path()).unwrap(), Some(&sig)).unwrap());
        let sig = refresh(&mut s, r.path()).unwrap().signature;
        commit_file(r.path(), "src/c.rs", "c\n");
        assert!(needs_refresh(&s, &key, &probe(r.path()).unwrap(), Some(&sig)).unwrap());
    }
}
