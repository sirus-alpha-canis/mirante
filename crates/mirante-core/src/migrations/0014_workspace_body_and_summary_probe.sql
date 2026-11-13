-- What Mirante wrote in a workspace task's folder (MIRANTE_WORKSPACE.md and AGENTS.md hold
-- this text; CLAUDE.md imports it). The folder is the agent's working directory: a session
-- that starts checks the files against this copy and writes them again when they changed,
-- so one session cannot leave instructions for the next.
CREATE TABLE workspace_bodies (
    task_id TEXT PRIMARY KEY REFERENCES tasks(id),
    body    TEXT NOT NULL
);
-- The tool-less check of the person's Claude CLI before thread summaries, one verdict per
-- binary as it is on disk (canonical path, version, size, modification time): a changed
-- binary is checked again. Kept here, not in a file an agent could write.
CREATE TABLE summary_probes (
    bin     TEXT NOT NULL,
    version TEXT NOT NULL,
    size    INTEGER NOT NULL,
    mtime   INTEGER NOT NULL,
    ok      INTEGER NOT NULL CHECK (ok IN (0, 1)),
    at      INTEGER NOT NULL,
    PRIMARY KEY (bin, version, size, mtime)
);
