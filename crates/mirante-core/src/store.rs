use std::path::Path;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

use crate::error::{CoreError, Result};
use crate::model::{
    new_id, now_ms, AgentKind, Conversation, Event, Lifecycle, NewTask, PermissionRequest, PermissionStatus, Session,
    Status, Task, Turn,
};

/// Embedded, ordered schema migrations. `PRAGMA user_version` records the last applied one.
const MIGRATIONS: &[(i64, &str)] =
    &[(1, include_str!("migrations/0001_init.sql")), (2, include_str!("migrations/0002_permission_answered_once.sql"))];

/// True for a UNIQUE / PRIMARY KEY constraint violation (the DB-level backstop for locks).
fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::ConstraintViolation
                && (f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                    || f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY)
    )
}

/// Maps a UNIQUE violation to the domain error `on_unique` builds; any other error is
/// passed through as `CoreError::Sqlite`.
fn map_unique(e: rusqlite::Error, on_unique: impl FnOnce() -> CoreError) -> CoreError {
    if is_unique_violation(&e) {
        on_unique()
    } else {
        e.into()
    }
}

/// WAL is required on disk; in-memory databases legitimately report "memory".
fn check_journal_mode(mode: &str) -> Result<()> {
    match mode.to_ascii_lowercase().as_str() {
        "wal" | "memory" => Ok(()),
        other => Err(CoreError::JournalMode(other.to_string())),
    }
}

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
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        check_journal_mode(&mode)?;
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
        assert_eq!(s.schema_version().unwrap(), 2);
        drop(s);
        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), 2);
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
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let event = insert_event(&tx, &self.boot_id, task_id, kind, payload)?;
        tx.commit()?;
        Ok(event)
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

const SESSION_COLUMNS: &str = "id, task_id, conversation_id, started_at, ended_at, transcript_path, transcript_offset";

fn session_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: r.get(0)?,
        task_id: r.get(1)?,
        conversation_id: r.get(2)?,
        started_at: r.get(3)?,
        ended_at: r.get(4)?,
        transcript_path: r.get(5)?,
        transcript_offset: r.get::<_, i64>(6)? as u64,
    })
}

fn conversation_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Conversation> {
    Ok(Conversation { id: r.get(0)?, task_id: r.get(1)?, native_session_id: r.get(2)?, created_at: r.get(3)? })
}

fn turn_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Turn> {
    Ok(Turn { id: r.get(0)?, session_id: r.get(1)?, idx: r.get(2)?, started_at: r.get(3)?, ended_at: r.get(4)? })
}

