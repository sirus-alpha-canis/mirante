use std::path::Path;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, TransactionBehavior};

use crate::error::Result;
use crate::model::{new_id, AgentKind, Lifecycle, PermissionStatus, Status};

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
