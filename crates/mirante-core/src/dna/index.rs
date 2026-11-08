//! The daemon's regenerable index of `.mirante/memory` (notes, anchors, links, FTS5), plus
//! the two tables that are not regenerable: the proposal queue and the trust ledger of
//! content hashes a person accepted or confirmed.
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::json;

use std::path::Path;

use super::anchor::Anchor;
use super::check::{check_since, memory_files_at, rev_parse, AnchorState, Previous};
use super::fsio::MemoryDir;
use super::{note, sha256_hex, DnaError, MAX_NOTES};
use crate::error::{CoreError, Result};
use crate::model::{new_id, now_ms};
use crate::store::{insert_event, Store};

/// Most terms a search uses, and most chars per term.
const MAX_QUERY_TERMS: usize = 12;
const MAX_TERM_CHARS: usize = 64;

/// A safe FTS5 `MATCH` expression for free text: alphanumeric runs, lowercased, each a
/// quoted prefix term (`"term"*`, cut to 64 chars), joined with `OR`, at most 12. FTS5
/// syntax in the input (`NEAR`, `-`, `:`, quotes, `*`) is dropped. `None` when no term is
/// left.
pub fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(MAX_QUERY_TERMS)
        .map(|t| format!("\"{}\"*", t.to_lowercase().chars().take(MAX_TERM_CHARS).collect::<String>()))
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

/// Most pending proposals one task, and one repo, may have at once
/// ([`Store::dna_insert_proposal`] counts them inside its own transaction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProposalCaps {
    pub per_task: usize,
    pub per_repo: usize,
}

/// Which proposals [`Store::dna_proposals`] returns: oldest first, filtered, at most
/// `limit`, starting after the proposal `after` (a cursor: the last id of the previous page).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProposalQuery {
    pub repo: Option<String>,
    pub task_id: Option<String>,
    /// `pending`, `accepted` or `rejected`; every status when `None`.
    pub status: Option<String>,
    pub id: Option<String>,
    pub after: Option<String>,
    pub limit: usize,
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
/// check is bypassed) and appends `dna.decided` to the audit trail, in the caller's
/// transaction.
fn decide(
    tx: &Transaction<'_>,
    boot_id: &str,
    id: &str,
    accepted: bool,
    slug: Option<&str>,
    note: &str,
) -> Result<Proposal> {
    let p = load_proposal(tx, id)?;
    let to = if accepted { "accepted" } else { "rejected" };
    if p.status != "pending" {
        return Err(CoreError::InvalidTransition { kind: "proposal", from: p.status, to: to.into() });
    }
    tx.execute(
        "UPDATE dna_proposals SET status = ?2, slug = ?3, decision_note = ?4, decided_at = ?5 WHERE id = ?1",
        params![id, to, slug, note, now_ms()],
    )?;
    let p = load_proposal(tx, id)?;
    let payload = json!({ "id": p.id, "accepted": accepted, "slug": p.slug, "repo": p.repo });
    insert_event(tx, boot_id, p.task_id.as_deref(), "dna.decided", &payload)?;
    Ok(p)
}

/// Records `hash` as the slug's current reviewed content; every other review of the slug
/// is superseded (kept as history, no longer counted).
fn record_review(tx: &Connection, repo: &str, slug: &str, hash: &str, how: &str) -> Result<()> {
    let now = now_ms();
    tx.execute(
        "UPDATE dna_reviews SET superseded_at = ?4
         WHERE repo = ?1 AND slug = ?2 AND content_hash != ?3 AND superseded_at IS NULL",
        params![repo, slug, hash, now],
    )?;
    tx.execute(
        "INSERT INTO dna_reviews (repo, slug, content_hash, how, reviewed_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (repo, slug, content_hash) DO UPDATE SET superseded_at = NULL",
        params![repo, slug, hash, how, now],
    )?;
    Ok(())
}

fn count(tx: &Connection, sql: &str, arg: &str) -> Result<usize> {
    Ok(tx.query_row(sql, [arg], |r| r.get::<_, i64>(0))? as usize)
}