fn load_session(conn: &Connection, id: &str) -> Result<Session> {
    conn.query_row(&format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1"), [id], session_from_row)
        .optional()?
        .ok_or_else(|| CoreError::NotFound { kind: "session", id: id.to_string() })
}

impl Store {
    pub fn create_conversation(&mut self, task_id: &str) -> Result<Conversation> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        load_task(&tx, task_id)?;
        let c =
            Conversation { id: new_id(), task_id: task_id.to_string(), native_session_id: None, created_at: now_ms() };
        tx.execute(
            "INSERT INTO conversations (id, task_id, native_session_id, created_at) VALUES (?1, ?2, NULL, ?3)",
            params![c.id, c.task_id, c.created_at],
        )?;
        insert_event(&tx, &self.boot_id, Some(task_id), "conversation_created", &json!({ "conversation_id": c.id }))?;
        tx.commit()?;
        Ok(c)
    }

    /// Records the agent's own session id (from `SessionStart`) so the task can `--resume`.
    pub fn set_native_session_id(&mut self, conversation_id: &str, native_session_id: &str) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let task_id: String = tx
            .query_row("SELECT task_id FROM conversations WHERE id = ?1", [conversation_id], |r| r.get(0))
            .optional()?
            .ok_or_else(|| CoreError::NotFound { kind: "conversation", id: conversation_id.to_string() })?;
        tx.execute(
            "UPDATE conversations SET native_session_id = ?2 WHERE id = ?1",
            params![conversation_id, native_session_id],
        )?;
        insert_event(
            &tx,
            &self.boot_id,
            Some(&task_id),
            "native_session_id",
            &json!({ "conversation_id": conversation_id, "native_session_id": native_session_id }),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn latest_conversation(&self, task_id: &str) -> Result<Option<Conversation>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, task_id, native_session_id, created_at FROM conversations
                 WHERE task_id = ?1 ORDER BY created_at DESC, id DESC LIMIT 1",
                [task_id],
                conversation_from_row,
            )
            .optional()?)
    }

    /// Starts a process activation. Per-task lock: fails with `TaskBusy` while another session
    /// of the task is live (also enforced by a partial unique index). A `Created` task becomes
    /// `Active`; an `Archived` task cannot start sessions.
    pub fn start_session(&mut self, task_id: &str, conversation_id: &str) -> Result<Session> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let task = load_task(&tx, task_id)?;
        let owner: String = tx
            .query_row("SELECT task_id FROM conversations WHERE id = ?1", [conversation_id], |r| r.get(0))
            .optional()?
            .ok_or_else(|| CoreError::NotFound { kind: "conversation", id: conversation_id.to_string() })?;
        if owner != task_id {
            return Err(CoreError::ConversationMismatch {
                conversation_id: conversation_id.to_string(),
                task_id: task_id.to_string(),
            });
        }
        if task.lifecycle == Lifecycle::Archived {
            return Err(CoreError::InvalidTransition { kind: "session", from: "archived".into(), to: "live".into() });
        }
        let busy: Option<String> = tx
            .query_row("SELECT id FROM sessions WHERE task_id = ?1 AND ended_at IS NULL", [task_id], |r| r.get(0))
            .optional()?;
        if busy.is_some() {
            return Err(CoreError::TaskBusy(task_id.to_string()));
        }
        let s = Session {
            id: new_id(),
            task_id: task_id.to_string(),
            conversation_id: conversation_id.to_string(),
            started_at: now_ms(),
            ended_at: None,
            transcript_path: None,
            transcript_offset: 0,
        };
        tx.execute(
            &format!("INSERT INTO sessions ({SESSION_COLUMNS}) VALUES (?1, ?2, ?3, ?4, NULL, NULL, 0)"),
            params![s.id, s.task_id, s.conversation_id, s.started_at],
        )
        .map_err(|e| map_unique(e, || CoreError::TaskBusy(task_id.to_string())))?;
        if task.lifecycle == Lifecycle::Created {
            tx.execute(
                "UPDATE tasks SET lifecycle = 'active', updated_at = ?2 WHERE id = ?1",
                params![task_id, now_ms()],
            )?;
            insert_event(
                &tx,
                &self.boot_id,
                Some(task_id),
                "lifecycle",
                &json!({ "from": "created", "to": "active" }),
            )?;
        }
        insert_event(
            &tx,
            &self.boot_id,
            Some(task_id),
            "session_started",
            &json!({ "session_id": s.id, "conversation_id": conversation_id }),
        )?;
        tx.commit()?;
        Ok(s)
    }

    /// Ends a session (releases the per-task lock) and closes any turn still open in it.
    pub fn end_session(&mut self, session_id: &str) -> Result<Session> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let s = load_session(&tx, session_id)?;
        if s.ended_at.is_none() {
            let now = now_ms();
            tx.execute("UPDATE sessions SET ended_at = ?2 WHERE id = ?1", params![session_id, now])?;
            tx.execute(
                "UPDATE turns SET ended_at = ?2 WHERE session_id = ?1 AND ended_at IS NULL",
                params![session_id, now],
            )?;
            insert_event(&tx, &self.boot_id, Some(&s.task_id), "session_ended", &json!({ "session_id": session_id }))?;
        }
        let s = load_session(&tx, session_id)?;
        tx.commit()?;
        Ok(s)
    }

    pub fn live_session(&self, task_id: &str) -> Result<Option<Session>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE task_id = ?1 AND ended_at IS NULL"),
                [task_id],
                session_from_row,
            )
            .optional()?)
    }

    pub fn get_session(&self, id: &str) -> Result<Session> {
        load_session(&self.conn, id)
    }

    /// Persists where the transcript tailer is. Not audited: it moves on every poll.
    pub fn set_transcript(&mut self, session_id: &str, path: &str, offset: u64) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n = tx.execute(
            "UPDATE sessions SET transcript_path = ?2, transcript_offset = ?3 WHERE id = ?1",
            params![session_id, path, offset as i64],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound { kind: "session", id: session_id.to_string() });
        }
        tx.commit()?;
        Ok(())
    }

    /// Opens the next turn of a live session; an ended session is `SessionEnded`.
    pub fn start_turn(&mut self, session_id: &str) -> Result<Turn> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let s = load_session(&tx, session_id)?;
        if s.ended_at.is_some() {
            return Err(CoreError::SessionEnded(session_id.to_string()));
        }
        let idx: i64 =
            tx.query_row("SELECT COALESCE(MAX(idx), 0) + 1 FROM turns WHERE session_id = ?1", [session_id], |r| {
                r.get(0)
            })?;
        let t = Turn { id: new_id(), session_id: session_id.to_string(), idx, started_at: now_ms(), ended_at: None };
        tx.execute(
            "INSERT INTO turns (id, session_id, idx, started_at, ended_at) VALUES (?1, ?2, ?3, ?4, NULL)",
            params![t.id, t.session_id, t.idx, t.started_at],
        )
        .map_err(|e| map_unique(e, || CoreError::DuplicateTurn { session_id: session_id.to_string(), idx }))?;
        insert_event(
            &tx,
            &self.boot_id,
            Some(&s.task_id),
            "turn_started",
            &json!({ "session_id": session_id, "turn_id": t.id, "idx": idx }),
        )?;
        tx.commit()?;
        Ok(t)
    }

    pub fn end_turn(&mut self, turn_id: &str) -> Result<Turn> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let t = tx
            .query_row(
                "SELECT id, session_id, idx, started_at, ended_at FROM turns WHERE id = ?1",
                [turn_id],
                turn_from_row,
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound { kind: "turn", id: turn_id.to_string() })?;
        if t.ended_at.is_none() {
            let task_id = load_session(&tx, &t.session_id)?.task_id;
            tx.execute("UPDATE turns SET ended_at = ?2 WHERE id = ?1", params![turn_id, now_ms()])?;
            insert_event(
                &tx,
                &self.boot_id,
                Some(&task_id),
                "turn_ended",
                &json!({ "turn_id": turn_id, "idx": t.idx }),
            )?;
        }
        let t = tx.query_row(
            "SELECT id, session_id, idx, started_at, ended_at FROM turns WHERE id = ?1",
            [turn_id],
            turn_from_row,
        )?;
        tx.commit()?;
        Ok(t)
    }

    pub fn turns(&self, session_id: &str) -> Result<Vec<Turn>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, idx, started_at, ended_at FROM turns WHERE session_id = ?1 ORDER BY idx",
        )?;
        let rows = stmt.query_map([session_id], turn_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;
    use crate::test_support::TempDir;

    fn task_and_conversation(s: &mut Store) -> (Task, Conversation) {
        let t = s.create_task(sample_task("feat-a")).unwrap();
        let c = s.create_conversation(&t.id).unwrap();
        (t, c)
    }

    #[test]
    fn native_session_id_survives_reopen() {
        let dir = TempDir::new("store-native");
        let path = dir.path().join("m.sqlite");
        let mut s = Store::open(&path).unwrap();
        let (t, c) = task_and_conversation(&mut s);
        s.set_native_session_id(&c.id, "claude-sess-1").unwrap();
        drop(s);
        let s = Store::open(&path).unwrap();
        let latest = s.latest_conversation(&t.id).unwrap().unwrap();
        assert_eq!(latest.native_session_id.as_deref(), Some("claude-sess-1"));
    }

    #[test]
    fn per_task_lock_allows_one_live_session() {
        let mut s = Store::open_in_memory().unwrap();
        let (t, c) = task_and_conversation(&mut s);
        let first = s.start_session(&t.id, &c.id).unwrap();
        assert!(matches!(s.start_session(&t.id, &c.id), Err(CoreError::TaskBusy(id)) if id == t.id));
        assert_eq!(s.live_session(&t.id).unwrap().unwrap().id, first.id);
        s.end_session(&first.id).unwrap();
        assert!(s.live_session(&t.id).unwrap().is_none());
        let second = s.start_session(&t.id, &c.id).unwrap();
        assert_ne!(second.id, first.id);
    }

    #[test]
    fn unique_index_backs_the_lock_even_without_the_check() {
        let mut s = Store::open_in_memory().unwrap();
        let (t, c) = task_and_conversation(&mut s);
        s.start_session(&t.id, &c.id).unwrap();
        let raw = s.conn.execute(
            "INSERT INTO sessions (id, task_id, conversation_id, started_at) VALUES ('x', ?1, ?2, 0)",
            params![t.id, c.id],
        );
        assert!(raw.is_err());
    }

    #[test]
    fn first_session_activates_the_task_and_archived_tasks_cannot_start() {
        let mut s = Store::open_in_memory().unwrap();
        let (t, c) = task_and_conversation(&mut s);
        let sess = s.start_session(&t.id, &c.id).unwrap();
        assert_eq!(s.get_task(&t.id).unwrap().lifecycle, Lifecycle::Active);
        s.end_session(&sess.id).unwrap();
        s.transition_lifecycle(&t.id, Lifecycle::Archived).unwrap();
        let err = s.start_session(&t.id, &c.id).unwrap_err();
        assert!(matches!(err, CoreError::InvalidTransition { kind: "session", .. }), "{err}");
    }

    #[test]
    fn turns_are_numbered_and_closed_by_end_session() {
        let mut s = Store::open_in_memory().unwrap();
        let (t, c) = task_and_conversation(&mut s);
        let sess = s.start_session(&t.id, &c.id).unwrap();
        let t1 = s.start_turn(&sess.id).unwrap();
        assert!(s.end_turn(&t1.id).unwrap().ended_at.is_some());
        let t2 = s.start_turn(&sess.id).unwrap();
        assert_eq!((t1.idx, t2.idx), (1, 2));
        s.end_session(&sess.id).unwrap();
        assert!(s.turns(&sess.id).unwrap().iter().all(|t| t.ended_at.is_some()));
        let kinds: Vec<String> = s.task_events(&t.id).unwrap().into_iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                "task_created",
                "conversation_created",
                "lifecycle",
                "session_started",
                "turn_started",
                "turn_ended",
                "turn_started",
                "session_ended"
            ]
        );
    }

    #[test]
    fn transcript_offset_persists() {
        let mut s = Store::open_in_memory().unwrap();
        let (t, c) = task_and_conversation(&mut s);
        let sess = s.start_session(&t.id, &c.id).unwrap();
        s.set_transcript(&sess.id, "/home/u/.claude/projects/x/s.jsonl", 4096).unwrap();
        let got = s.get_session(&sess.id).unwrap();
        assert_eq!(got.transcript_path.as_deref(), Some("/home/u/.claude/projects/x/s.jsonl"));
        assert_eq!(got.transcript_offset, 4096);
        assert!(matches!(s.set_transcript("nope", "/x", 0), Err(CoreError::NotFound { .. })));
    }
}

