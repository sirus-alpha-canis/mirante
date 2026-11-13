-- Agent-initiated fan-out (Batch E, `mirante-orchestrator`). A task's agent may propose
-- child tasks; a person approves each one (founder decision 1, 2026-11-12: no budget in
-- v1, every proposal is a card). An approved child is a real task under the proposing
-- task (`parent_id`, one level deep, migration 11), like a thread (migration 12).
--
-- What a task with a parent is: a thread (bound to an object, made by a person) or a child
-- an agent proposed and a person approved ('spawn'). Set exactly when `parent_id` is, and
-- fixed at creation. Every child made before this migration is a thread.
ALTER TABLE tasks ADD COLUMN child_kind TEXT CHECK (child_kind IN ('thread', 'spawn'));
UPDATE tasks SET child_kind = 'thread' WHERE parent_id IS NOT NULL;
CREATE TRIGGER tasks_child_kind_fixed BEFORE UPDATE OF child_kind ON tasks
WHEN OLD.child_kind IS NOT NEW.child_kind
BEGIN SELECT RAISE(ABORT, 'a child''s kind is fixed at creation'); END;
CREATE TRIGGER tasks_child_kind BEFORE INSERT ON tasks
WHEN (NEW.parent_id IS NULL) IS NOT (NEW.child_kind IS NULL)
BEGIN SELECT RAISE(ABORT, 'a child task has a kind, and only then'); END;
-- Orchestration: per task, off unless a person turned it on. While on, the task's next
-- sessions get the `mirante-orchestrator` MCP server. (A later version may add a column
-- here for children created without asking; v1 has none.)
CREATE TABLE task_orchestration (
    task_id    TEXT PRIMARY KEY REFERENCES tasks(id),
    enabled    INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    updated_at INTEGER NOT NULL
);
-- What an agent proposed, as it proposed it (redacted, capped): fixed once written. A
-- person decides once (accepted: the child it made; rejected; withdrawn: its parent was
-- archived first). The person's edits live on the child and in the decision's event.
CREATE TABLE spawn_proposals (
    id         TEXT PRIMARY KEY,
    parent_id  TEXT NOT NULL REFERENCES tasks(id),
    status     TEXT NOT NULL CHECK (status IN ('pending', 'accepted', 'rejected', 'withdrawn')),
    goal       TEXT NOT NULL,
    agent      TEXT NOT NULL CHECK (agent IN ('claude', 'codex')),
    title      TEXT,
    why        TEXT,
    child_id   TEXT REFERENCES tasks(id),
    created_at INTEGER NOT NULL,
    decided_at INTEGER
);
CREATE INDEX spawn_pending ON spawn_proposals(parent_id, status);
CREATE TRIGGER spawn_proposals_fixed BEFORE UPDATE OF id, parent_id, goal, agent, title, why, created_at
ON spawn_proposals
BEGIN SELECT RAISE(ABORT, 'a proposal is fixed as the agent wrote it'); END;
CREATE TRIGGER spawn_proposals_decided_once BEFORE UPDATE OF status, child_id, decided_at ON spawn_proposals
WHEN OLD.status != 'pending'
BEGIN SELECT RAISE(ABORT, 'a proposal is decided once'); END;
