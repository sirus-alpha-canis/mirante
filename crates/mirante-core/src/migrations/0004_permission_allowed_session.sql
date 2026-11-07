-- Adds the 'allowed_session' answer: the user allowed a request that grants something for
-- the rest of the session (Codex `grantRoot`: write access under a directory), recorded as
-- what it is rather than as a one-time allow. SQLite cannot alter a CHECK constraint: the
-- table is rebuilt (nothing refers to it), then its index and the answered-once trigger
-- (dropped with the old table) are created again. Runs with foreign keys off (see `migrate`).
CREATE TABLE permission_requests_new (
    id          TEXT PRIMARY KEY,
    task_id     TEXT NOT NULL REFERENCES tasks(id),
    session_id  TEXT NOT NULL REFERENCES sessions(id),
    tool        TEXT NOT NULL,
    detail      TEXT NOT NULL,
    status      TEXT NOT NULL CHECK (status IN ('pending', 'allowed_once', 'allowed_session', 'denied')),
    created_at  INTEGER NOT NULL,
    answered_at INTEGER
);
INSERT INTO permission_requests_new (id, task_id, session_id, tool, detail, status, created_at, answered_at)
    SELECT id, task_id, session_id, tool, detail, status, created_at, answered_at FROM permission_requests;
DROP TABLE permission_requests;
ALTER TABLE permission_requests_new RENAME TO permission_requests;
CREATE INDEX permission_requests_task ON permission_requests(task_id, status);
CREATE TRIGGER permission_requests_answered_once BEFORE UPDATE ON permission_requests
WHEN OLD.status != 'pending'
BEGIN SELECT RAISE(ABORT, 'permission request already answered'); END;
