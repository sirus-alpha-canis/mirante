//! Agent-initiated fan-out (Batch E, `mirante-orchestrator`): a task's agent proposes child
//! tasks, and a person decides each one (founder decision 1, 2026-11-12: no budget in v1;
//! every proposal is a card). Orchestration is opt-in per task and only a person turns it
//! on or off. An approved child is a real task under its parent (`parent_id`, one level
//! deep, `child_kind = 'spawn'`): its own worktree, agent session and permissions (manual
//! mode, a person answers). It inherits its parent's origin (and tenant), copied in the
//! transaction that makes it, never recomputed from a checkout an agent could have changed.
//! Its goal was written by an agent, so it is never a person's for mining (`by_person = 0`),
//! even after a person approved it.
//!
//! The store's rows (`task_orchestration`, `spawn_proposals`) and the checks on what an
//! agent proposes live here.
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{CoreError, Result};
use crate::model::{new_id, now_ms, AgentKind, Lifecycle, Task};
use crate::store::{insert_event, load_task, task_from_row, Store, TASK_COLUMNS};

/// Most children one `task.spawn_propose` call proposes.
pub const MAX_PER_CALL: usize = 5;
/// Most proposals waiting for a person under one task.
pub const MAX_PENDING: usize = 10;
/// Most approved children (not archived) one task has.
pub const MAX_OPEN_CHILDREN: usize = 8;
/// Longest goal kept (UTF-8 bytes, after redaction).
pub const MAX_GOAL: usize = 4096;
/// Longest title kept (characters).
pub const MAX_TITLE: usize = 120;
/// Longest reason kept (characters).
pub const MAX_WHY: usize = 500;

/// Where a proposal stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    /// Waiting for a person.
    Pending,
    /// A person approved it: `child_id` is the task it made.
    Accepted,
    Rejected,
    /// Its parent was archived while it waited.
    Withdrawn,
}

impl ProposalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ProposalStatus::Pending => "pending",
            ProposalStatus::Accepted => "accepted",
            ProposalStatus::Rejected => "rejected",
            ProposalStatus::Withdrawn => "withdrawn",
        }
    }

    pub fn parse(s: &str) -> Result<ProposalStatus> {
        match s {
            "pending" => Ok(ProposalStatus::Pending),
            "accepted" => Ok(ProposalStatus::Accepted),
            "rejected" => Ok(ProposalStatus::Rejected),
            "withdrawn" => Ok(ProposalStatus::Withdrawn),
            other => Err(CoreError::BadEnum { kind: "proposal_status", value: other.to_string() }),
        }
    }
}

/// A child task an agent proposed, as it proposed it (redacted, capped).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnProposal {
    pub id: String,
    /// The task whose agent proposed it.
    pub parent_id: String,
    pub status: ProposalStatus,
    /// Agent text.
    pub goal: String,
    pub agent: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Why the agent wants it (agent text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// The child an approval made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_id: Option<String>,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<i64>,
}

/// A proposal as it is stored: checked by [`check_proposal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProposal {
    pub goal: String,
    pub agent: AgentKind,
    pub title: Option<String>,
    pub why: Option<String>,
}

/// An approved proposal becoming a task (inside the task insert's transaction).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSpawn {
    pub parent: String,
    pub proposal: String,
    /// What the person changed before approving (`goal`, `agent`, `title`), for the record.
    pub edits: serde_json::Value,
}

/// A value on one line (control characters and line breaks become spaces), trimmed.
fn one_line(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().to_string()
}

/// One line, credentials masked, at most `max` characters; `None` when empty.
fn short(s: Option<&str>, max: usize) -> Option<String> {
    let s = crate::redact::redact(&one_line(s?));
    let s: String = s.chars().take(max).collect();
    (!s.is_empty()).then_some(s)
}

/// A goal as it is kept: control characters (but line breaks and tabs) removed, credentials
/// masked, at most [`MAX_GOAL`] bytes after that.
pub fn check_goal(goal: &str) -> std::result::Result<String, String> {
    let clean: String = goal.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').collect();
    let clean = crate::redact::redact(clean.trim());
    if clean.is_empty() {
        return Err("the goal is empty".into());
    }
    if clean.len() > MAX_GOAL {
        return Err(format!("the goal is {} bytes; at most {MAX_GOAL} are kept", clean.len()));
    }
    Ok(clean)
}

