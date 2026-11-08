-- Who created each task: 1 for a person, 0 for anyone else (a Mirante-launched agent, a
-- caller that could not be checked) and for rows from before this column (unknown). Mining
-- reads only a person's tasks' goals. Fixed at creation like the rest of the row.
ALTER TABLE task_origins ADD COLUMN by_person INTEGER NOT NULL DEFAULT 0 CHECK (by_person IN (0, 1));
CREATE TRIGGER task_origins_by_fixed BEFORE UPDATE OF by_person ON task_origins
BEGIN SELECT RAISE(ABORT, 'origin is fixed at task creation'); END;
-- A task's origin is its skills' provenance: never removed.
CREATE TRIGGER task_origins_kept BEFORE DELETE ON task_origins
BEGIN SELECT RAISE(ABORT, 'a task''s origin is kept'); END;
