CREATE TABLE tasks (
    id            TEXT PRIMARY KEY,
    title         TEXT NOT NULL,
    repo_path     TEXT NOT NULL,
    worktree_path TEXT NOT NULL,
    branch        TEXT NOT NULL,
    agent         TEXT NOT NULL CHECK (agent IN ('claude', 'codex')),
    goal          TEXT NOT NULL DEFAULT '',
    notes         TEXT NOT NULL DEFAULT '',
    lifecycle     TEXT NOT NULL CHECK (lifecycle IN ('created', 'active', 'archived')),
    status        TEXT NOT NULL CHECK (status IN ('idle', 'working', 'needs_you', 'ended')),
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);

CREATE TABLE conversations (
    id                TEXT PRIMARY KEY,
    task_id           TEXT NOT NULL REFERENCES tasks(id),
    native_session_id TEXT,
    created_at        INTEGER NOT NULL
);
CREATE INDEX conversations_task ON conversations(task_id, created_at);

CREATE TABLE sessions (
    id                TEXT PRIMARY KEY,
    task_id           TEXT NOT NULL REFERENCES tasks(id),
    conversation_id   TEXT NOT NULL REFERENCES conversations(id),
    started_at        INTEGER NOT NULL,
    ended_at          INTEGER,
    transcript_path   TEXT,
    transcript_offset INTEGER NOT NULL DEFAULT 0
);
-- Per-task lock: at most one live (not ended) session per task.
CREATE UNIQUE INDEX sessions_one_live_per_task ON sessions(task_id) WHERE ended_at IS NULL;

CREATE TABLE turns (
    id         TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    idx        INTEGER NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at   INTEGER,
    UNIQUE (session_id, idx)
);

CREATE TABLE permission_requests (
    id          TEXT PRIMARY KEY,
    task_id     TEXT NOT NULL REFERENCES tasks(id),
    session_id  TEXT NOT NULL REFERENCES sessions(id),
    tool        TEXT NOT NULL,
    detail      TEXT NOT NULL,
    status      TEXT NOT NULL CHECK (status IN ('pending', 'allowed_once', 'denied')),
    created_at  INTEGER NOT NULL,
    answered_at INTEGER
);
CREATE INDEX permission_requests_task ON permission_requests(task_id, status);

-- Append-only audit trail. AUTOINCREMENT: seq is never reused, even after deletes.
CREATE TABLE events (
    seq     INTEGER PRIMARY KEY AUTOINCREMENT,
    boot_id TEXT NOT NULL,
    task_id TEXT,
    kind    TEXT NOT NULL,
    payload TEXT NOT NULL,
    ts_ms   INTEGER NOT NULL
);
CREATE INDEX events_task ON events(task_id, seq);
CREATE TRIGGER events_no_update BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
CREATE TRIGGER events_no_delete BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