/// What an agent proposed, checked: the goal ([`check_goal`]), the title and reason on one
/// line each (masked, capped).
pub fn check_proposal(
    goal: &str,
    agent: AgentKind,
    title: Option<&str>,
    why: Option<&str>,
) -> std::result::Result<NewProposal, String> {
    Ok(NewProposal { goal: check_goal(goal)?, agent, title: short(title, MAX_TITLE), why: short(why, MAX_WHY) })
}

const PROPOSAL_COLUMNS: &str = "id, parent_id, status, goal, agent, title, why, child_id, created_at, decided_at";

fn proposal_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SpawnProposal> {
    let status: String = r.get(2)?;
    Ok(SpawnProposal {
        id: r.get(0)?,
        parent_id: r.get(1)?,
        status: ProposalStatus::parse(&status)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e)))?,
        goal: r.get(3)?,
        agent: r.get(4)?,
        title: r.get(5)?,
        why: r.get(6)?,
        child_id: r.get(7)?,
        created_at: r.get(8)?,
        decided_at: r.get(9)?,
    })
}

fn load_proposal(conn: &Connection, id: &str) -> Result<SpawnProposal> {
    conn.query_row(&format!("SELECT {PROPOSAL_COLUMNS} FROM spawn_proposals WHERE id = ?1"), [id], proposal_from_row)
        .optional()?
        .ok_or_else(|| CoreError::NotFound { kind: "spawn proposal", id: id.to_string() })
}

fn enabled(conn: &Connection, task: &str) -> Result<bool> {
    Ok(conn
        .query_row("SELECT enabled FROM task_orchestration WHERE task_id = ?1", [task], |r| r.get::<_, i64>(0))
        .optional()?
        == Some(1))
}

fn count(conn: &Connection, sql: &str, task: &str) -> Result<usize> {
    Ok(conn.query_row(sql, [task], |r| r.get::<_, i64>(0))? as usize)
}

/// Inside a task insert's transaction: the proposal is still pending under a parent that
/// can take a child, which has room. Called before the child's row is inserted.
pub(crate) fn check_spawn(tx: &Connection, spawn: &NewSpawn) -> Result<()> {
    let p = load_proposal(tx, &spawn.proposal)?;
    if p.parent_id != spawn.parent {
        return Err(CoreError::Limit("that proposal belongs to another task".into()));
    }
    if p.status != ProposalStatus::Pending {
        return Err(CoreError::Limit(format!("that proposal was already decided ({})", p.status.as_str())));
    }
    let parent = load_task(tx, &spawn.parent)?;
    if matches!(parent.lifecycle, Lifecycle::Archiving | Lifecycle::Archived) {
        return Err(CoreError::Limit(format!("task {} is archived: it takes no new children", parent.branch)));
    }
    let open = count(
        tx,
        "SELECT COUNT(*) FROM tasks WHERE parent_id = ?1 AND child_kind = 'spawn' AND lifecycle != 'archived'",
        &spawn.parent,
    )?;
    if open >= MAX_OPEN_CHILDREN {
        return Err(CoreError::Limit(format!(
            "task {} already has {MAX_OPEN_CHILDREN} children (the most one task runs): archive one first",
            parent.branch
        )));
    }
    Ok(())
}

/// Inside a task insert's transaction, after the child's row: the proposal is accepted
/// with it, the child inherits the parent's origin row (`by_person = 0`: an agent wrote the
/// goal), and `child_created` / `spawn_decided` land on the parent.
pub(crate) fn finish_spawn(tx: &Connection, boot_id: &str, child: &Task, spawn: &NewSpawn) -> Result<()> {
    let now = now_ms();
    tx.execute(
        "UPDATE spawn_proposals SET status = 'accepted', child_id = ?2, decided_at = ?3 WHERE id = ?1",
        params![spawn.proposal, child.id, now],
    )?;
    tx.execute(
        "INSERT INTO task_origins (task_id, repo, origin, tenant, observed, created_at, by_person) \
         SELECT ?1, repo, origin, tenant, observed, ?2, 0 FROM task_origins WHERE task_id = ?3",
        params![child.id, now, spawn.parent],
    )?;
    let created = json!({
        "child": child.id, "title": child.title, "branch": child.branch, "agent": child.agent,
        "by": "agent", "via": "review", "proposal": spawn.proposal,
    });
    insert_event(tx, boot_id, Some(&spawn.parent), "child_created", &created)?;
    let decided = json!({ "id": spawn.proposal, "accepted": true, "child": child.id, "edits": spawn.edits });
    insert_event(tx, boot_id, Some(&spawn.parent), "spawn_decided", &decided)?;
    Ok(())
}

