-- Threads (Batch D): a piece of work bound to one object (a PR, an issue, a branch, or a
-- message of another conversation) is its own task: its own worktree, agent session and
-- context window. Its link says what it is bound to; its notes (the existing, unused
-- `tasks.notes`) are the person's context for it, delivered in the conversation (at the
-- first prompt, the first message after a resume, and the next message after they change).
-- `parent_id` (migration 11, one level deep) is the conversation it came from, if any.
CREATE TABLE task_links (
    task_id    TEXT PRIMARY KEY REFERENCES tasks(id),
    kind       TEXT NOT NULL CHECK (kind IN ('pr', 'issue', 'branch', 'message')),
    ref        TEXT NOT NULL,
    url        TEXT,
    title      TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX task_links_ref ON task_links(kind, ref);
-- The hash of the notes and link last handed to the agent (NULL: never).
ALTER TABLE tasks ADD COLUMN notes_sent_hash TEXT;
-- The thread's newest summary: deterministic (from its last answer), or the person's own
-- CLI on demand. Agent-derived text: shown as text, never as markup.
ALTER TABLE tasks ADD COLUMN summary TEXT;
ALTER TABLE tasks ADD COLUMN summary_at INTEGER;
ALTER TABLE tasks ADD COLUMN summary_by TEXT CHECK (summary_by IN ('deterministic', 'claude'));
