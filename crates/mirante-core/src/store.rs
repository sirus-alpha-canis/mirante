use std::path::Path;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

use crate::error::{CoreError, Result};
use crate::model::{new_id, now_ms, AgentKind, Event, Lifecycle, NewTask, PermissionStatus, Status, Task};

/// Embedded, ordered schema migrations. `PRAGMA user_version` records the last applied one.
const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("migrations/0001_init.sql"))];

macro_rules! sql_text_enum {
    ($($t:ty),+) => {$(
        impl ToSql for $t {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.as_str()))
            }
        }
        impl FromSql for $t {
            fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
                <$t>::parse(v.as_str()?).map_err(|e| FromSqlError::Other(Box::new(e)))
            }
        }
    )+};
}
sql_text_enum!(Status, Lifecycle, AgentKind, PermissionStatus);

/// Single owner of Mirante's durable state. Every mutation runs in one transaction that
/// also appends its audit event, so state and trail never diverge.
pub struct Store {
    conn: Connection,
    boot_id: String,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        Store::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Store> {
        Store::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Store> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // Returns the resulting mode ("wal" on disk, "memory" for in-memory databases).
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&mut conn)?;
        Ok(Store { conn, boot_id: new_id() })
    }

    /// Identifies this opening of the store (one daemon boot). Stamped on every event.
    pub fn boot_id(&self) -> &str {
        &self.boot_id
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    pub fn journal_mode(&self) -> Result<String> {
        Ok(self.conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?)
    }
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v > current) {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn migrates_to_latest_version_once() {
        let dir = TempDir::new("store-migrate");
        let path = dir.path().join("db/mirante.sqlite");
        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), 1);
        drop(s);
        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), 1);
    }

    #[test]
    fn file_database_uses_wal_and_foreign_keys() {
        let dir = TempDir::new("store-wal");
        let s = Store::open(&dir.path().join("m.sqlite")).unwrap();
        assert_eq!(s.journal_mode().unwrap(), "wal");
        let fk: i64 = s.conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn each_opening_gets_a_new_boot_id() {
        let a = Store::open_in_memory().unwrap();
        let b = Store::open_in_memory().unwrap();
        assert_ne!(a.boot_id(), b.boot_id());
    }
}

fn insert_event(conn: &Connection, boot_id: &str, task_id: Option<&str>, kind: &str, payload: &Value) -> Result<Event> {
    let ts_ms = now_ms();
    conn.execute(
        "INSERT INTO events (boot_id, task_id, kind, payload, ts_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![boot_id, task_id, kind, payload.to_string(), ts_ms],
    )?;
    Ok(Event {
        seq: conn.last_insert_rowid() as u64,
        boot_id: boot_id.to_string(),
        task_id: task_id.map(str::to_string),
        kind: kind.to_string(),
        payload: payload.clone(),
        ts_ms,
    })
}

const TASK_COLUMNS: &str =
    "id, title, repo_path, worktree_path, branch, agent, goal, notes, lifecycle, status, created_at, updated_at";

fn task_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: r.get(0)?,
        title: r.get(1)?,
        repo_path: r.get(2)?,
        worktree_path: r.get(3)?,
        branch: r.get(4)?,
        agent: r.get(5)?,
        goal: r.get(6)?,
        notes: r.get(7)?,
        lifecycle: r.get(8)?,
        status: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

fn event_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let payload: String = r.get(4)?;
    Ok(Event {
        seq: r.get::<_, i64>(0)? as u64,
        boot_id: r.get(1)?,
        task_id: r.get(2)?,
        kind: r.get(3)?,
        payload: serde_json::from_str(&payload)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e)))?,
        ts_ms: r.get(5)?,
    })
}

