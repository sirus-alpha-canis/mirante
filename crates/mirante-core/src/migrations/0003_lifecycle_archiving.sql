-- Adds the 'archiving' lifecycle (set before a worktree is removed, so recovery can finish
-- or undo an archive interrupted by a crash). SQLite cannot alter a CHECK constraint: the
-- table is rebuilt. Runs with foreign keys off (see `migrate`); the child tables refer to
-- `tasks` by name, so they point at the rebuilt table.
CREATE TABLE tasks_new (
    id            TEXT PRIMARY KEY,
    title         TEXT NOT NULL,
    repo_path     TEXT NOT NULL,
    worktree_path TEXT NOT NULL,
    branch        TEXT NOT NULL,
    agent         TEXT NOT NULL CHECK (agent IN ('claude', 'codex')),
    goal          TEXT NOT NULL DEFAULT '',
    notes         TEXT NOT NULL DEFAULT '',
    lifecycle     TEXT NOT NULL CHECK (lifecycle IN ('created', 'active', 'archiving', 'archived')),
    status        TEXT NOT NULL CHECK (status IN ('idle', 'working', 'needs_you', 'ended')),
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);
INSERT INTO tasks_new (id, title, repo_path, worktree_path, branch, agent, goal, notes, lifecycle, status, created_at, updated_at)
    SELECT id, title, repo_path, worktree_path, branch, agent, goal, notes, lifecycle, status, created_at, updated_at
    FROM tasks;
DROP TABLE tasks;
ALTER TABLE tasks_new RENAME TO tasks;
