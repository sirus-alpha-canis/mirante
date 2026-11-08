-- DNA (repo memory). dna_notes / dna_anchors / dna_links / dna_fts are a regenerable index
-- of <repo>/.mirante/memory/*.md; dna_repos, dna_proposals and dna_reviews are not.
CREATE TABLE dna_repos (
    repo         TEXT PRIMARY KEY,                       -- canonical checkout path
    enabled      INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    checked_head TEXT,
    indexed_at   INTEGER,
    updated_at   INTEGER NOT NULL
);
CREATE TABLE dna_notes (
    repo TEXT NOT NULL, slug TEXT NOT NULL, id TEXT NOT NULL, title TEXT NOT NULL,
    kind TEXT NOT NULL, status TEXT NOT NULL CHECK (status IN ('active', 'retired')),
    body TEXT NOT NULL, content_hash TEXT NOT NULL, source TEXT NOT NULL, parse_error TEXT,
    PRIMARY KEY (repo, slug)
);
CREATE TABLE dna_anchors (
    repo TEXT NOT NULL, slug TEXT NOT NULL, idx INTEGER NOT NULL,
    path TEXT NOT NULL, symbol TEXT, line_start INTEGER NOT NULL, line_end INTEGER NOT NULL,
    commit_sha TEXT NOT NULL, fingerprint TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('unchecked', 'fresh', 'relocated', 'stale', 'unverifiable')),
    reason TEXT, now_start INTEGER, now_end INTEGER,
    PRIMARY KEY (repo, slug, idx)
);
CREATE INDEX dna_anchors_path ON dna_anchors(repo, path);
CREATE TABLE dna_links (repo TEXT NOT NULL, src TEXT NOT NULL, dst TEXT NOT NULL, PRIMARY KEY (repo, src, dst));
CREATE VIRTUAL TABLE dna_fts USING fts5(repo UNINDEXED, slug UNINDEXED, title, body,
    tokenize = 'unicode61 remove_diacritics 2');
CREATE TABLE dna_reviews (
    repo TEXT NOT NULL, slug TEXT NOT NULL, content_hash TEXT NOT NULL,
    how TEXT NOT NULL CHECK (how IN ('accepted', 'confirmed')), reviewed_at INTEGER NOT NULL,
    PRIMARY KEY (repo, slug, content_hash)
);
CREATE TABLE dna_proposals (
    id TEXT PRIMARY KEY, repo TEXT NOT NULL, task_id TEXT REFERENCES tasks(id),
    origin TEXT NOT NULL CHECK (origin IN ('agent', 'cli')),
    title TEXT NOT NULL, kind TEXT NOT NULL, body TEXT NOT NULL,
    anchors TEXT NOT NULL,                                  -- JSON array of Anchor
    redactions INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL CHECK (status IN ('pending', 'accepted', 'rejected')),
    slug TEXT, decision_note TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL, decided_at INTEGER
);
CREATE INDEX dna_proposals_repo ON dna_proposals(repo, status);
CREATE TRIGGER dna_proposals_decided_once BEFORE UPDATE ON dna_proposals
WHEN OLD.status != 'pending'
BEGIN SELECT RAISE(ABORT, 'proposal already decided'); END;
