-- Workspace tasks: one task across several repositories of a folder (a "group"). The task
-- row keeps the primary repo (the first selected) in repo_path / worktree_path so every
-- single-repo reader keeps working; the agent runs in workspace_path. Both columns are NULL
-- for a single-repo task.
ALTER TABLE tasks ADD COLUMN workspace_path TEXT;
ALTER TABLE tasks ADD COLUMN group_path TEXT;
-- Each repository a workspace task may change, in the order the person selected them (idx
-- 0 is the primary). Fixed at creation and kept after archive, like the branches.
CREATE TABLE task_repos (
    task_id        TEXT NOT NULL REFERENCES tasks(id),
    idx            INTEGER NOT NULL CHECK (idx >= 0),
    name           TEXT NOT NULL,
    repo_path      TEXT NOT NULL,
    worktree_path  TEXT NOT NULL,
    branch         TEXT NOT NULL,
    created_branch INTEGER NOT NULL DEFAULT 0 CHECK (created_branch IN (0, 1)),
    head           TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (task_id, idx),
    UNIQUE (task_id, name)
);
CREATE TRIGGER task_repos_fixed BEFORE UPDATE ON task_repos
BEGIN SELECT RAISE(ABORT, 'a task''s repos are fixed at creation'); END;