fn load_task(conn: &Connection, id: &str) -> Result<Task> {
    conn.query_row(&format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"), [id], task_from_row)
        .optional()?
        .ok_or_else(|| CoreError::NotFound { kind: "task", id: id.to_string() })
}

impl Store {
    pub fn create_task(&mut self, new: NewTask) -> Result<Task> {
        let now = now_ms();
        let task = Task {
            id: new_id(),
            title: new.title,
            repo_path: new.repo_path,
            worktree_path: new.worktree_path,
            branch: new.branch,
            agent: new.agent,
            goal: new.goal,
            notes: String::new(),
            lifecycle: Lifecycle::Created,
            status: Status::Idle,
            created_at: now,
            updated_at: now,
        };
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            &format!("INSERT INTO tasks ({TASK_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"),
            params![
                task.id,
                task.title,
                task.repo_path,
                task.worktree_path,
                task.branch,
                task.agent,
                task.goal,
                task.notes,
                task.lifecycle,
                task.status,
                task.created_at,
                task.updated_at
            ],
        )?;
        insert_event(&tx, &self.boot_id, Some(&task.id), "task_created", &serde_json::to_value(&task)?)?;
        tx.commit()?;
        Ok(task)
    }

    pub fn get_task(&self, id: &str) -> Result<Task> {
        load_task(&self.conn, id)
    }

    /// Tasks ordered by creation time; archived ones only when asked.
    pub fn list_tasks(&self, include_archived: bool) -> Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {TASK_COLUMNS} FROM tasks WHERE ?1 OR lifecycle != 'archived' ORDER BY created_at, id"
        ))?;
        let rows = stmt.query_map([include_archived], task_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn update_goal_notes(&mut self, id: &str, goal: &str, notes: &str) -> Result<Task> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        load_task(&tx, id)?;
        tx.execute(
            "UPDATE tasks SET goal = ?2, notes = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, goal, notes, now_ms()],
        )?;
        insert_event(&tx, &self.boot_id, Some(id), "task_updated", &json!({ "goal": goal, "notes": notes }))?;
        let task = load_task(&tx, id)?;
        tx.commit()?;
        Ok(task)
    }

    /// Validated lifecycle move (see `Lifecycle::can_transition_to`), persisted with its event.
    pub fn transition_lifecycle(&mut self, id: &str, to: Lifecycle) -> Result<Task> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let from = load_task(&tx, id)?.lifecycle;
        if !from.can_transition_to(to) {
            return Err(CoreError::InvalidTransition {
                kind: "lifecycle",
                from: from.as_str().into(),
                to: to.as_str().into(),
            });
        }
        tx.execute("UPDATE tasks SET lifecycle = ?2, updated_at = ?3 WHERE id = ?1", params![id, to, now_ms()])?;
        insert_event(&tx, &self.boot_id, Some(id), "lifecycle", &json!({ "from": from, "to": to }))?;
        let task = load_task(&tx, id)?;
        tx.commit()?;
        Ok(task)
    }

    /// Compare-and-set of the task status. Fails with `StaleStatus` when the stored status
    /// is not `from`, and with `InvalidTransition` when `from -> to` is not allowed.
    pub fn set_status(&mut self, id: &str, from: Status, to: Status, cause: &str) -> Result<Event> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actual = load_task(&tx, id)?.status;
        if actual != from {
            return Err(CoreError::StaleStatus {
                task_id: id.to_string(),
                expected: from.as_str().into(),
                actual: actual.as_str().into(),
            });
        }
        if !from.can_transition_to(to) {
            return Err(CoreError::InvalidTransition {
                kind: "status",
                from: from.as_str().into(),
                to: to.as_str().into(),
            });
        }
        tx.execute("UPDATE tasks SET status = ?2, updated_at = ?3 WHERE id = ?1", params![id, to, now_ms()])?;
        let event =
            insert_event(&tx, &self.boot_id, Some(id), "status", &json!({ "from": from, "to": to, "cause": cause }))?;
        tx.commit()?;
        Ok(event)
    }

    /// Appends a free-form event (e.g. a normalized agent step) to the audit trail.
    pub fn append_event(&mut self, task_id: Option<&str>, kind: &str, payload: &Value) -> Result<Event> {
        insert_event(&self.conn, &self.boot_id, task_id, kind, payload)
    }

    /// Events with `seq > after_seq`, oldest first, at most `limit`. Clients reconcile from
    /// a snapshot plus this; the live stream is only a hint.
    pub fn events_since(&self, after_seq: u64, limit: usize) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, boot_id, task_id, kind, payload, ts_ms FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![after_seq as i64, limit as i64], event_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn task_events(&self, task_id: &str) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, boot_id, task_id, kind, payload, ts_ms FROM events WHERE task_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([task_id], event_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
fn sample_task(title: &str) -> NewTask {
    NewTask {
        title: title.to_string(),
        repo_path: "/repos/app".into(),
        worktree_path: format!("/wt/app/{title}"),
        branch: title.to_string(),
        agent: AgentKind::Claude,
        goal: "ship it".into(),
    }
}

#[cfg(test)]
mod task_tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn create_get_and_list_tasks() {
        let mut s = Store::open_in_memory().unwrap();
        let a = s.create_task(sample_task("feat-a")).unwrap();
        let b = s.create_task(sample_task("feat-b")).unwrap();
        assert_eq!(a.lifecycle, Lifecycle::Created);
        assert_eq!(a.status, Status::Idle);
        assert_eq!(s.get_task(&a.id).unwrap(), a);
        let ids: Vec<String> = s.list_tasks(false).unwrap().into_iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![a.id.clone(), b.id.clone()]);
        assert!(matches!(s.get_task("nope"), Err(CoreError::NotFound { kind: "task", .. })));
    }

    #[test]
    fn archived_tasks_are_hidden_unless_asked() {
        let mut s = Store::open_in_memory().unwrap();
        let a = s.create_task(sample_task("feat-a")).unwrap();
        s.transition_lifecycle(&a.id, Lifecycle::Archived).unwrap();
        assert!(s.list_tasks(false).unwrap().is_empty());
        assert_eq!(s.list_tasks(true).unwrap().len(), 1);
    }

    #[test]
    fn lifecycle_transitions_are_validated_and_audited() {
        let mut s = Store::open_in_memory().unwrap();
        let t = s.create_task(sample_task("feat-a")).unwrap();
        assert_eq!(s.transition_lifecycle(&t.id, Lifecycle::Active).unwrap().lifecycle, Lifecycle::Active);
        let err = s.transition_lifecycle(&t.id, Lifecycle::Created).unwrap_err();
        assert!(matches!(err, CoreError::InvalidTransition { kind: "lifecycle", .. }), "{err}");
        let kinds: Vec<String> = s.task_events(&t.id).unwrap().into_iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec!["task_created", "lifecycle"]);
    }

    #[test]
    fn set_status_is_compare_and_set_and_validated() {
        let mut s = Store::open_in_memory().unwrap();
        let t = s.create_task(sample_task("feat-a")).unwrap();
        let e = s.set_status(&t.id, Status::Idle, Status::Working, "hook:UserPromptSubmit").unwrap();
        assert_eq!(e.kind, "status");
        assert_eq!(e.payload, json!({"from": "idle", "to": "working", "cause": "hook:UserPromptSubmit"}));
        assert_eq!(s.get_task(&t.id).unwrap().status, Status::Working);
        let stale = s.set_status(&t.id, Status::Idle, Status::NeedsYou, "x").unwrap_err();
        assert!(matches!(stale, CoreError::StaleStatus { .. }), "{stale}");
        s.set_status(&t.id, Status::Working, Status::Ended, "hook:SessionEnd").unwrap();
        let bad = s.set_status(&t.id, Status::Ended, Status::Working, "x").unwrap_err();
        assert!(matches!(bad, CoreError::InvalidTransition { kind: "status", .. }), "{bad}");
        assert_eq!(s.get_task(&t.id).unwrap().status, Status::Ended);
    }

    #[test]
    fn goal_and_notes_persist() {
        let mut s = Store::open_in_memory().unwrap();
        let t = s.create_task(sample_task("feat-a")).unwrap();
        let t = s.update_goal_notes(&t.id, "new goal", "remember X").unwrap();
        assert_eq!((t.goal.as_str(), t.notes.as_str()), ("new goal", "remember X"));
    }

    #[test]
    fn events_are_monotonic_across_reopen_and_carry_boot_id() {
        let dir = TempDir::new("store-events");
        let path = dir.path().join("m.sqlite");
        let mut s1 = Store::open(&path).unwrap();
        let boot1 = s1.boot_id().to_string();
        let t = s1.create_task(sample_task("feat-a")).unwrap();
        let first = s1.append_event(Some(&t.id), "step", &json!({"tool": "Bash"})).unwrap();
        drop(s1);
        let mut s2 = Store::open(&path).unwrap();
        let second = s2.append_event(Some(&t.id), "step", &json!({"tool": "Edit"})).unwrap();
        assert!(second.seq > first.seq);
        assert_ne!(second.boot_id, boot1);
        let since = s2.events_since(first.seq, 10).unwrap();
        assert_eq!(since.len(), 1);
        assert_eq!(since[0], second);
        assert_eq!(s2.events_since(0, 1).unwrap().len(), 1);
        assert_eq!(s2.events_since(0, 1).unwrap()[0].boot_id, boot1);
    }

    #[test]
    fn events_table_is_append_only() {
        let mut s = Store::open_in_memory().unwrap();
        s.append_event(None, "boot", &json!({})).unwrap();
        assert!(s.conn.execute("UPDATE events SET kind = 'x'", []).is_err());
        assert!(s.conn.execute("DELETE FROM events", []).is_err());
    }
}
