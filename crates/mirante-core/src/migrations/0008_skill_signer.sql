-- The fingerprint of Mirante's skill signing key, pinned when the key is first used (made).
-- A key swapped on disk later (with its .pub and allowed_signers, all user-writable) no
-- longer matches: signing and injection refuse it. One row, never changed or removed.
CREATE TABLE skill_signer (id INTEGER PRIMARY KEY CHECK (id = 1), fingerprint TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TRIGGER skill_signer_fixed BEFORE UPDATE ON skill_signer
BEGIN SELECT RAISE(ABORT, 'the skill signing key is pinned'); END;
CREATE TRIGGER skill_signer_kept BEFORE DELETE ON skill_signer
BEGIN SELECT RAISE(ABORT, 'the skill signing key is pinned'); END;