const PERMISSION_COLUMNS: &str = "id, task_id, session_id, tool, detail, status, created_at, answered_at";

fn permission_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PermissionRequest> {
    Ok(PermissionRequest {
        id: r.get(0)?,
        task_id: r.get(1)?,
        session_id: r.get(2)?,
        tool: r.get(3)?,
        detail: r.get(4)?,
        status: r.get(5)?,
        created_at: r.get(6)?,
        answered_at: r.get(7)?,
    })
}

impl Store {
    pub fn create_permission_request(
        &mut self,
        session_id: &str,
        tool: &str,
        detail: &str,
    ) -> Result<PermissionRequest> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let s = load_session(&tx, session_id)?;
        if s.ended_at.is_some() {
            return Err(CoreError::SessionEnded(session_id.to_string()));
        }
        let p = PermissionRequest {
            id: new_id(),
            task_id: s.task_id.clone(),
            session_id: session_id.to_string(),
            tool: tool.to_string(),
            detail: detail.to_string(),
            status: PermissionStatus::Pending,
            created_at: now_ms(),
            answered_at: None,
        };
        tx.execute(
            &format!(
                "INSERT INTO permission_requests ({PERMISSION_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)"
            ),
            params![p.id, p.task_id, p.session_id, p.tool, p.detail, p.status, p.created_at],
        )?;
        insert_event(
            &tx,
            &self.boot_id,
            Some(&p.task_id),
            "permission_requested",
            &json!({ "permission_id": p.id, "tool": tool, "detail": detail }),
        )?;
        tx.commit()?;
        Ok(p)
    }

    /// Answers a pending request exactly once. `answer` must be `AllowedOnce` or `Denied`;
    /// there is no "always allow".
    pub fn answer_permission(&mut self, id: &str, answer: PermissionStatus) -> Result<PermissionRequest> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let p = tx
            .query_row(
                &format!("SELECT {PERMISSION_COLUMNS} FROM permission_requests WHERE id = ?1"),
                [id],
                permission_from_row,
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound { kind: "permission_request", id: id.to_string() })?;
        let already = || CoreError::InvalidTransition {
            kind: "permission",
            from: p.status.as_str().into(),
            to: answer.as_str().into(),
        };
        if p.status != PermissionStatus::Pending || answer == PermissionStatus::Pending {
            return Err(already());
        }
        // Compare-and-set at the DB level: only a still-pending row can be answered.
        let n = tx.execute(
            "UPDATE permission_requests SET status = ?2, answered_at = ?3 WHERE id = ?1 AND status = 'pending'",
            params![id, answer, now_ms()],
        )?;
        if n == 0 {
            return Err(already());
        }
        insert_event(
            &tx,
            &self.boot_id,
            Some(&p.task_id),
            "permission_answered",
            &json!({ "permission_id": id, "answer": answer }),
        )?;
        let p = tx.query_row(
            &format!("SELECT {PERMISSION_COLUMNS} FROM permission_requests WHERE id = ?1"),
            [id],
            permission_from_row,
        )?;
        tx.commit()?;
        Ok(p)
    }

    pub fn pending_permissions(&self, task_id: &str) -> Result<Vec<PermissionRequest>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {PERMISSION_COLUMNS} FROM permission_requests WHERE task_id = ?1 AND status = 'pending' ORDER BY created_at, id"
        ))?;
        let rows = stmt.query_map([task_id], permission_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod permission_tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn pending_request_survives_restart_and_is_answered_once() {
        let dir = TempDir::new("store-perm");
        let path = dir.path().join("m.sqlite");
        let mut s = Store::open(&path).unwrap();
        let t = s.create_task(sample_task("feat-a")).unwrap();
        let c = s.create_conversation(&t.id).unwrap();
        let sess = s.start_session(&t.id, &c.id).unwrap();
        let p = s.create_permission_request(&sess.id, "Bash", "touch a.txt").unwrap();
        drop(s);

        let mut s = Store::open(&path).unwrap();
        let pending = s.pending_permissions(&t.id).unwrap();
        assert_eq!(pending, vec![p.clone()]);
        let answered = s.answer_permission(&p.id, PermissionStatus::Denied).unwrap();
        assert_eq!(answered.status, PermissionStatus::Denied);
        assert!(answered.answered_at.is_some());
        assert!(s.pending_permissions(&t.id).unwrap().is_empty());
        let again = s.answer_permission(&p.id, PermissionStatus::AllowedOnce).unwrap_err();
        assert!(matches!(again, CoreError::InvalidTransition { kind: "permission", .. }), "{again}");
    }

    #[test]
    fn pending_is_not_an_answer() {
        let mut s = Store::open_in_memory().unwrap();
        let t = s.create_task(sample_task("feat-a")).unwrap();
        let c = s.create_conversation(&t.id).unwrap();
        let sess = s.start_session(&t.id, &c.id).unwrap();
        let p = s.create_permission_request(&sess.id, "Edit", "src/a.rs").unwrap();
        assert!(s.answer_permission(&p.id, PermissionStatus::Pending).is_err());
        assert_eq!(
            s.answer_permission(&p.id, PermissionStatus::AllowedOnce).unwrap().status,
            PermissionStatus::AllowedOnce
        );
    }
}

