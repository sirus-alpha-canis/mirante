-- Skills (Plan 5). The library itself lives in $MIRANTE_HOME/skills/<name>/v<N>/; these
-- tables are its index, the trust ledger of manifest hashes a person accepted, the review
-- queue, the tenants with learning enabled, each task's origin (fixed at creation) and usage.
-- Each task's origin key and tenant, fixed at creation (an agent rewriting .git/config later
-- cannot move the task). `repo` is the checkout's canonical git common dir. `observed` is
-- set when the live origin at creation was not the repo's pinned one (or the repo had no
-- pin and no person created the task): such a task gets no skills until a person confirms
-- the pinned origin, which clears it. Nothing else of a row ever changes.
CREATE TABLE task_origins (task_id TEXT PRIMARY KEY REFERENCES tasks(id), repo TEXT NOT NULL, origin TEXT NOT NULL,
  tenant TEXT NOT NULL, observed TEXT, created_at INTEGER NOT NULL);
CREATE TRIGGER task_origins_once BEFORE UPDATE OF task_id, repo, origin, tenant, created_at ON task_origins
BEGIN SELECT RAISE(ABORT, 'origin is fixed at task creation'); END;
CREATE TRIGGER task_origins_confirm_only BEFORE UPDATE OF observed ON task_origins WHEN NEW.observed IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'origin is fixed at task creation'); END;
CREATE INDEX task_origins_tenant ON task_origins(tenant);
CREATE INDEX task_origins_repo ON task_origins(repo);
-- The origin a person pinned for a checkout (its canonical git common dir): the first task a
-- person created there, or a person's confirmation.
CREATE TABLE repo_origins (repo TEXT PRIMARY KEY, origin TEXT NOT NULL, tenant TEXT NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE skill_tenants (tenant TEXT PRIMARY KEY, learning INTEGER NOT NULL CHECK (learning IN (0,1)), updated_at INTEGER NOT NULL);
CREATE TABLE skills (name TEXT PRIMARY KEY, current_version INTEGER NOT NULL, manifest_hash TEXT NOT NULL,
  scope_kind TEXT NOT NULL CHECK (scope_kind IN ('personal','org','repo')), scope_value TEXT NOT NULL,
  activation TEXT NOT NULL CHECK (activation IN ('always','on_demand')), agents TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('active','retired')), description TEXT NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE skill_reviews (name TEXT NOT NULL, version INTEGER NOT NULL, manifest_hash TEXT NOT NULL,
  how TEXT NOT NULL CHECK (how IN ('accepted','rescoped','rolled_back')), reviewed_at INTEGER NOT NULL, superseded_at INTEGER,
  PRIMARY KEY (name, version));
CREATE TABLE skill_proposals (id TEXT PRIMARY KEY, origin TEXT NOT NULL CHECK (origin IN ('miner','agent','cli','import')),
  task_id TEXT REFERENCES tasks(id), tenant TEXT NOT NULL, kind TEXT NOT NULL CHECK (kind IN ('new','edit')),
  name TEXT NOT NULL, base_hash TEXT, scope_kind TEXT NOT NULL, scope_value TEXT NOT NULL, activation TEXT NOT NULL,
  description TEXT NOT NULL, body TEXT NOT NULL, files TEXT NOT NULL DEFAULT '[]', evidence TEXT NOT NULL DEFAULT '[]',
  fingerprint TEXT, lint TEXT NOT NULL DEFAULT '[]', redactions INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL CHECK (status IN ('pending','accepted','rejected')), version INTEGER, decision_note TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL, decided_at INTEGER);
CREATE INDEX skill_proposals_q ON skill_proposals(tenant, status, created_at);
CREATE INDEX skill_proposals_fp ON skill_proposals(fingerprint);
CREATE INDEX skill_proposals_task ON skill_proposals(task_id, status);
CREATE TRIGGER skill_proposals_decided_once BEFORE UPDATE OF status ON skill_proposals WHEN OLD.status != 'pending'
BEGIN SELECT RAISE(ABORT, 'proposal already decided'); END;
CREATE TABLE skill_usage (session_id TEXT NOT NULL, task_id TEXT NOT NULL, name TEXT NOT NULL, version INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('injected','used','refused')), detail TEXT NOT NULL DEFAULT '', ts_ms INTEGER NOT NULL);
CREATE INDEX skill_usage_name ON skill_usage(name, ts_ms);
CREATE INDEX skill_usage_session ON skill_usage(session_id, name);
