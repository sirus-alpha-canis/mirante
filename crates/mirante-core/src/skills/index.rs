//! The skills index in the store (migration 7): each task's origin (fixed at creation), the
//! tenants with learning enabled, the skills and their current version, the trust ledger of
//! manifest hashes a person accepted, the proposal queue and per-session usage.
//!
//! Every state change runs in one transaction with its audit event (`skill.proposed`,
//! `skill.decided`, `skill.changed`, `skill.learning`, `skill.injected`, `skill.refused`,
//! `skill.used`).
use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::format::Activation;
use super::lint::Finding;
use super::mine::{Source, Utterance};
use super::origin::Scope;
use crate::error::{CoreError, Result};
use crate::model::{new_id, now_ms};
use crate::store::{insert_event, Store};

/// A skill as the index knows it: its current version and what the manifest says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRow {
    pub name: String,
    /// The current version.
    pub version: u32,
    /// The current version's manifest hash (what the ledger must hold).
    pub manifest_hash: String,
    pub scope: Scope,
    pub activation: Activation,
    pub agents: Vec<String>,
    /// `active` or `retired`.
    pub status: String,
    pub description: String,
    pub updated_at: i64,
}

/// A task's origin, fixed at its creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOrigin {
    /// The checkout's canonical git common dir.
    pub repo: String,
    /// The normalized origin key (`host/path`, or `local/<hash>`).
    pub origin: String,
    pub tenant: String,
    /// The live origin at creation when it was not confirmed (it differed from the pin, or
    /// no person created the task of an unpinned checkout): no skills until a person
    /// confirms.
    pub observed: Option<String>,
}

/// A tenant: one that has tasks, or one whose learning a person set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantRow {
    pub tenant: String,
    pub learning: bool,
    /// Tasks whose origin is in this tenant.
    pub tasks: usize,
}

/// An extra file of a proposal (text only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalFile {
    pub path: String,
    pub text: String,
}

/// One example of what the operator said (redacted), for a mined proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceItem {
    pub task: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSkillProposal {
    /// `miner`, `agent`, `cli` or `import`.
    pub origin: String,
    pub task_id: Option<String>,
    pub tenant: String,
    /// `new` or `edit`.
    pub kind: String,
    pub name: String,
    /// For an edit: the manifest hash of the version it edits.
    pub base_hash: Option<String>,
    pub scope: Scope,
    pub activation: Activation,
    pub description: String,
    pub body: String,
    pub files: Vec<ProposalFile>,
    pub evidence: Vec<EvidenceItem>,
    pub fingerprint: Option<String>,
    pub lint: Vec<Finding>,
    pub redactions: usize,
}

/// A proposed skill (or edit) waiting for, or past, a person's review. `scope` is rendered
/// (`personal`, `org:<prefix>`, `repo:<key>`); `activation` is `always` or `on_demand`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillProposal {
    pub id: String,
    pub origin: String,
    pub task_id: Option<String>,
    pub tenant: String,
    pub kind: String,
    pub name: String,
    pub base_hash: Option<String>,
    pub scope: String,
    pub activation: String,
    pub description: String,
    pub body: String,
    pub files: Vec<ProposalFile>,
    pub evidence: Vec<EvidenceItem>,
    pub fingerprint: Option<String>,
    pub lint: Vec<Finding>,
    pub redactions: usize,
    /// `pending`, `accepted` or `rejected`.
    pub status: String,
    /// The version an accept created.
    pub version: Option<u32>,
    pub decision_note: String,
    pub created_at: i64,
    pub decided_at: Option<i64>,
}

/// What [`Store::skill_insert_proposal`] did: queued a new proposal, or (`new: false`)
/// updated the evidence of the pending one with the same fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inserted {
    pub proposal: SkillProposal,
    pub new: bool,
}

/// Most pending proposals one task, and one tenant, may have at once (counted inside the
/// insert's transaction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkillCaps {
    pub per_task: usize,
    pub per_tenant: usize,
}

/// Which proposals [`Store::skill_proposals`] returns: oldest first, filtered, at most
/// `limit`, after the proposal `after` (a cursor).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillProposalQuery {
    pub tenant: Option<String>,
    pub task_id: Option<String>,
    pub status: Option<String>,
    pub id: Option<String>,
    pub after: Option<String>,
    pub limit: usize,
}

/// What an accepted proposal became (the person's edits applied), written back to its row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Final {
    pub name: String,
    pub description: String,
    pub body: String,
    pub scope: Scope,
    pub activation: Activation,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageCounts {
    pub injected: usize,
    pub used: usize,
    pub refused: usize,
}

const SKILL_COLUMNS: &str =
    "name, current_version, manifest_hash, scope_kind, scope_value, activation, agents, status, description, updated_at";
const PROPOSAL_COLUMNS: &str = "id, origin, task_id, tenant, kind, name, base_hash, scope_kind, scope_value, \
     activation, description, body, files, evidence, fingerprint, lint, redactions, status, version, decision_note, \
     created_at, decided_at";

fn bad_column(i: usize, e: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
}

fn json_column<T: serde::de::DeserializeOwned>(r: &Row<'_>, i: usize) -> rusqlite::Result<T> {
    let text: String = r.get(i)?;
    serde_json::from_str(&text).map_err(|e| bad_column(i, e))
}

fn skill_from_row(r: &Row<'_>) -> rusqlite::Result<SkillRow> {
    let kind: String = r.get(3)?;
    let value: String = r.get(4)?;
    let activation: String = r.get(5)?;
    Ok(SkillRow {
        name: r.get(0)?,
        version: r.get(1)?,
        manifest_hash: r.get(2)?,
        scope: Scope::from_parts(&kind, &value).map_err(|e| bad_column(3, e))?,
        activation: Activation::parse(&activation)
            .ok_or_else(|| bad_column(5, std::io::Error::other(format!("activation {activation:?}"))))?,
        agents: json_column(r, 6)?,
        status: r.get(7)?,
        description: r.get(8)?,
        updated_at: r.get(9)?,
    })
}

