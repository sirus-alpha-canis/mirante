//! The skill signing key's fingerprint, pinned in SQLite (migration 8).
//!
//! The key, its `.pub` and `allowed_signers` live in the Mirante home and are writable by
//! anything running as the user; swapped together they are consistent on disk. The
//! fingerprint pinned at the key's first use is what signing and verification compare
//! against ([`crate::skills::library::read_verified`] takes it).
use rusqlite::{params, OptionalExtension};

use crate::error::Result;
use crate::model::now_ms;
use crate::store::Store;

impl Store {
    /// The pinned fingerprint of the skill signing key, if any.
    pub fn skill_signer_key(&self) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT fingerprint FROM skill_signer WHERE id = 1", [], |r| r.get(0)).optional()?)
    }

    /// Pins `fingerprint` unless a key is pinned already; returns the pinned one (compare it
    /// with `fingerprint`).
    pub fn skill_pin_signer(&mut self, fingerprint: &str) -> Result<String> {
        self.conn.execute(
            "INSERT OR IGNORE INTO skill_signer (id, fingerprint, created_at) VALUES (1, ?1, ?2)",
            params![fingerprint, now_ms()],
        )?;
        Ok(self.conn.query_row("SELECT fingerprint FROM skill_signer WHERE id = 1", [], |r| r.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use crate::store::Store;

    #[test]
    fn the_first_key_is_pinned_for_good() {
        let mut s = Store::open_in_memory().unwrap();
        assert_eq!(s.skill_signer_key().unwrap(), None);
        assert_eq!(s.skill_pin_signer("SHA256:one").unwrap(), "SHA256:one");
        assert_eq!(s.skill_pin_signer("SHA256:two").unwrap(), "SHA256:one", "a later key never replaces it");
        assert_eq!(s.skill_signer_key().unwrap().as_deref(), Some("SHA256:one"));
        assert!(s.conn.execute("UPDATE skill_signer SET fingerprint = 'SHA256:two'", []).is_err());
        assert!(s.conn.execute("DELETE FROM skill_signer", []).is_err());
    }
}