fn replace_index(tx: &Connection, repo: &str, notes: &[IndexedNote]) -> Result<()> {
    for table in ["dna_notes", "dna_anchors", "dna_links", "dna_fts"] {
        tx.execute(&format!("DELETE FROM {table} WHERE repo = ?1"), [repo])?;
    }
    for n in notes {
        tx.execute(
            &format!("INSERT INTO dna_notes (repo, {NOTE_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"),
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
    Ok(())
}

fn set_anchor_states(
    tx: &Connection,
    repo: &str,
    head: Option<&str>,
    states: &[(String, u32, AnchorState)],
) -> Result<()> {
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
        replace_index(&tx, repo, notes)?;
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
        set_anchor_states(&tx, repo, head, states)?;
        tx.commit()?;
        Ok(())
    }

    /// [`Store::dna_replace_index`] then [`Store::dna_set_anchor_states`] in one transaction:
    /// a reader never sees the new notes with every anchor `unchecked`.
    pub fn dna_apply_index(
        &mut self,
        repo: &str,
        notes: &[IndexedNote],
        head: Option<&str>,
        states: &[(String, u32, AnchorState)],
    ) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        replace_index(&tx, repo, notes)?;
        set_anchor_states(&tx, repo, head, states)?;
        tx.commit()?;
        Ok(())
    }

    /// The anchor states of the last check and the HEAD they were found at, for
    /// [`check_scan`] to reuse; `None` before any check at a commit.
    pub fn dna_previous(&self, repo: &str) -> Result<Option<Previous>> {
        let Some(head) = self.dna_repo(repo)?.and_then(|r| r.checked_head) else {
            return Ok(None);
        };
        let mut stmt = self.conn.prepare(
            "SELECT path, symbol, line_start, line_end, commit_sha, fingerprint, state, reason, now_start, now_end
             FROM dna_anchors WHERE repo = ?1",
        )?;
        let rows = stmt.query_map([repo], |r| {
            let state: String = r.get(6)?;
            let anchor = Anchor {
                path: r.get(0)?,
                symbol: r.get(1)?,
                start: r.get(2)?,
                end: r.get(3)?,
                commit: r.get(4)?,
                fingerprint: r.get(5)?,
            };
            Ok((anchor, state_from(&state, r.get(7)?, (r.get(8)?, r.get(9)?))))
        })?;
        let states = rows.collect::<rusqlite::Result<_>>()?;
        Ok(Some(Previous { head, states }))
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
                     AND r.content_hash = n.content_hash AND r.superseded_at IS NULL)",
            )?,
            pending: count("SELECT COUNT(*) FROM dna_proposals WHERE repo = ?1 AND status = 'pending'")?,
            invalid: count("SELECT COUNT(*) FROM dna_notes WHERE repo = ?1 AND parse_error IS NOT NULL")?,
        })
    }

    /// Queues a proposal and appends `dna.proposed`, in one transaction that also counts the
    /// pending proposals against `caps` (so two concurrent proposals cannot both pass).
    pub fn dna_insert_proposal(&mut self, p: &NewProposal, caps: ProposalCaps) -> Result<Proposal> {
        let boot = self.boot_id().to_string();
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(task) = &p.task_id {
            let n = count(&tx, "SELECT COUNT(*) FROM dna_proposals WHERE task_id = ?1 AND status = 'pending'", task)?;
            if n >= caps.per_task {
                let max = caps.per_task;
                return Err(CoreError::Limit(format!("this task already has {max} proposals waiting for review")));
            }
        }
        let n = count(&tx, "SELECT COUNT(*) FROM dna_proposals WHERE repo = ?1 AND status = 'pending'", &p.repo)?;
        if n >= caps.per_repo {
            let max = caps.per_repo;
            return Err(CoreError::Limit(format!("this repo already has {max} proposals waiting for review")));
        }
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
        insert_event(
            &tx,
            &boot,
            p.task_id.as_deref(),
            "dna.proposed",
            &json!({ "id": out.id, "title": out.title, "repo": out.repo }),
        )?;
        tx.commit()?;
        Ok(out)
    }

    pub fn dna_proposal(&self, id: &str) -> Result<Proposal> {
        load_proposal(&self.conn, id)
    }

    /// Proposals, oldest first (see [`ProposalQuery`]). An unknown `after` is `NotFound`.
    pub fn dna_proposals(&self, q: &ProposalQuery) -> Result<Vec<Proposal>> {
        let cursor: Option<(i64, String)> = match &q.after {
            Some(id) => Some(
                self.conn
                    .query_row("SELECT created_at, id FROM dna_proposals WHERE id = ?1", [id], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })
                    .optional()?
                    .ok_or_else(|| CoreError::NotFound { kind: "proposal", id: id.clone() })?,
            ),
            None => None,
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {PROPOSAL_COLUMNS} FROM dna_proposals
             WHERE (?1 IS NULL OR repo = ?1) AND (?2 IS NULL OR task_id = ?2) AND (?3 IS NULL OR status = ?3)
               AND (?4 IS NULL OR id = ?4)
               AND (?5 IS NULL OR created_at > ?5 OR (created_at = ?5 AND id > ?6))
             ORDER BY created_at, id LIMIT ?7"
        ))?;
        let rows = stmt.query_map(
            params![
                q.repo,
                q.task_id,
                q.status,
                q.id,
                cursor.as_ref().map(|c| c.0),
                cursor.as_ref().map(|c| c.1.as_str()),
                q.limit as i64
            ],
            proposal_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Accepts (with the note's `slug`) or rejects a pending proposal. A decided one is
    /// `InvalidTransition`.
    pub fn dna_decide(&mut self, id: &str, accepted: bool, slug: Option<&str>, note: &str) -> Result<Proposal> {
        let boot = self.boot_id().to_string();
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let p = decide(&tx, &boot, id, accepted, slug, note)?;
        tx.commit()?;
        Ok(p)
    }

    /// Accepts a pending proposal as the note `slug` and records its exact content as
    /// reviewed, with its `dna.decided` event, in one transaction.
    pub fn dna_accept(&mut self, id: &str, slug: &str, note: &str, content_hash: &str) -> Result<Proposal> {
        let boot = self.boot_id().to_string();
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let p = decide(&tx, &boot, id, true, Some(slug), note)?;
        record_review(&tx, &p.repo, slug, content_hash, "accepted")?;
        tx.commit()?;
        Ok(p)
    }

    /// Records that a person accepted or confirmed this exact content (`how`: `accepted` or
    /// `confirmed`), superseding the slug's earlier reviews, and appends `dna.changed`, in one
    /// transaction.
    pub fn dna_record_review(&mut self, repo: &str, slug: &str, content_hash: &str, how: &str) -> Result<()> {
        let boot = self.boot_id().to_string();
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        record_review(&tx, repo, slug, content_hash, how)?;
        insert_event(&tx, &boot, None, "dna.changed", &json!({ "repo": repo, "slug": slug }))?;
        tx.commit()?;
        Ok(())
    }

    pub fn dna_is_reviewed(&self, repo: &str, slug: &str, content_hash: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM dna_reviews
                 WHERE repo = ?1 AND slug = ?2 AND content_hash = ?3 AND superseded_at IS NULL",
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

/// A stat signature of `.mirante/memory`: every entry's name, type, size and mtime,
/// hashed. `"absent"` when the directory does not exist.
pub fn signature(repo: &Path) -> std::result::Result<String, DnaError> {
    match MemoryDir::open(repo)? {
        Some(dir) => signature_of(&dir),
        None => Ok("absent".into()),
    }
}

fn signature_of(dir: &MemoryDir) -> std::result::Result<String, DnaError> {
    let mut lines: Vec<String> = dir
        .entries(MAX_NOTES)?
        .iter()
        .map(|e| format!("{}\0{:o}\0{}\0{}.{}", e.name, e.mode, e.size, e.mtime, e.mtime_nsec))
        .collect();
    lines.sort();
    Ok(sha256_hex(lines.join("\n").as_bytes()))
}

/// Reads `.mirante/memory` through one [`MemoryDir`] descriptor: at most [`MAX_NOTES`]
/// entries; regular `*.md` files only (symlinks, directories, dotfiles such as the
/// `.<name>.tmp-*` files of a write in progress, other extensions and `README.md` are
/// skipped); each read without following symlinks, single-link and size-capped. A file
/// whose name is not a slug is listed as invalid; one that does not parse is indexed with
/// its `parse_error`.
pub fn scan(repo: &Path) -> std::result::Result<Scan, DnaError> {
    let Some(dir) = MemoryDir::open(repo)? else {
        return Ok(Scan { notes: Vec::new(), invalid: Vec::new(), signature: "absent".into() });
    };
    let signature = signature_of(&dir)?;
    let (mut notes, mut invalid) = (Vec::new(), Vec::new());
    for (i, entry) in dir.entries(MAX_NOTES)?.into_iter().enumerate() {
        if i >= MAX_NOTES {
            invalid.push(InvalidNote {
                file: String::new(),
                error: format!("more than {MAX_NOTES} entries; rest skipped"),
            });
            break;
        }
        let name = entry.name.clone();
        if name.starts_with('.') || name.eq_ignore_ascii_case("README.md") || !entry.is_file() {
            continue;
        }
        add_file(&name, || dir.read_file(&name), &mut notes, &mut invalid);
    }
    notes.sort_by(|a, b| a.slug.cmp(&b.slug));
    invalid.sort_by(|a, b| a.file.cmp(&b.file));
    Ok(Scan { notes, invalid, signature })
}

/// [`scan`] of `.mirante/memory` as committed at `commit` (a full sha), read through the
/// hardened git (`ls-tree`, `cat-file`), never the working tree: what a CI job checks at a
/// revision. The same rules: at most [`MAX_NOTES`] entries, regular `*.md` files only
/// (symlinks, subdirectories, dotfiles and `README.md` skipped), size-capped. No signature
/// (`rev:<commit>`).
pub fn scan_at(repo: &Path, commit: &str) -> std::result::Result<Scan, DnaError> {
    let files = memory_files_at(repo, commit, MAX_NOTES + 1)?;
    let (mut notes, mut invalid) = (Vec::new(), Vec::new());
    for (i, (name, text)) in files.into_iter().enumerate() {
        if i >= MAX_NOTES {
            invalid.push(InvalidNote {
                file: String::new(),
                error: format!("more than {MAX_NOTES} entries; rest skipped"),
            });
            break;
        }
        if name.starts_with('.') || name.eq_ignore_ascii_case("README.md") {
            continue;
        }
        add_file(&name, || text, &mut notes, &mut invalid);
    }
    notes.sort_by(|a, b| a.slug.cmp(&b.slug));
    invalid.sort_by(|a, b| a.file.cmp(&b.file));
    Ok(Scan { notes, invalid, signature: format!("rev:{commit}") })
}

/// One file of the memory directory: a note (indexed, with its `parse_error` when it does
/// not parse) or an invalid entry. Files that are not `*.md` are skipped; `read` runs only
/// for a file whose name is a slug.
fn add_file(
    name: &str,
    read: impl FnOnce() -> std::result::Result<String, DnaError>,
    notes: &mut Vec<IndexedNote>,
    invalid: &mut Vec<InvalidNote>,
) {
    let file = name.to_string();
    let Some(stem) = name.strip_suffix(".md").map(str::to_string) else { return };
    if !note::is_slug(&stem) {
        invalid.push(InvalidNote { file, error: "the file name is not a note name (a-z, 0-9 and -)".into() });
        return;
    }
    let text = match read() {
        Ok(t) => t,
        Err(e) => {
            invalid.push(InvalidNote { file, error: e.to_string() });
            return;
        }
    };
    let content_hash = sha256_hex(text.as_bytes());
    match note::parse(&text) {
        Ok(n) => notes.push(IndexedNote {
            slug: stem,
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
            invalid.push(InvalidNote { file, error: e.to_string() });
            notes.push(IndexedNote {
                slug: stem.clone(),
                id: String::new(),
                title: stem,
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

/// The HEAD a scan was checked at and each anchor's state (slug, idx, state).
pub type Checks = (Option<String>, Vec<(String, u32, AnchorState)>);

/// Anchor states of a scan's notes at the repo's HEAD. git only; no store. Without a HEAD
/// commit every anchor stays `Unchecked`. `prev`: the states of the last check
/// ([`Store::dna_previous`]), reused for anchors whose path did not change since
/// ([`check_since`]); `None` checks every anchor.
pub fn check_scan(repo: &Path, scan: &Scan, prev: Option<&Previous>) -> std::result::Result<Checks, DnaError> {
    let keys: Vec<(String, u32, Anchor)> = scan
        .notes
        .iter()
        .flat_map(|n| n.anchors.iter().enumerate().map(|(i, a)| (n.slug.clone(), i as u32, a.clone())))
        .collect();
    let Ok(head) = rev_parse(repo, "HEAD") else {
        return Ok((None, keys.into_iter().map(|(s, i, _)| (s, i, AnchorState::Unchecked)).collect()));
    };
    let anchors: Vec<Anchor> = keys.iter().map(|(_, _, a)| a.clone()).collect();
    let checked = check_since(repo, &head, &anchors, prev)?;
    Ok((Some(head), keys.into_iter().zip(checked).map(|((s, i, _), c)| (s, i, c.state)).collect()))
}

/// Writes a scan and its checks to the index and reports the counts.
pub fn apply(store: &mut Store, key: &str, scan: &Scan, checks: &Checks) -> Result<RefreshReport> {
    let (head, states) = checks;
    store.dna_apply_index(key, &scan.notes, head.as_deref(), states)?;
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

/// Re-reads `.mirante/memory` into the index and checks every anchor at HEAD (reusing the
/// last check's states where the cited path did not change). Notes that do not parse are
/// listed, never deleted.
pub fn refresh(store: &mut Store, repo: &Path) -> Result<RefreshReport> {
    let key = repo_key(repo).map_err(dna)?;
    let scan = scan(repo).map_err(dna)?;
    let prev = store.dna_previous(&key)?;
    let checks = check_scan(repo, &scan, prev.as_ref()).map_err(dna)?;
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
        // Each term is cut to 64 chars.
        assert_eq!(fts_query(&"é".repeat(1000)).unwrap(), format!("\"{}\"*", "é".repeat(64)));
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

    const CAPS: ProposalCaps = ProposalCaps { per_task: 20, per_repo: 200 };

    fn kinds(s: &Store) -> Vec<(Option<String>, String)> {
        s.events_since(0, 1000).unwrap().into_iter().map(|e| (e.task_id, e.kind)).collect()
    }

    #[test]
    fn migration_5_creates_fts_and_the_decided_once_trigger() {
        let mut s = Store::open_in_memory().unwrap();
        assert!(s.schema_version().unwrap() >= 5);
        let p = s.dna_insert_proposal(&new_proposal("r", None), CAPS).unwrap();
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
        let a = s.dna_insert_proposal(&new_proposal("r", None), CAPS).unwrap();
        s.dna_insert_proposal(&new_proposal("r2", None), CAPS).unwrap();
        s.dna_decide(&a.id, false, None, "no").unwrap();
        let q = |repo: Option<&str>, status: Option<&str>| ProposalQuery {
            repo: repo.map(str::to_string),
            status: status.map(str::to_string),
            limit: 100,
            ..Default::default()
        };
        assert_eq!(s.dna_proposals(&q(Some("r"), None)).unwrap().len(), 1);
        assert_eq!(s.dna_proposals(&q(None, Some("pending"))).unwrap().len(), 1);
        assert_eq!(s.dna_status("r2").unwrap().pending, 1);
        assert_eq!(s.dna_proposal(&a.id).unwrap().decision_note, "no");
    }

    #[test]
    fn migration_6_supersedes_older_reviews_of_a_slug() {
        let mut s = Store::open_in_memory().unwrap();
        assert_eq!(s.schema_version().unwrap(), 6);
        s.dna_record_review("r", "a", "h1", "accepted").unwrap();
        s.dna_record_review("r", "a", "h2", "confirmed").unwrap();
        // Retired (h2): restoring the old accepted text (h1) is not reviewed any more.
        assert!(!s.dna_is_reviewed("r", "a", "h1").unwrap());
        assert!(s.dna_is_reviewed("r", "a", "h2").unwrap());
        // Another slug's ledger is untouched; a person confirming h1 again makes it current.
        s.dna_record_review("r", "b", "h1", "accepted").unwrap();
        assert!(s.dna_is_reviewed("r", "b", "h1").unwrap());
        s.dna_record_review("r", "a", "h1", "confirmed").unwrap();
        assert!(s.dna_is_reviewed("r", "a", "h1").unwrap() && !s.dna_is_reviewed("r", "a", "h2").unwrap());
        let mut a = row("a", "A", "x");
        a.content_hash = "h2".into();
        s.dna_replace_index("r", &[a]).unwrap();
        assert_eq!(s.dna_status("r").unwrap().unreviewed, 1);
    }

    #[test]
    fn every_dna_state_change_writes_its_event_in_the_same_transaction() {
        let mut s = Store::open_in_memory().unwrap();
        s.conn
            .execute(
                "INSERT INTO tasks (id, title, repo_path, worktree_path, branch, agent, goal, notes, lifecycle, status,
                     created_at, updated_at) VALUES ('t1', 't', '/r', '/w', 'b', 'claude', 'g', '', 'active', 'idle', 0, 0)",
                [],
            )
            .unwrap();
        let p = s.dna_insert_proposal(&new_proposal("r", Some("t1")), CAPS).unwrap();
        let q = s.dna_insert_proposal(&new_proposal("r", None), CAPS).unwrap();
        s.dna_accept(&p.id, "slug-a", "ok", "h").unwrap();
        s.dna_decide(&q.id, false, None, "no").unwrap();
        s.dna_record_review("r", "slug-a", "h2", "confirmed").unwrap();
        let t1 = Some("t1".to_string());
        assert_eq!(
            kinds(&s),
            [
                (t1.clone(), "dna.proposed".to_string()),
                (None, "dna.proposed".to_string()),
                (t1, "dna.decided".to_string()),
                (None, "dna.decided".to_string()),
                (None, "dna.changed".to_string()),
            ]
        );
        let ev = s.events_since(0, 1000).unwrap();
        // Every dna.* event names its repo (the key the cockpit refetches by).
        assert_eq!(ev[0].payload, serde_json::json!({ "id": p.id, "title": p.title, "repo": "r" }));
        assert_eq!(ev[2].payload, serde_json::json!({ "id": p.id, "accepted": true, "slug": "slug-a", "repo": "r" }));
        assert_eq!(ev[3].payload["repo"], "r");
        assert_eq!(ev[4].payload, serde_json::json!({ "repo": "r", "slug": "slug-a" }));
        // A refused decision writes neither the state nor an event.
        assert!(s.dna_decide(&p.id, false, None, "").is_err());
        assert_eq!(s.events_since(0, 1000).unwrap().len(), 5);
    }

    #[test]
    fn pending_caps_are_counted_inside_the_insert() {
        let mut s = Store::open_in_memory().unwrap();
        let caps = ProposalCaps { per_task: 2, per_repo: 3 };
        s.conn
            .execute(
                "INSERT INTO tasks (id, title, repo_path, worktree_path, branch, agent, goal, notes, lifecycle, status,
                     created_at, updated_at) VALUES ('t1', 't', '/r', '/w', 'b', 'claude', 'g', '', 'active', 'idle', 0, 0)",
                [],
            )
            .unwrap();
        s.dna_insert_proposal(&new_proposal("r", Some("t1")), caps).unwrap();
        s.dna_insert_proposal(&new_proposal("r", Some("t1")), caps).unwrap();
        let task = s.dna_insert_proposal(&new_proposal("r", Some("t1")), caps);
        assert!(matches!(&task, Err(CoreError::Limit(m)) if m.contains("this task already has 2")), "{task:?}");
        s.dna_insert_proposal(&new_proposal("r", None), caps).unwrap();
        let repo = s.dna_insert_proposal(&new_proposal("r", None), caps);
        assert!(matches!(&repo, Err(CoreError::Limit(m)) if m.contains("this repo already has 3")), "{repo:?}");
        // Another repo has its own budget; nothing refused was stored.
        s.dna_insert_proposal(&new_proposal("r2", None), caps).unwrap();
        assert_eq!(s.dna_status("r").unwrap().pending, 3);
    }

    #[test]
    fn proposals_page_by_task_status_and_cursor() {
        let mut s = Store::open_in_memory().unwrap();
        s.conn
            .execute(
                "INSERT INTO tasks (id, title, repo_path, worktree_path, branch, agent, goal, notes, lifecycle, status,
                     created_at, updated_at) VALUES ('t1', 't', '/r', '/w', 'b', 'claude', 'g', '', 'active', 'idle', 0, 0)",
                [],
            )
            .unwrap();
        let ids: Vec<String> = (0..5)
            .map(|i| {
                let task = if i % 2 == 0 { Some("t1") } else { None };
                s.dna_insert_proposal(&new_proposal("r", task), CAPS).unwrap().id
            })
            .collect();
        let page = |q: ProposalQuery| s.dna_proposals(&q).unwrap().into_iter().map(|p| p.id).collect::<Vec<_>>();
        let all = ProposalQuery { limit: 100, ..Default::default() };
        assert_eq!(page(all.clone()), ids);
        let first = page(ProposalQuery { limit: 2, ..all.clone() });
        assert_eq!(first, ids[..2]);
        assert_eq!(page(ProposalQuery { limit: 2, after: Some(first[1].clone()), ..all.clone() }), ids[2..4]);
        let t1 = page(ProposalQuery { task_id: Some("t1".into()), ..all.clone() });
        assert_eq!(t1, [ids[0].clone(), ids[2].clone(), ids[4].clone()]);
        assert_eq!(page(ProposalQuery { id: Some(ids[3].clone()), ..all.clone() }), [ids[3].clone()]);
        let unknown = s.dna_proposals(&ProposalQuery { after: Some("nope".into()), ..all });
        assert!(matches!(unknown, Err(CoreError::NotFound { .. })), "{unknown:?}");
        let plan: String = s
            .conn
            .query_row(
                "EXPLAIN QUERY PLAN SELECT id FROM dna_proposals WHERE task_id = 't1' AND status = 'pending'",
                [],
                |r| r.get(3),
            )
            .unwrap();
        assert!(plan.contains("dna_proposals_task"), "{plan}");
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
        .unwrap()
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
        // A write in progress (temp file) and other dotfiles are not notes.
        std::fs::write(dir.join(".good.md.tmp-0193"), "partial").unwrap();
        std::fs::write(dir.join(".hidden.md"), "x").unwrap();
        // A hard link planted in place of a note is refused, never indexed.
        std::fs::write(r.path().join("secret.txt"), "secret").unwrap();
        std::fs::hard_link(r.path().join("secret.txt"), dir.join("linked.md")).unwrap();
        let mut s = Store::open_in_memory().unwrap();
        let rep = refresh(&mut s, r.path()).unwrap();
        assert_eq!(rep.notes, 1);
        assert_eq!(rep.invalid.iter().map(|i| i.file.as_str()).collect::<Vec<_>>(), ["Not A Slug.md", "linked.md"]);
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
    fn a_refresh_after_head_moves_starts_from_the_stored_states() {
        let r = TempDir::new("dna-since");
        plain(r.path(), &["init", "-q", "-b", "main"]);
        commit_file(r.path(), "src/a.rs", CODE);
        commit_file(r.path(), "src/b.rs", CODE);
        fsio::init(r.path()).unwrap();
        let dir = r.path().join(".mirante/memory");
        std::fs::write(dir.join("a.md"), note_text(r.path(), "a", "src/a.rs", 2, 4)).unwrap();
        std::fs::write(dir.join("b.md"), note_text(r.path(), "b", "src/b.rs", 2, 4)).unwrap();
        let mut s = Store::open_in_memory().unwrap();
        let key = repo_key(r.path()).unwrap();
        assert!(s.dna_previous(&key).unwrap().is_none(), "nothing checked yet");
        refresh(&mut s, r.path()).unwrap();
        let prev = s.dna_previous(&key).unwrap().expect("states at the checked head");
        assert_eq!(prev.head, rev_parse(r.path(), "HEAD").unwrap());
        assert_eq!(prev.states.len(), 2);
        assert!(prev.states.values().all(|st| *st == AnchorState::Fresh));
        // b's cited lines change between the stored HEAD and the new one: stale, not reused.
        commit_file(r.path(), "src/b.rs", &CODE.replace("write()", "flush()"));
        let rep = refresh(&mut s, r.path()).unwrap();
        assert_eq!((rep.stale, rep.relocated), (1, 0));
        assert_eq!(s.dna_note(&key, "a").unwrap().unwrap().anchors[0].state, AnchorState::Fresh);
        assert!(matches!(s.dna_note(&key, "b").unwrap().unwrap().anchors[0].state, AnchorState::Stale { .. }));
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