fn render_scope(kind: &str, value: &str) -> String {
    if kind == "personal" {
        "personal".into()
    } else {
        format!("{kind}:{value}")
    }
}

fn proposal_from_row(r: &Row<'_>) -> rusqlite::Result<SkillProposal> {
    let kind: String = r.get(7)?;
    let value: String = r.get(8)?;
    Ok(SkillProposal {
        id: r.get(0)?,
        origin: r.get(1)?,
        task_id: r.get(2)?,
        tenant: r.get(3)?,
        kind: r.get(4)?,
        name: r.get(5)?,
        base_hash: r.get(6)?,
        scope: render_scope(&kind, &value),
        activation: r.get(9)?,
        description: r.get(10)?,
        body: r.get(11)?,
        files: json_column(r, 12)?,
        evidence: json_column(r, 13)?,
        fingerprint: r.get(14)?,
        lint: json_column(r, 15)?,
        redactions: r.get::<_, i64>(16)? as usize,
        status: r.get(17)?,
        version: r.get(18)?,
        decision_note: r.get(19)?,
        created_at: r.get(20)?,
        decided_at: r.get(21)?,
    })
}

fn load_proposal(c: &Connection, id: &str) -> Result<SkillProposal> {
    c.query_row(&format!("SELECT {PROPOSAL_COLUMNS} FROM skill_proposals WHERE id = ?1"), [id], proposal_from_row)
        .optional()?
        .ok_or_else(|| CoreError::NotFound { kind: "skill proposal", id: id.to_string() })
}

fn load_skill(c: &Connection, name: &str) -> Result<Option<SkillRow>> {
    Ok(c.query_row(&format!("SELECT {SKILL_COLUMNS} FROM skills WHERE name = ?1"), [name], skill_from_row)
        .optional()?)
}

fn count(c: &Connection, sql: &str, arg: &str) -> Result<usize> {
    Ok(c.query_row(sql, [arg], |r| r.get::<_, i64>(0))? as usize)
}

/// Marks a pending proposal decided (the trigger refuses a second decision even if this
/// check is bypassed), writes what an accept became, and appends `skill.decided`, in the
/// caller's transaction.
fn decide(
    tx: &Transaction<'_>,
    boot_id: &str,
    id: &str,
    accepted: bool,
    fin: Option<(&Final, u32)>,
    note: &str,
) -> Result<SkillProposal> {
    let p = load_proposal(tx, id)?;
    let to = if accepted { "accepted" } else { "rejected" };
    if p.status != "pending" {
        return Err(CoreError::InvalidTransition { kind: "skill proposal", from: p.status, to: to.into() });
    }
    if let Some((f, _)) = fin {
        tx.execute(
            "UPDATE skill_proposals SET name = ?2, description = ?3, body = ?4, scope_kind = ?5, scope_value = ?6,
                 activation = ?7 WHERE id = ?1",
            params![id, f.name, f.description, f.body, f.scope.kind(), f.scope.value(), f.activation.as_str()],
        )?;
    }
    tx.execute(
        "UPDATE skill_proposals SET status = ?2, version = ?3, decision_note = ?4, decided_at = ?5 WHERE id = ?1",
        params![id, to, fin.map(|(_, v)| v), note, now_ms()],
    )?;
    let p = load_proposal(tx, id)?;
    let payload = json!({ "id": p.id, "accepted": accepted, "name": p.name, "version": p.version });
    insert_event(tx, boot_id, p.task_id.as_deref(), "skill.decided", &payload)?;
    Ok(p)
}

/// Records `hash` as the name's current reviewed manifest; every other review of the name
/// is superseded (kept as history, no longer counted).
fn record_review(c: &Connection, name: &str, version: u32, hash: &str, how: &str) -> Result<()> {
    let now = now_ms();
    c.execute(
        "UPDATE skill_reviews SET superseded_at = ?3 WHERE name = ?1 AND version != ?2 AND superseded_at IS NULL",
        params![name, version, now],
    )?;
    c.execute(
        "INSERT INTO skill_reviews (name, version, manifest_hash, how, reviewed_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (name, version) DO UPDATE SET manifest_hash = ?3, how = ?4, reviewed_at = ?5, superseded_at = NULL",
        params![name, version, hash, how, now],
    )?;
    Ok(())
}

fn upsert(c: &Connection, s: &SkillRow) -> Result<()> {
    c.execute(
        &format!(
            "INSERT INTO skills ({SKILL_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (name) DO UPDATE SET current_version = ?2, manifest_hash = ?3, scope_kind = ?4,
                 scope_value = ?5, activation = ?6, agents = ?7, status = ?8, description = ?9, updated_at = ?10"
        ),
        params![
            s.name,
            s.version,
            s.manifest_hash,
            s.scope.kind(),
            s.scope.value(),
            s.activation.as_str(),
            serde_json::to_string(&s.agents)?,
            s.status,
            s.description,
            now_ms()
        ],
    )?;
    Ok(())
}