#[cfg(test)]
mod hardening_tests {
    use super::*;

    fn setup(s: &mut Store) -> (Task, Conversation, Session) {
        let t = s.create_task(sample_task("feat-a")).unwrap();
        let c = s.create_conversation(&t.id).unwrap();
        let sess = s.start_session(&t.id, &c.id).unwrap();
        (t, c, sess)
    }

    #[test]
    fn unique_violation_is_detected() {
        let s = Store::open_in_memory().unwrap();
        s.conn.execute_batch("CREATE TABLE u (a TEXT UNIQUE); INSERT INTO u VALUES ('x');").unwrap();
        let e = s.conn.execute("INSERT INTO u VALUES ('x')", []).unwrap_err();
        assert!(is_unique_violation(&e));
        let e = s.conn.execute("INSERT INTO nope VALUES (1)", []).unwrap_err();
        assert!(!is_unique_violation(&e));
    }

    #[test]
    fn duplicate_turn_index_maps_to_a_clear_error() {
        let mut s = Store::open_in_memory().unwrap();
        let (_, _, sess) = setup(&mut s);
        let t1 = s.start_turn(&sess.id).unwrap();
        let dup = || CoreError::DuplicateTurn { session_id: sess.id.clone(), idx: t1.idx };
        let e = s
            .conn
            .execute(
                "INSERT INTO turns (id, session_id, idx, started_at) VALUES ('dup', ?1, ?2, 0)",
                params![sess.id, t1.idx],
            )
            .unwrap_err();
        let mapped = map_unique(e, dup);
        assert!(matches!(mapped, CoreError::DuplicateTurn { idx, .. } if idx == t1.idx), "{mapped}");
        // Any other error passes through unchanged.
        let other = s.conn.execute("INSERT INTO nope VALUES (1)", []).unwrap_err();
        assert!(matches!(map_unique(other, dup), CoreError::Sqlite(_)));
    }