impl Store {
    /// Whether `task`'s agent gets the orchestrator (off unless a person turned it on).
    pub fn orchestration(&self, task: &str) -> Result<bool> {
        enabled(&self.conn, task)
    }

    /// A person turns orchestration on or off for `task` (`orchestration` event). Refused
    /// for a child (one level deep) and an archived task.
    pub fn set_orchestration(&mut self, task: &str, on: bool) -> Result<bool> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let t = load_task(&tx, task)?;
        if t.parent_id.is_some() {
            return Err(CoreError::Limit(format!(
                "task {} is a child task: children cannot have children (one level deep)",
                t.branch
            )));
        }
        if on && matches!(t.lifecycle, Lifecycle::Archiving | Lifecycle::Archived) {
            return Err(CoreError::Limit(format!("task {} is archived", t.branch)));
        }
        tx.execute(
            "INSERT INTO task_orchestration (task_id, enabled, updated_at) VALUES (?1, ?2, ?3) \
             ON CONFLICT (task_id) DO UPDATE SET enabled = ?2, updated_at = ?3",
            params![task, on as i64, now_ms()],
        )?;
        insert_event(&tx, &self.boot_id, Some(task), "orchestration", &json!({ "enabled": on }))?;
        tx.commit()?;
        Ok(on)
    }

    /// Proposals from `parent`'s agent, waiting for a person, in one transaction: refused
    /// unless orchestration is on, the parent is no child and not archived, and the
    /// proposals fit the caps ([`MAX_PER_CALL`], [`MAX_PENDING`]). `spawn_proposed` lands on
    /// the parent.
    pub fn insert_proposals(&mut self, parent: &str, list: &[NewProposal]) -> Result<Vec<SpawnProposal>> {
        if list.is_empty() || list.len() > MAX_PER_CALL {
            return Err(CoreError::Limit(format!("propose 1 to {MAX_PER_CALL} children at a time")));
        }
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let t = load_task(&tx, parent)?;
        if !enabled(&tx, parent)? {
            return Err(CoreError::Limit(format!(
                "orchestration is off for task {}: only the person can turn it on",
                t.branch
            )));
        }
        if t.parent_id.is_some() {
            return Err(CoreError::Limit("a child task cannot propose children (one level deep)".into()));
        }
        if matches!(t.lifecycle, Lifecycle::Archiving | Lifecycle::Archived) {
            return Err(CoreError::Limit(format!("task {} is archived", t.branch)));
        }
        let pending =
            count(&tx, "SELECT COUNT(*) FROM spawn_proposals WHERE parent_id = ?1 AND status = 'pending'", parent)?;
        if pending + list.len() > MAX_PENDING {
            return Err(CoreError::Limit(format!(
                "{pending} proposals already wait for the person (at most {MAX_PENDING}): wait for their decision"
            )));
        }
        let now = now_ms();
        let mut out = Vec::with_capacity(list.len());
        for p in list {
            let row = SpawnProposal {
                id: new_id(),
                parent_id: parent.to_string(),
                status: ProposalStatus::Pending,
                goal: p.goal.clone(),
                agent: p.agent,
                title: p.title.clone(),
                why: p.why.clone(),
                child_id: None,
                created_at: now,
                decided_at: None,
            };
            tx.execute(
                &format!(
                    "INSERT INTO spawn_proposals ({PROPOSAL_COLUMNS}) VALUES (?1, ?2, 'pending', ?3, ?4, ?5, ?6, \
                     NULL, ?7, NULL)"
                ),
                params![row.id, row.parent_id, row.goal, row.agent, row.title, row.why, row.created_at],
            )?;
            out.push(row);
        }
        let ids: Vec<&str> = out.iter().map(|p| p.id.as_str()).collect();
        insert_event(&tx, &self.boot_id, Some(parent), "spawn_proposed", &json!({ "ids": ids, "n": ids.len() }))?;
        tx.commit()?;
        Ok(out)
    }

    pub fn proposal(&self, id: &str) -> Result<SpawnProposal> {
        load_proposal(&self.conn, id)
    }

    /// Proposals, newest last: of one parent (or every task's), of one status (or any).
    pub fn proposals(&self, parent: Option<&str>, status: Option<ProposalStatus>) -> Result<Vec<SpawnProposal>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {PROPOSAL_COLUMNS} FROM spawn_proposals WHERE (?1 IS NULL OR parent_id = ?1) \
             AND (?2 IS NULL OR status = ?2) ORDER BY created_at, id"
        ))?;
        let rows = stmt.query_map(params![parent, status.map(ProposalStatus::as_str)], proposal_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// How many proposals wait for a person under `parent`.
    pub fn pending_proposals(&self, parent: &str) -> Result<usize> {
        count(&self.conn, "SELECT COUNT(*) FROM spawn_proposals WHERE parent_id = ?1 AND status = 'pending'", parent)
    }

    /// A person rejects a pending proposal (`spawn_decided` on its parent).
    pub fn reject_proposal(&mut self, id: &str) -> Result<SpawnProposal> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let p = load_proposal(&tx, id)?;
        if p.status != ProposalStatus::Pending {
            return Err(CoreError::Limit(format!("that proposal was already decided ({})", p.status.as_str())));
        }
        tx.execute(
            "UPDATE spawn_proposals SET status = 'rejected', decided_at = ?2 WHERE id = ?1",
            params![id, now_ms()],
        )?;
        insert_event(&tx, &self.boot_id, Some(&p.parent_id), "spawn_decided", &json!({ "id": id, "accepted": false }))?;
        let p = load_proposal(&tx, id)?;
        tx.commit()?;
        Ok(p)
    }

    /// `parent` was archived: what waited under it is withdrawn (`spawn_withdrawn`, when
    /// there was any). Returns how many.
    pub fn withdraw_proposals(&mut self, parent: &str) -> Result<usize> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n = tx.execute(
            "UPDATE spawn_proposals SET status = 'withdrawn', decided_at = ?2 WHERE parent_id = ?1 AND status = 'pending'",
            params![parent, now_ms()],
        )?;
        if n > 0 {
            insert_event(&tx, &self.boot_id, Some(parent), "spawn_withdrawn", &json!({ "n": n }))?;
        }
        tx.commit()?;
        Ok(n)
    }

    /// The children `parent`'s agent proposed and a person approved: open ones first, then
    /// by creation.
    pub fn spawned_children(&self, parent: &str) -> Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {TASK_COLUMNS} FROM tasks WHERE parent_id = ?1 AND child_kind = 'spawn' \
             ORDER BY lifecycle = 'archived', created_at, id"
        ))?;
        let rows = stmt.query_map([parent], task_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every child of `parent` (threads and approved children), by creation.
    pub fn children_of(&self, parent: &str) -> Result<Vec<Task>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {TASK_COLUMNS} FROM tasks WHERE parent_id = ?1 ORDER BY created_at, id"))?;
        let rows = stmt.query_map([parent], task_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChildKind, NewTask, Status};

    fn task(s: &mut Store, branch: &str) -> Task {
        s.create_task(NewTask {
            title: branch.into(),
            repo_path: "/repos/app".into(),
            worktree_path: format!("/wt/app/{branch}"),
            branch: branch.into(),
            agent: AgentKind::Claude,
            goal: "ship it".into(),
        })
        .unwrap()
    }

    fn proposal(goal: &str) -> NewProposal {
        check_proposal(goal, AgentKind::Codex, Some("Add tests"), Some("coverage")).unwrap()
    }

    fn kinds(s: &Store, task: &str) -> Vec<String> {
        s.task_events(task).unwrap().into_iter().map(|e| e.kind).collect()
    }

    fn child(s: &mut Store, parent: &Task, proposal: &str, branch: &str) -> Result<Task> {
        let spawn = NewSpawn { parent: parent.id.clone(), proposal: proposal.into(), edits: json!({}) };
        s.create_spawned_task(
            NewTask {
                title: "child".into(),
                repo_path: "/repos/app".into(),
                worktree_path: format!("/wt/app/{branch}"),
                branch: branch.into(),
                agent: AgentKind::Codex,
                goal: "do it".into(),
            },
            None,
            spawn,
        )
    }

    #[test]
    fn orchestration_is_off_until_a_person_turns_it_on_and_never_for_a_child() {
        let mut s = Store::open_in_memory().unwrap();
        let t = task(&mut s, "p");
        assert!(!s.orchestration(&t.id).unwrap());
        assert!(s.set_orchestration(&t.id, true).unwrap());
        assert!(s.orchestration(&t.id).unwrap());
        s.set_orchestration(&t.id, false).unwrap();
        assert!(!s.orchestration(&t.id).unwrap());
        assert_eq!(kinds(&s, &t.id).iter().filter(|k| *k == "orchestration").count(), 2);
        s.set_orchestration(&t.id, true).unwrap();
        let p = s.insert_proposals(&t.id, &[proposal("write tests")]).unwrap();
        let c = child(&mut s, &t, &p[0].id, "p--1-x").unwrap();
        let e = s.set_orchestration(&c.id, true).unwrap_err();
        assert!(e.to_string().contains("one level deep"), "{e}");
        assert!(matches!(s.set_orchestration("nope", true), Err(CoreError::NotFound { .. })));
    }

    #[test]
    fn proposals_need_orchestration_and_fit_the_caps() {
        let mut s = Store::open_in_memory().unwrap();
        let t = task(&mut s, "p");
        let e = s.insert_proposals(&t.id, &[proposal("a")]).unwrap_err();
        assert!(e.to_string().contains("orchestration is off"), "{e}");
        s.set_orchestration(&t.id, true).unwrap();
        let six: Vec<NewProposal> = (0..6).map(|i| proposal(&format!("g{i}"))).collect();
        assert!(s.insert_proposals(&t.id, &six).is_err());
        assert!(s.insert_proposals(&t.id, &[]).is_err());
        s.insert_proposals(&t.id, &six[..5]).unwrap();
        s.insert_proposals(&t.id, &six[..5]).unwrap();
        assert_eq!(s.pending_proposals(&t.id).unwrap(), MAX_PENDING);
        let e = s.insert_proposals(&t.id, &six[..1]).unwrap_err();
        assert!(e.to_string().contains("wait for the person"), "{e}");
        assert_eq!(kinds(&s, &t.id).iter().filter(|k| *k == "spawn_proposed").count(), 2);
        assert_eq!(s.proposals(Some(&t.id), Some(ProposalStatus::Pending)).unwrap().len(), 10);
        assert!(s.proposals(None, Some(ProposalStatus::Accepted)).unwrap().is_empty());
    }

    #[test]
    fn proposals_round_trip_are_fixed_and_decided_once() {
        let mut s = Store::open_in_memory().unwrap();
        let t = task(&mut s, "p");
        s.set_orchestration(&t.id, true).unwrap();
        let p = s.insert_proposals(&t.id, &[proposal("one"), proposal("two")]).unwrap();
        assert_eq!(s.proposal(&p[0].id).unwrap(), p[0]);
        assert_eq!(
            (p[0].agent, p[0].title.as_deref(), p[0].why.as_deref()),
            (AgentKind::Codex, Some("Add tests"), Some("coverage"))
        );
        // What the agent wrote is fixed.
        assert!(s.conn.execute("UPDATE spawn_proposals SET goal = 'x' WHERE id = ?1", [&p[0].id]).is_err());
        let r = s.reject_proposal(&p[0].id).unwrap();
        assert_eq!(r.status, ProposalStatus::Rejected);
        assert!(r.decided_at.is_some());
        assert!(s.reject_proposal(&p[0].id).is_err());
        assert!(child(&mut s, &t, &p[0].id, "p--1-a").is_err(), "a rejected proposal makes no child");
        // The database refuses a second decision too.
        assert!(s.conn.execute("UPDATE spawn_proposals SET status = 'accepted' WHERE id = ?1", [&p[0].id]).is_err());
        let c = child(&mut s, &t, &p[1].id, "p--1-b").unwrap();
        assert!(child(&mut s, &t, &p[1].id, "p--2-b").is_err(), "accepted once");
        let p1 = s.proposal(&p[1].id).unwrap();
        assert_eq!((p1.status, p1.child_id.as_deref()), (ProposalStatus::Accepted, Some(c.id.as_str())));
    }

    #[test]
    fn an_approved_child_is_a_spawn_under_its_parent_with_its_origin_and_never_by_person() {
        let mut s = Store::open_in_memory().unwrap();
        let t = task(&mut s, "p");
        s.skill_create_origin(&t.id, "/repos/app/.git", "github.com/acme/app", "github.com/acme", true).unwrap();
        s.set_orchestration(&t.id, true).unwrap();
        let p = s.insert_proposals(&t.id, &[proposal("one")]).unwrap();
        let c = child(&mut s, &t, &p[0].id, "p--1-one").unwrap();
        assert_eq!((c.parent_id.as_deref(), c.child_kind), (Some(t.id.as_str()), Some(ChildKind::Spawn)));
        assert_eq!(s.get_task(&c.id).unwrap(), c);
        let o = s.skill_origin_of(&c.id).unwrap().unwrap();
        assert_eq!(
            (o.origin.as_str(), o.tenant.as_str(), o.observed),
            ("github.com/acme/app", "github.com/acme", None)
        );
        let by: i64 =
            s.conn.query_row("SELECT by_person FROM task_origins WHERE task_id = ?1", [&c.id], |r| r.get(0)).unwrap();
        assert_eq!(by, 0);
        let parent_events = s.task_events(&t.id).unwrap();
        let created = parent_events.iter().find(|e| e.kind == "child_created").unwrap();
        assert_eq!((created.payload["by"].as_str(), created.payload["via"].as_str()), (Some("agent"), Some("review")));
        assert!(parent_events.iter().any(|e| e.kind == "spawn_decided" && e.payload["accepted"] == true));
        assert_eq!(s.spawned_children(&t.id).unwrap(), std::slice::from_ref(&c));
        assert_eq!(s.children_of(&t.id).unwrap(), std::slice::from_ref(&c));
        // Depth one: the child cannot be a parent.
        assert!(s.insert_proposals(&c.id, &[proposal("x")]).is_err());
        // A parent with no origin row gives its child none.
        let bare = task(&mut s, "bare");
        s.set_orchestration(&bare.id, true).unwrap();
        let q = s.insert_proposals(&bare.id, &[proposal("one")]).unwrap();
        let c2 = child(&mut s, &bare, &q[0].id, "bare--1-one").unwrap();
        assert_eq!(s.skill_origin_of(&c2.id).unwrap(), None);
    }

    #[test]
    fn a_parent_runs_at_most_eight_open_children_and_none_once_archived() {
        let mut s = Store::open_in_memory().unwrap();
        let t = task(&mut s, "p");
        s.set_orchestration(&t.id, true).unwrap();
        let mut made = Vec::new();
        for round in 0..2 {
            let ps = s
                .insert_proposals(&t.id, &(0..5).map(|i| proposal(&format!("{round}{i}"))).collect::<Vec<_>>())
                .unwrap();
            for p in ps {
                made.push((p.id.clone(), child(&mut s, &t, &p.id, &format!("p--{round}{}", made.len()))));
            }
        }
        assert_eq!(made.iter().filter(|(_, r)| r.is_ok()).count(), MAX_OPEN_CHILDREN);
        let refused = made.iter().find_map(|(_, r)| r.as_ref().err()).unwrap();
        assert!(refused.to_string().contains("archive one first"), "{refused}");
        // Archiving one makes room.
        let first = made[0].1.as_ref().unwrap().id.clone();
        s.transition_lifecycle(&first, Lifecycle::Archiving).unwrap();
        s.transition_lifecycle(&first, Lifecycle::Archived).unwrap();
        let (left, _) = made.iter().find(|(_, r)| r.is_err()).unwrap();
        child(&mut s, &t, left, "p--again").unwrap();
        // An archived parent: what waits is withdrawn, nothing more is made.
        let wait = s.proposals(Some(&t.id), Some(ProposalStatus::Pending)).unwrap();
        assert_eq!(wait.len(), 1);
        s.transition_lifecycle(&t.id, Lifecycle::Archiving).unwrap();
        assert!(child(&mut s, &t, &wait[0].id, "p--late").unwrap_err().to_string().contains("archived"));
        s.transition_lifecycle(&t.id, Lifecycle::Archived).unwrap();
        assert_eq!(s.withdraw_proposals(&t.id).unwrap(), 1);
        assert_eq!(s.proposal(&wait[0].id).unwrap().status, ProposalStatus::Withdrawn);
        assert!(kinds(&s, &t.id).contains(&"spawn_withdrawn".to_string()));
        assert!(s.insert_proposals(&t.id, &[proposal("x")]).is_err());
        assert_eq!(s.get_task(&t.id).unwrap().status, Status::Idle);
    }

    #[test]
    fn goals_are_masked_capped_and_titles_one_line() {
        let p = check_proposal(
            "use key sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\nthen\u{7}test",
            AgentKind::Claude,
            Some("  a\ntitle  "),
            Some(""),
        )
        .unwrap();
        assert!(!p.goal.contains("sk-ant-api03-AAAA"), "{}", p.goal);
        assert!(p.goal.contains("then") && !p.goal.contains('\u{7}'), "{}", p.goal);
        assert_eq!((p.title.as_deref(), p.why), (Some("a title"), None));
        assert!(check_goal(&"x".repeat(MAX_GOAL)).is_ok());
        assert!(check_goal(&"x".repeat(MAX_GOAL + 1)).unwrap_err().contains("at most"));
        assert!(check_goal("  \n ").is_err());
        let long = check_proposal("g", AgentKind::Claude, Some(&"t".repeat(500)), Some(&"w".repeat(900))).unwrap();
        assert_eq!((long.title.unwrap().chars().count(), long.why.unwrap().chars().count()), (MAX_TITLE, MAX_WHY));
    }

    #[test]
    fn migration_13_marks_older_children_as_threads() {
        let dir = crate::test_support::TempDir::new("store-v12");
        let path = dir.path().join("m.sqlite");
        {
            let mut conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "foreign_keys", false).unwrap();
            for (v, sql) in crate::store::MIGRATIONS.iter().filter(|(v, _)| *v <= 12) {
                let tx = conn.transaction().unwrap();
                tx.execute_batch(sql).unwrap();
                tx.pragma_update(None, "user_version", v).unwrap();
                tx.commit().unwrap();
            }
            for (id, parent) in [("p", None), ("c", Some("p"))] {
                conn.execute(
                    "INSERT INTO tasks (id, title, repo_path, worktree_path, branch, agent, goal, notes, lifecycle, \
                     status, created_at, updated_at, parent_id) VALUES (?1, 'T', '/r', '/w', ?1, 'claude', 'g', '', \
                     'active', 'idle', 1, 1, ?2)",
                    params![id, parent],
                )
                .unwrap();
            }
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.get_task("c").unwrap().child_kind, Some(ChildKind::Thread));
        assert_eq!(s.get_task("p").unwrap().child_kind, None);
        assert!(!s.orchestration("p").unwrap());
        // Fixed, and only with a parent.
        assert!(s.conn.execute("UPDATE tasks SET child_kind = 'spawn' WHERE id = 'c'", []).is_err());
        assert!(s
            .conn
            .execute(
                "INSERT INTO tasks (id, title, repo_path, worktree_path, branch, agent, goal, notes, lifecycle, status, \
                 created_at, updated_at, child_kind) VALUES ('x', 'T', '/r', '/w', 'x', 'claude', 'g', '', 'active', \
                 'idle', 1, 1, 'spawn')",
                [],
            )
            .is_err());
    }
}