fn repo_origin(c: &Connection, repo: &str) -> Result<Option<(String, String)>> {
    Ok(c.query_row("SELECT origin, tenant FROM repo_origins WHERE repo = ?1", [repo], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?)
}

fn pin(c: &Connection, repo: &str, origin: &str, tenant: &str) -> Result<()> {
    c.execute(
        "INSERT INTO repo_origins (repo, origin, tenant, updated_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (repo) DO UPDATE SET origin = ?2, tenant = ?3, updated_at = ?4",
        params![repo, origin, tenant, now_ms()],
    )?;
    Ok(())
}

fn insert_usage(
    c: &Connection,
    session: &str,
    task: &str,
    name: &str,
    version: u32,
    kind: &str,
    detail: &str,
) -> Result<()> {
    c.execute(
        "INSERT INTO skill_usage (session_id, task_id, name, version, kind, detail, ts_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![session, task, name, version, kind, detail, now_ms()],
    )?;
    Ok(())
}

impl Store {
    /// Fixes `task`'s origin at its creation, against the origin pinned for its checkout
    /// (`repo`: the canonical git common dir), in one transaction:
    /// - the live origin is the pinned one: the task gets it;
    /// - it differs: the task keeps the pinned origin and tenant, and `observed` records the
    ///   live one (no skills until a person confirms; see [`Store::skill_confirm_origin`]);
    /// - nothing is pinned yet: a person's task pins the live origin (`skill.origin_pinned`);
    ///   anyone else's gets it as `observed` too.
    ///
    /// Written once: a second write fails (the primary key) and triggers refuse any update
    /// but the confirmation's.
    pub fn skill_create_origin(
        &mut self,
        task: &str,
        repo: &str,
        live_origin: &str,
        live_tenant: &str,
        by_person: bool,
    ) -> Result<TaskOrigin> {
        let boot = self.boot_id().to_string();
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pinned = repo_origin(&tx, repo)?;
        let (origin, tenant, observed) = match pinned {
            Some((o, t)) if o == live_origin => (o, t, None),
            Some((o, t)) => (o, t, Some(live_origin.to_string())),
            None if by_person => {
                pin(&tx, repo, live_origin, live_tenant)?;
                let payload = json!({ "repo": repo, "origin": live_origin, "tenant": live_tenant });
                insert_event(&tx, &boot, Some(task), "skill.origin_pinned", &payload)?;
                (live_origin.to_string(), live_tenant.to_string(), None)
            }
            None => (live_origin.to_string(), live_tenant.to_string(), Some(live_origin.to_string())),
        };
        tx.execute(
            "INSERT INTO task_origins (task_id, repo, origin, tenant, observed, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![task, repo, origin, tenant, observed, now_ms()],
        )?;
        tx.commit()?;
        Ok(TaskOrigin { repo: repo.to_string(), origin, tenant, observed })
    }

    /// `task`'s origin as fixed at its creation.
    pub fn skill_origin_of(&self, task: &str) -> Result<Option<TaskOrigin>> {
        Ok(self
            .conn
            .query_row("SELECT repo, origin, tenant, observed FROM task_origins WHERE task_id = ?1", [task], |r| {
                Ok(TaskOrigin { repo: r.get(0)?, origin: r.get(1)?, tenant: r.get(2)?, observed: r.get(3)? })
            })
            .optional()?)
    }

    /// The origin pinned for the checkout `repo`: `(origin, tenant)`.
    pub fn skill_repo_origin(&self, repo: &str) -> Result<Option<(String, String)>> {
        repo_origin(&self.conn, repo)
    }

    /// A person confirms `origin` as the checkout's: it becomes the pin, and the tasks of
    /// that checkout whose fixed origin is `origin` lose their `observed` mark (they get
    /// skills again). Tasks fixed to another origin stay unconfirmed. `skill.origin_confirmed`
    /// in the same transaction. Returns how many tasks were cleared.
    pub fn skill_confirm_origin(&mut self, repo: &str, origin: &str, tenant: &str) -> Result<usize> {
        let boot = self.boot_id().to_string();
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        pin(&tx, repo, origin, tenant)?;
        let cleared = tx.execute(
            "UPDATE task_origins SET observed = NULL WHERE repo = ?1 AND origin = ?2 AND observed IS NOT NULL",
            params![repo, origin],
        )?;
        let payload = json!({ "repo": repo, "origin": origin, "tenant": tenant, "cleared": cleared });
        insert_event(&tx, &boot, None, "skill.origin_confirmed", &payload)?;
        tx.commit()?;
        Ok(cleared)
    }

    /// Every tenant that has tasks or a learning setting, by name.
    pub fn skill_tenants(&self) -> Result<Vec<TenantRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.tenant, COALESCE((SELECT learning FROM skill_tenants s WHERE s.tenant = t.tenant), 0),
                    (SELECT COUNT(*) FROM task_origins o WHERE o.tenant = t.tenant)
             FROM (SELECT tenant FROM task_origins UNION SELECT tenant FROM skill_tenants) t ORDER BY t.tenant",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(TenantRow { tenant: r.get(0)?, learning: r.get::<_, i64>(1)? == 1, tasks: r.get::<_, i64>(2)? as usize })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn skill_learning(&self, tenant: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT learning FROM skill_tenants WHERE tenant = ?1", [tenant], |r| r.get::<_, i64>(0))
            .optional()?
            == Some(1))
    }

    /// Turns mining on or off for `tenant`, with its `skill.learning` event.
    pub fn skill_set_learning(&mut self, tenant: &str, enabled: bool) -> Result<()> {
        let boot = self.boot_id().to_string();
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO skill_tenants (tenant, learning, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (tenant) DO UPDATE SET learning = ?2, updated_at = ?3",
            params![tenant, enabled as i64, now_ms()],
        )?;
        insert_event(&tx, &boot, None, "skill.learning", &json!({ "tenant": tenant, "learning": enabled }))?;
        tx.commit()?;
        Ok(())
    }

    pub fn skill_get(&self, name: &str) -> Result<Option<SkillRow>> {
        load_skill(&self.conn, name)
    }

    /// Every skill, by name.
    pub fn skill_list(&self) -> Result<Vec<SkillRow>> {
        let mut stmt = self.conn.prepare(&format!("SELECT {SKILL_COLUMNS} FROM skills ORDER BY name"))?;
        let rows = stmt.query_map([], skill_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Sets the skill's index row (its current version). No event: see
    /// [`Store::skill_accept`] and [`Store::skill_change`].
    pub fn skill_upsert_current(&mut self, s: &SkillRow) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        upsert(&tx, s)?;
        tx.commit()?;
        Ok(())
    }

    /// Records that a person reviewed version `version` of `name`, whose manifest hashes to
    /// `hash` (`how`: `accepted`, `rescoped` or `rolled_back`); the name's other reviews
    /// are superseded.
    pub fn skill_record_review(&mut self, name: &str, version: u32, hash: &str, how: &str) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        record_review(&tx, name, version, hash, how)?;
        tx.commit()?;
        Ok(())
    }

    /// Whether `hash` is the manifest a person last reviewed for `name`.
    pub fn skill_is_reviewed(&self, name: &str, hash: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM skill_reviews WHERE name = ?1 AND manifest_hash = ?2 AND superseded_at IS NULL",
                params![name, hash],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// The manifest hash a person reviewed for version `version` (superseded or not).
    pub fn skill_review_hash(&self, name: &str, version: u32) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT manifest_hash FROM skill_reviews WHERE name = ?1 AND version = ?2",
                params![name, version],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Queues a proposal with its `skill.proposed` event, counting the pending ones against
    /// `caps` in the same transaction. A pending proposal with the same fingerprint gets the
    /// new evidence instead (`new: false`, no event); a rejected fingerprint is `Limit`.
    pub fn skill_insert_proposal(&mut self, p: &NewSkillProposal, caps: SkillCaps) -> Result<Inserted> {
        let boot = self.boot_id().to_string();
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(fp) = &p.fingerprint {
            let seen: Vec<(String, String)> = {
                let mut stmt = tx.prepare(
                    "SELECT id, status FROM skill_proposals WHERE fingerprint = ?1 AND tenant = ?2
                     ORDER BY created_at, id",
                )?;
                let rows = stmt.query_map(params![fp, p.tenant], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            if seen.iter().any(|(_, s)| s == "rejected") {
                return Err(CoreError::Limit("a person rejected this proposal before".into()));
            }
            if let Some((id, _)) = seen.iter().find(|(_, s)| s == "pending") {
                tx.execute(
                    "UPDATE skill_proposals SET evidence = ?2 WHERE id = ?1",
                    params![id, serde_json::to_string(&p.evidence)?],
                )?;
                let proposal = load_proposal(&tx, id)?;
                tx.commit()?;
                return Ok(Inserted { proposal, new: false });
            }
        }
        if let Some(task) = &p.task_id {
            let n = count(&tx, "SELECT COUNT(*) FROM skill_proposals WHERE task_id = ?1 AND status = 'pending'", task)?;
            if n >= caps.per_task {
                let max = caps.per_task;
                return Err(CoreError::Limit(format!(
                    "this task already has {max} skill proposals waiting for review"
                )));
            }
        }
        let n = count(&tx, "SELECT COUNT(*) FROM skill_proposals WHERE tenant = ?1 AND status = 'pending'", &p.tenant)?;
        if n >= caps.per_tenant {
            let max = caps.per_tenant;
            return Err(CoreError::Limit(format!("this tenant already has {max} skill proposals waiting for review")));
        }
        let id = new_id();
        tx.execute(
            &format!(
                "INSERT INTO skill_proposals ({PROPOSAL_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                     ?11, ?12, ?13, ?14, ?15, ?16, ?17, 'pending', NULL, '', ?18, NULL)"
            ),
            params![
                id,
                p.origin,
                p.task_id,
                p.tenant,
                p.kind,
                p.name,
                p.base_hash,
                p.scope.kind(),
                p.scope.value(),
                p.activation.as_str(),
                p.description,
                p.body,
                serde_json::to_string(&p.files)?,
                serde_json::to_string(&p.evidence)?,
                p.fingerprint,
                serde_json::to_string(&p.lint)?,
                p.redactions as i64,
                now_ms()
            ],
        )?;
        let proposal = load_proposal(&tx, &id)?;
        let payload = json!({ "id": id, "name": proposal.name, "origin": proposal.origin, "tenant": proposal.tenant });
        insert_event(&tx, &boot, p.task_id.as_deref(), "skill.proposed", &payload)?;
        tx.commit()?;
        Ok(Inserted { proposal, new: true })
    }

    pub fn skill_proposal(&self, id: &str) -> Result<SkillProposal> {
        load_proposal(&self.conn, id)
    }

    /// Proposals, oldest first (see [`SkillProposalQuery`]). An unknown `after` is `NotFound`.
    pub fn skill_proposals(&self, q: &SkillProposalQuery) -> Result<Vec<SkillProposal>> {
        let cursor: Option<(i64, String)> = match &q.after {
            Some(id) => Some(
                self.conn
                    .query_row("SELECT created_at, id FROM skill_proposals WHERE id = ?1", [id], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })
                    .optional()?
                    .ok_or_else(|| CoreError::NotFound { kind: "skill proposal", id: id.clone() })?,
            ),
            None => None,
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {PROPOSAL_COLUMNS} FROM skill_proposals
             WHERE (?1 IS NULL OR tenant = ?1) AND (?2 IS NULL OR task_id = ?2) AND (?3 IS NULL OR status = ?3)
               AND (?4 IS NULL OR id = ?4)
               AND (?5 IS NULL OR created_at > ?5 OR (created_at = ?5 AND id > ?6))
             ORDER BY created_at, id LIMIT ?7"
        ))?;
        let rows = stmt.query_map(
            params![
                q.tenant,
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

    /// Rejects (or, without writing a version, accepts) a pending proposal, with its
    /// `skill.decided` event. A decided one is `InvalidTransition`.
    pub fn skill_decide(&mut self, id: &str, accepted: bool, note: &str) -> Result<SkillProposal> {
        let boot = self.boot_id().to_string();
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let p = decide(&tx, &boot, id, accepted, None, note)?;
        tx.commit()?;
        Ok(p)
    }

    /// Accepts a pending proposal as `row` (the version just written): the decision (with
    /// what the proposal became), the ledger entry, the index row and `skill.decided`
    /// commit together or not at all.
    pub fn skill_accept(&mut self, id: &str, fin: &Final, row: &SkillRow, note: &str) -> Result<SkillProposal> {
        let boot = self.boot_id().to_string();
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let p = decide(&tx, &boot, id, true, Some((fin, row.version)), note)?;
        record_review(&tx, &row.name, row.version, &row.manifest_hash, "accepted")?;
        upsert(&tx, row)?;
        tx.commit()?;
        Ok(p)
    }

    /// A person's action on a skill whose current manifest hash is `expect_hash`: sets
    /// `row` as the index row, records `how` in the ledger when the action made a version
    /// (`rescoped`, `rolled_back`), and appends `skill.changed`, in one transaction. A skill
    /// that moved on meanwhile is `InvalidTransition`.
    pub fn skill_change(&mut self, expect_hash: &str, row: &SkillRow, how: Option<&str>, action: &str) -> Result<()> {
        let boot = self.boot_id().to_string();
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current =
            load_skill(&tx, &row.name)?.ok_or_else(|| CoreError::NotFound { kind: "skill", id: row.name.clone() })?;
        if current.manifest_hash != expect_hash {
            return Err(CoreError::InvalidTransition {
                kind: "skill",
                from: format!("{} v{}", current.name, current.version),
                to: format!("{action} (it changed since you looked)"),
            });
        }
        if let Some(how) = how {
            record_review(&tx, &row.name, row.version, &row.manifest_hash, how)?;
        }
        upsert(&tx, row)?;
        let payload = json!({ "name": row.name, "version": row.version, "action": action });
        insert_event(&tx, &boot, None, "skill.changed", &payload)?;
        tx.commit()?;
        Ok(())
    }

    /// What the operator said and denied in `tenant`'s tasks since `since` (ms), oldest
    /// first per task: goals, `message_sent` / `message_queued` texts and the details of
    /// denied permission requests. Empty unless the tenant's learning is on; tasks whose
    /// origin is unconfirmed are left out.
    pub fn skill_history(&self, tenant: &str, since: i64) -> Result<Vec<Utterance>> {
        if !self.skill_learning(tenant)? {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT o.task_id, o.origin, 0, t.goal, t.created_at, '' FROM task_origins o JOIN tasks t ON t.id = o.task_id
                 WHERE o.tenant = ?1 AND o.observed IS NULL AND t.created_at >= ?2
             UNION ALL
             SELECT o.task_id, o.origin, CASE e.kind WHEN 'message_sent' THEN 1 ELSE 2 END,
                    COALESCE(json_extract(e.payload, '$.text'), ''), e.ts_ms, ''
                 FROM task_origins o JOIN events e ON e.task_id = o.task_id
                 WHERE o.tenant = ?1 AND o.observed IS NULL AND e.kind IN ('message_sent', 'message_queued') AND e.ts_ms >= ?2
             UNION ALL
             SELECT o.task_id, o.origin, 3, p.detail, p.created_at, p.tool
                 FROM task_origins o JOIN permission_requests p ON p.task_id = o.task_id
                 WHERE o.tenant = ?1 AND o.observed IS NULL AND p.status = 'denied' AND p.created_at >= ?2
             ORDER BY 5, 1",
        )?;
        let rows = stmt.query_map(params![tenant, since], |r| {
            let source = match r.get::<_, i64>(2)? {
                0 => Source::Goal,
                1 => Source::Steer,
                2 => Source::Queue,
                _ => Source::Denied { tool: r.get(5)? },
            };
            Ok(Utterance {
                task: r.get(0)?,
                repo_key: r.get(1)?,
                tenant: tenant.to_string(),
                text: r.get(3)?,
                ts: r.get(4)?,
                source,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().filter(|u| !u.text.is_empty()).collect())
    }

    /// For each task, the skills ever injected into its sessions (by name, sorted).
    pub fn skill_injected_by_task(&self) -> Result<HashMap<String, Vec<String>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT task_id, name FROM skill_usage WHERE kind = 'injected' ORDER BY task_id, name")?;
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (task, name) = row?;
            out.entry(task).or_default().push(name);
        }
        Ok(out)
    }

    /// What a new session got: `injected` (name, version) and `refused` (name, version,
    /// reason), as usage rows plus `skill.injected { session_id, names }` (when any) and one
    /// `skill.refused { session_id, name, reason }` each, in one transaction.
    pub fn skill_record_session(
        &mut self,
        session: &str,
        task: &str,
        injected: &[(String, u32)],
        refused: &[(String, u32, String)],
    ) -> Result<()> {
        let boot = self.boot_id().to_string();
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (name, version) in injected {
            insert_usage(&tx, session, task, name, *version, "injected", "")?;
        }
        if !injected.is_empty() {
            let names: Vec<&str> = injected.iter().map(|(n, _)| n.as_str()).collect();
            insert_event(&tx, &boot, Some(task), "skill.injected", &json!({ "session_id": session, "names": names }))?;
        }
        for (name, version, reason) in refused {
            insert_usage(&tx, session, task, name, *version, "refused", reason)?;
            let payload = json!({ "session_id": session, "name": name, "reason": reason });
            insert_event(&tx, &boot, Some(task), "skill.refused", &payload)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Records that the agent of `session` loaded the skill `name` (`skill.used`), when that
    /// skill was injected into that session; false (nothing recorded) otherwise.
    pub fn skill_record_used(&mut self, session: &str, task: &str, name: &str) -> Result<bool> {
        let boot = self.boot_id().to_string();
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: Option<u32> = tx
            .query_row(
                "SELECT version FROM skill_usage WHERE session_id = ?1 AND name = ?2 AND kind = 'injected'
                 ORDER BY ts_ms DESC LIMIT 1",
                params![session, name],
                |r| r.get(0),
            )
            .optional()?;
        let Some(version) = version else {
            return Ok(false);
        };
        insert_usage(&tx, session, task, name, version, "used", "")?;
        let payload = json!({ "session_id": session, "name": name, "version": version });
        insert_event(&tx, &boot, Some(task), "skill.used", &payload)?;
        tx.commit()?;
        Ok(true)
    }

    /// Usage per skill name since `since` (ms); names without usage are absent.
    pub fn skill_usage_counts(&self, since: i64) -> Result<HashMap<String, UsageCounts>> {
        let mut stmt =
            self.conn.prepare("SELECT name, kind, COUNT(*) FROM skill_usage WHERE ts_ms >= ?1 GROUP BY name, kind")?;
        let mut out: HashMap<String, UsageCounts> = HashMap::new();
        for row in
            stmt.query_map([since], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))?
        {
            let (name, kind, n) = row?;
            let c = out.entry(name).or_default();
            match kind.as_str() {
                "injected" => c.injected = n as usize,
                "used" => c.used = n as usize,
                _ => c.refused = n as usize,
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentKind, NewTask};
    use crate::skills::format::Activation;
    use crate::skills::mine::Source;
    use crate::skills::origin::Scope;
    use crate::store::Store;

    fn task(s: &mut Store, goal: &str) -> String {
        s.create_task(NewTask {
            title: "t".into(),
            repo_path: "/r".into(),
            worktree_path: format!("/w/{}", crate::model::new_id()),
            branch: "b".into(),
            agent: AgentKind::Claude,
            goal: goal.into(),
        })
        .unwrap()
        .id
    }

    fn proposal(tenant: &str, task: Option<&str>, fp: Option<&str>) -> NewSkillProposal {
        NewSkillProposal {
            origin: "miner".into(),
            task_id: task.map(str::to_string),
            tenant: tenant.into(),
            kind: "new".into(),
            name: "run-tests".into(),
            base_hash: None,
            scope: Scope::org(tenant),
            activation: Activation::Always,
            description: "Run the tests".into(),
            body: "Run the tests first.\n".into(),
            files: Vec::new(),
            evidence: vec![EvidenceItem { task: "t1".into(), text: "run the tests".into() }],
            fingerprint: fp.map(str::to_string),
            lint: Vec::new(),
            redactions: 0,
        }
    }

    fn row(name: &str, version: u32, hash: &str) -> SkillRow {
        SkillRow {
            name: name.into(),
            version,
            manifest_hash: hash.into(),
            scope: Scope::org("github.com/acme"),
            activation: Activation::Always,
            agents: vec!["claude".into(), "codex".into()],
            status: "active".into(),
            description: "d".into(),
            updated_at: 0,
        }
    }

    /// A person's task in a checkout of its own (keyed by the origin).
    fn origin(s: &mut Store, task: &str, origin: &str, tenant: &str) {
        s.skill_create_origin(task, origin, origin, tenant, true).unwrap();
    }

    const CAPS: SkillCaps = SkillCaps { per_task: 20, per_tenant: 200 };

    fn kinds(s: &Store) -> Vec<String> {
        s.events_since(0, 1000).unwrap().into_iter().map(|e| e.kind).collect()
    }

    #[test]
    fn migration_7_and_decided_once() {
        let mut s = Store::open_in_memory().unwrap();
        assert_eq!(s.schema_version().unwrap(), 7);
        let p = s.skill_insert_proposal(&proposal("github.com/acme", None, None), CAPS).unwrap();
        assert!(p.new);
        assert_eq!((p.proposal.status.as_str(), p.proposal.scope.as_str()), ("pending", "org:github.com/acme"));
        assert_eq!(p.proposal.evidence.len(), 1);
        let d = s.skill_decide(&p.proposal.id, false, "no").unwrap();
        assert_eq!((d.status.as_str(), d.decision_note.as_str()), ("rejected", "no"));
        assert!(s.skill_decide(&p.proposal.id, true, "").is_err());
        // The trigger is the backstop, whatever the Rust side checks.
        assert!(s.conn.execute("UPDATE skill_proposals SET status = 'accepted'", []).is_err());
        assert!(kinds(&s).ends_with(&["skill.proposed".to_string(), "skill.decided".to_string()]));
    }

    #[test]
    fn accept_writes_decision_ledger_index_and_event_together() {
        let mut s = Store::open_in_memory().unwrap();
        let p = s.skill_insert_proposal(&proposal("github.com/acme", None, None), CAPS).unwrap().proposal;
        let fin = Final {
            name: "run-tests".into(),
            description: "Edited".into(),
            body: "B\n".into(),
            scope: Scope::Personal,
            activation: Activation::OnDemand,
        };
        let d = s.skill_accept(&p.id, &fin, &row("run-tests", 1, "h1"), "ok").unwrap();
        assert_eq!((d.status.as_str(), d.version, d.scope.as_str()), ("accepted", Some(1), "personal"));
        assert_eq!((d.description.as_str(), d.activation.as_str()), ("Edited", "on_demand"));
        assert!(s.skill_is_reviewed("run-tests", "h1").unwrap());
        assert_eq!(s.skill_get("run-tests").unwrap().unwrap().manifest_hash, "h1");
        let ev = s.events_since(0, 100).unwrap();
        let decided = ev.iter().find(|e| e.kind == "skill.decided").unwrap();
        assert_eq!(decided.payload["version"], 1);
        assert_eq!(decided.payload["name"], "run-tests");
        // A second accept of the same proposal changes nothing.
        assert!(s.skill_accept(&p.id, &fin, &row("run-tests", 2, "h2"), "").is_err());
        assert_eq!(s.skill_get("run-tests").unwrap().unwrap().version, 1);
    }

    #[test]
    fn ledger_counts_only_the_latest_review_of_a_name() {
        let mut s = Store::open_in_memory().unwrap();
        s.skill_record_review("a", 1, "h1", "accepted").unwrap();
        assert!(s.skill_is_reviewed("a", "h1").unwrap());
        s.skill_record_review("a", 2, "h2", "rescoped").unwrap();
        assert!(!s.skill_is_reviewed("a", "h1").unwrap());
        assert!(s.skill_is_reviewed("a", "h2").unwrap());
        // History stays: the old version's hash is still known for a rollback.
        assert_eq!(s.skill_review_hash("a", 1).unwrap().as_deref(), Some("h1"));
        // Another name is untouched; an unknown `how` is refused.
        s.skill_record_review("b", 1, "h1", "accepted").unwrap();
        assert!(s.skill_is_reviewed("b", "h1").unwrap());
        assert!(s.skill_record_review("a", 3, "h3", "planted").is_err());
        // Rolling back to v1's content is a new version and becomes the only reviewed one.
        s.skill_record_review("a", 3, "h1", "rolled_back").unwrap();
        assert!(s.skill_is_reviewed("a", "h1").unwrap() && !s.skill_is_reviewed("a", "h2").unwrap());
    }

    #[test]
    fn change_checks_the_expected_hash_and_emits_changed() {
        let mut s = Store::open_in_memory().unwrap();
        s.skill_upsert_current(&row("a", 1, "h1")).unwrap();
        let mut retired = row("a", 1, "h1");
        retired.status = "retired".into();
        assert!(s.skill_change("stale", &retired, None, "retire").is_err());
        s.skill_change("h1", &retired, None, "retire").unwrap();
        assert_eq!(s.skill_get("a").unwrap().unwrap().status, "retired");
        let v2 = row("a", 2, "h2");
        s.skill_change("h1", &v2, Some("rescoped"), "rescope").unwrap();
        assert!(s.skill_is_reviewed("a", "h2").unwrap());
        let changed: Vec<_> =
            s.events_since(0, 100).unwrap().into_iter().filter(|e| e.kind == "skill.changed").collect();
        assert_eq!(changed.len(), 2);
        assert_eq!(changed[1].payload["action"], "rescope");
        assert_eq!(changed[1].payload["version"], 2);
    }

    #[test]
    fn proposals_cap_update_pending_fingerprints_and_refuse_rejected_ones() {
        let mut s = Store::open_in_memory().unwrap();
        let t = task(&mut s, "g");
        let first = s.skill_insert_proposal(&proposal("github.com/acme", None, Some("fp1")), CAPS).unwrap();
        let mut again = proposal("github.com/acme", None, Some("fp1"));
        again.evidence.push(EvidenceItem { task: "t2".into(), text: "x".into() });
        let second = s.skill_insert_proposal(&again, CAPS).unwrap();
        assert!(!second.new);
        assert_eq!(second.proposal.id, first.proposal.id);
        assert_eq!(second.proposal.evidence.len(), 2);
        s.skill_decide(&first.proposal.id, false, "").unwrap();
        let refused = s.skill_insert_proposal(&proposal("github.com/acme", None, Some("fp1")), CAPS);
        assert!(matches!(refused, Err(CoreError::Limit(_))), "{refused:?}");
        let small = SkillCaps { per_task: 1, per_tenant: 2 };
        s.skill_insert_proposal(&proposal("github.com/acme", Some(&t), None), small).unwrap();
        let per_task = s.skill_insert_proposal(&proposal("github.com/acme", Some(&t), None), small);
        assert!(matches!(per_task, Err(CoreError::Limit(_))), "{per_task:?}");
        s.skill_insert_proposal(&proposal("github.com/acme", None, None), small).unwrap();
        let per_tenant = s.skill_insert_proposal(&proposal("github.com/acme", None, None), small);
        assert!(matches!(per_tenant, Err(CoreError::Limit(_))), "{per_tenant:?}");
        // Another tenant has its own budget.
        s.skill_insert_proposal(&proposal("github.com/other", None, None), small).unwrap();
        let q = |tenant: &str| SkillProposalQuery {
            tenant: Some(tenant.into()),
            status: Some("pending".into()),
            limit: 100,
            ..Default::default()
        };
        assert_eq!(s.skill_proposals(&q("github.com/acme")).unwrap().len(), 2);
        assert_eq!(s.skill_proposals(&q("github.com/other")).unwrap().len(), 1);
    }

    #[test]
    fn task_origin_is_written_once() {
        let mut s = Store::open_in_memory().unwrap();
        let t = task(&mut s, "g");
        assert_eq!(s.skill_origin_of(&t).unwrap(), None);
        origin(&mut s, &t, "github.com/acme/api", "github.com/acme");
        assert!(s.skill_create_origin(&t, "/evil", "github.com/evil/x", "github.com/evil", true).is_err());
        for col in ["tenant = 'github.com/evil'", "origin = 'x'", "repo = 'x'", "observed = 'x'"] {
            assert!(s.conn.execute(&format!("UPDATE task_origins SET {col}"), []).is_err(), "{col}");
        }
        let o = s.skill_origin_of(&t).unwrap().unwrap();
        assert_eq!(
            (o.origin.as_str(), o.tenant.as_str(), o.observed),
            ("github.com/acme/api", "github.com/acme", None)
        );
    }

    #[test]
    fn the_first_person_pins_the_origin_and_a_changed_one_waits_for_confirmation() {
        let mut s = Store::open_in_memory().unwrap();
        let (a, b, c, d) = (task(&mut s, "g"), task(&mut s, "g"), task(&mut s, "g"), task(&mut s, "g"));
        // No pin and no person: unconfirmed, nothing pinned.
        let o = s.skill_create_origin(&a, "/r/.git", "github.com/acme/api", "github.com/acme", false).unwrap();
        assert_eq!(o.observed.as_deref(), Some("github.com/acme/api"));
        assert_eq!(s.skill_repo_origin("/r/.git").unwrap(), None);
        // A person's task pins it.
        let o = s.skill_create_origin(&b, "/r/.git", "github.com/acme/api", "github.com/acme", true).unwrap();
        assert_eq!(o.observed, None);
        assert_eq!(s.skill_repo_origin("/r/.git").unwrap().unwrap().1, "github.com/acme");
        // An agent rewrote origin to another client's: the task keeps the pinned tenant.
        let o = s.skill_create_origin(&c, "/r/.git", "github.com/evil/x", "github.com/evil", true).unwrap();
        assert_eq!((o.tenant.as_str(), o.observed.as_deref()), ("github.com/acme", Some("github.com/evil/x")));
        let o = s.skill_create_origin(&d, "/r/.git", "github.com/acme/api", "github.com/acme", false).unwrap();
        assert_eq!(o.observed, None, "matches the pin");
        // Confirming the pinned origin clears the tasks fixed to it (a and c), not others.
        assert_eq!(s.skill_confirm_origin("/r/.git", "github.com/acme/api", "github.com/acme").unwrap(), 2);
        assert_eq!(s.skill_origin_of(&c).unwrap().unwrap().observed, None);
        assert!(kinds(&s).contains(&"skill.origin_confirmed".to_string()));
        assert!(kinds(&s).contains(&"skill.origin_pinned".to_string()));
    }

    #[test]
    fn tenants_list_task_origins_and_learning() {
        let mut s = Store::open_in_memory().unwrap();
        let a = task(&mut s, "g");
        let b = task(&mut s, "g");
        origin(&mut s, &a, "github.com/acme/api", "github.com/acme");
        origin(&mut s, &b, "github.com/acme/web", "github.com/acme");
        s.skill_set_learning("dev.azure.com/contoso", true).unwrap();
        let t = s.skill_tenants().unwrap();
        assert_eq!(
            t,
            vec![
                TenantRow { tenant: "dev.azure.com/contoso".into(), learning: true, tasks: 0 },
                TenantRow { tenant: "github.com/acme".into(), learning: false, tasks: 2 },
            ]
        );
        assert!(s.skill_learning("dev.azure.com/contoso").unwrap());
        s.skill_set_learning("dev.azure.com/contoso", false).unwrap();
        assert!(!s.skill_learning("dev.azure.com/contoso").unwrap());
        assert!(kinds(&s).contains(&"skill.learning".to_string()));
    }

    #[test]
    fn history_query_reads_goals_messages_and_denials_of_learning_tenants_only() {
        let mut s = Store::open_in_memory().unwrap();
        let a = task(&mut s, "Rode cargo clippy antes de commitar");
        let b = task(&mut s, "other client goal");
        origin(&mut s, &a, "github.com/acme/api", "github.com/acme");
        origin(&mut s, &b, "github.com/other/x", "github.com/other");
        for t in [&a, &b] {
            s.append_event(Some(t), "message_sent", &serde_json::json!({"mode": "steer", "text": "steered"})).unwrap();
            s.append_event(Some(t), "message_queued", &serde_json::json!({"text": "queued", "position": 1})).unwrap();
            s.append_event(Some(t), "step", &serde_json::json!({"text": "not an instruction"})).unwrap();
            let c = s.create_conversation(t).unwrap();
            let sess = s.start_session(t, &c.id).unwrap();
            let p = s.create_permission_request(&sess.id, "Bash", "git push origin main").unwrap();
            s.answer_permission(&p.id, crate::model::PermissionStatus::Denied).unwrap();
            let ok = s.create_permission_request(&sess.id, "Bash", "ls").unwrap();
            s.answer_permission(&ok.id, crate::model::PermissionStatus::AllowedOnce).unwrap();
        }
        // Learning is off: nothing is read.
        assert!(s.skill_history("github.com/acme", 0).unwrap().is_empty());
        s.skill_set_learning("github.com/acme", true).unwrap();
        let h = s.skill_history("github.com/acme", 0).unwrap();
        let got: Vec<(&str, &Source)> = h.iter().map(|u| (u.text.as_str(), &u.source)).collect();
        assert_eq!(
            got,
            vec![
                ("Rode cargo clippy antes de commitar", &Source::Goal),
                ("steered", &Source::Steer),
                ("queued", &Source::Queue),
                ("git push origin main", &Source::Denied { tool: "Bash".into() }),
            ]
        );
        assert!(h.iter().all(|u| u.task == a && u.repo_key == "github.com/acme/api" && u.tenant == "github.com/acme"));
        // The window: nothing newer than the future.
        assert!(s.skill_history("github.com/acme", crate::model::now_ms() + 60_000).unwrap().is_empty());
    }

    #[test]
    fn usage_counts_by_skill_and_window() {
        let mut s = Store::open_in_memory().unwrap();
        let t = task(&mut s, "g");
        s.skill_record_session("s1", &t, &[("a".into(), 1), ("b".into(), 2)], &[("c".into(), 1, "over budget".into())])
            .unwrap();
        s.skill_record_session("s2", &t, &[("a".into(), 1)], &[]).unwrap();
        assert!(s.skill_record_used("s1", &t, "a").unwrap());
        assert!(!s.skill_record_used("s1", &t, "c").unwrap(), "refused, never injected");
        assert!(!s.skill_record_used("s9", &t, "a").unwrap(), "not injected in that session");
        let c = s.skill_usage_counts(0).unwrap();
        assert_eq!(c["a"], UsageCounts { injected: 2, used: 1, refused: 0 });
        assert_eq!(c["b"], UsageCounts { injected: 1, used: 0, refused: 0 });
        assert_eq!(c["c"], UsageCounts { injected: 0, used: 0, refused: 1 });
        assert!(s.skill_usage_counts(crate::model::now_ms() + 60_000).unwrap().is_empty());
        let by_task = s.skill_injected_by_task().unwrap();
        assert_eq!(by_task[&t], vec!["a".to_string(), "b".to_string()]);
        let ev: Vec<String> = kinds(&s).into_iter().filter(|k| k.starts_with("skill.")).collect();
        assert_eq!(ev, ["skill.injected", "skill.refused", "skill.injected", "skill.used"]);
    }
}