    #[test]
    fn start_turn_rejects_an_ended_session() {
        let mut s = Store::open_in_memory().unwrap();
        let (_, _, sess) = setup(&mut s);
        s.end_session(&sess.id).unwrap();
        let err = s.start_turn(&sess.id).unwrap_err();
        assert!(matches!(err, CoreError::SessionEnded(ref id) if *id == sess.id), "{err}");
        assert!(s.turns(&sess.id).unwrap().is_empty());
    }

    /// A database created before migration 0002 (user_version 1) gets the answered-once
    /// trigger on open, and it guards rows answered before the upgrade too.
    #[test]
    fn v1_database_upgrades_and_the_trigger_guards_old_answers() {
        let dir = crate::test_support::TempDir::new("store-v1");
        let path = dir.path().join("m.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATIONS[0].1).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            conn.execute_batch(
                "INSERT INTO tasks VALUES ('t', 'x', '/r', '/w', 'b', 'claude', '', '', 'active', 'idle', 0, 0);
                 INSERT INTO conversations VALUES ('c', 't', NULL, 0);
                 INSERT INTO sessions VALUES ('s', 't', 'c', 0, NULL, NULL, 0);
                 INSERT INTO permission_requests VALUES ('p', 't', 's', 'Bash', 'ls', 'denied', 0, 1);",
            )
            .unwrap();
            // Before the upgrade nothing stops a re-answer.
            conn.execute("UPDATE permission_requests SET answered_at = 2 WHERE id = 'p'", []).unwrap();
        }
        let mut s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), MIGRATIONS.last().unwrap().0);
        let raw = s.conn.execute("UPDATE permission_requests SET status = 'allowed_once' WHERE id = 'p'", []);
        assert!(raw.is_err(), "trigger must block re-answering a pre-upgrade row");
        let err = s.answer_permission("p", PermissionStatus::AllowedOnce).unwrap_err();
        assert!(matches!(err, CoreError::InvalidTransition { kind: "permission", .. }), "{err}");
    }

    #[test]
    fn start_session_rejects_a_foreign_conversation() {
        let mut s = Store::open_in_memory().unwrap();
        let a = s.create_task(sample_task("feat-a")).unwrap();
        let b = s.create_task(sample_task("feat-b")).unwrap();
        let cb = s.create_conversation(&b.id).unwrap();
        let err = s.start_session(&a.id, &cb.id).unwrap_err();
        assert!(matches!(err, CoreError::ConversationMismatch { .. }), "{err}");
        let err = s.start_session(&a.id, "missing").unwrap_err();
        assert!(matches!(err, CoreError::NotFound { kind: "conversation", .. }), "{err}");
        assert!(s.live_session(&a.id).unwrap().is_none());
    }

    #[test]
    fn permission_request_requires_a_live_session() {
        let mut s = Store::open_in_memory().unwrap();
        let (_, _, sess) = setup(&mut s);
        s.end_session(&sess.id).unwrap();
        let err = s.create_permission_request(&sess.id, "Bash", "ls").unwrap_err();
        assert!(matches!(err, CoreError::SessionEnded(_)), "{err}");
    }

    #[test]
    fn db_refuses_to_change_an_answered_permission() {
        let mut s = Store::open_in_memory().unwrap();
        let (_, _, sess) = setup(&mut s);
        let p = s.create_permission_request(&sess.id, "Bash", "ls").unwrap();
        s.answer_permission(&p.id, PermissionStatus::Denied).unwrap();
        let raw = s.conn.execute("UPDATE permission_requests SET status = 'allowed_once' WHERE id = ?1", [&p.id]);
        assert!(raw.is_err(), "trigger must block re-answering");
        let status: String =
            s.conn.query_row("SELECT status FROM permission_requests WHERE id = ?1", [&p.id], |r| r.get(0)).unwrap();
        assert_eq!(status, "denied");
    }

    #[test]
    fn journal_mode_check() {
        check_journal_mode("wal").unwrap();
        check_journal_mode("memory").unwrap();
        assert!(matches!(check_journal_mode("delete"), Err(CoreError::JournalMode(m)) if m == "delete"));
    }

    #[test]
    fn append_event_and_set_transcript_commit() {
        let mut s = Store::open_in_memory().unwrap();
        let (_, _, sess) = setup(&mut s);
        let e = s.append_event(None, "x", &json!({})).unwrap();
        assert_eq!(s.events_since(e.seq - 1, 10).unwrap().len(), 1);
        s.set_transcript(&sess.id, "/p", 7).unwrap();
        assert_eq!(s.get_session(&sess.id).unwrap().transcript_offset, 7);
    }
}
