-- Fan-out groups (founder decision 2, 2026-11-12): the person's "same goal on several
-- agents" or "one child per bullet" is a lightweight group, not a task. A group has no
-- worktree and no session: it holds the shared goal, names its children's branches
-- (<branch>--<n>-<slug>) and is archived with them, all or nothing.
CREATE TABLE task_groups (
    id          TEXT PRIMARY KEY,
    kind        TEXT NOT NULL CHECK (kind IN ('fanout', 'compare')),
    title       TEXT NOT NULL,
    goal        TEXT NOT NULL,
    branch      TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    archived_at INTEGER
);
CREATE TRIGGER task_groups_fixed BEFORE UPDATE OF id, kind, title, goal, branch, created_at ON task_groups
BEGIN SELECT RAISE(ABORT, 'a group is fixed at creation'); END;
-- A task's group (and its role in it: one of the children, or the task that synthesizes
-- their results) is fixed at creation.
ALTER TABLE tasks ADD COLUMN group_id TEXT REFERENCES task_groups(id);
ALTER TABLE tasks ADD COLUMN group_role TEXT CHECK (group_role IN ('child', 'synth'));
-- Agent-initiated children (a later batch): a real parent task, one level deep. Unused yet.
ALTER TABLE tasks ADD COLUMN parent_id TEXT REFERENCES tasks(id);
CREATE INDEX tasks_group ON tasks(group_id) WHERE group_id IS NOT NULL;
CREATE INDEX tasks_parent ON tasks(parent_id) WHERE parent_id IS NOT NULL;
CREATE TRIGGER tasks_group_fixed BEFORE UPDATE OF group_id, group_role ON tasks
WHEN OLD.group_id IS NOT NEW.group_id OR OLD.group_role IS NOT NEW.group_role
BEGIN SELECT RAISE(ABORT, 'a task''s group is fixed at creation'); END;
CREATE TRIGGER tasks_group_role BEFORE INSERT ON tasks
WHEN (NEW.group_id IS NULL) IS NOT (NEW.group_role IS NULL)
BEGIN SELECT RAISE(ABORT, 'a task in a group has a role, and only then'); END;
CREATE TRIGGER tasks_parent_fixed BEFORE UPDATE OF parent_id ON tasks
WHEN OLD.parent_id IS NOT NEW.parent_id
BEGIN SELECT RAISE(ABORT, 'a task''s parent is fixed at creation'); END;
CREATE TRIGGER tasks_depth_one BEFORE INSERT ON tasks
WHEN NEW.parent_id IS NOT NULL AND (SELECT parent_id FROM tasks WHERE id = NEW.parent_id) IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'a child task cannot have children'); END;
