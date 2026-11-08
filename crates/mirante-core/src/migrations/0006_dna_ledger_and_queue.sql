-- DNA after the Batch B review.
-- The trust ledger keeps every hash a person reviewed, but only the latest review of a
-- slug counts: once a note is confirmed or retired, its older accepted text is no longer
-- "reviewed" if it comes back (a revert, a planted copy). Older rows stay as history.
ALTER TABLE dna_reviews ADD COLUMN superseded_at INTEGER;
UPDATE dna_reviews SET superseded_at = reviewed_at WHERE EXISTS (
    SELECT 1 FROM dna_reviews n
    WHERE n.repo = dna_reviews.repo AND n.slug = dna_reviews.slug
      AND (n.reviewed_at > dna_reviews.reviewed_at
           OR (n.reviewed_at = dna_reviews.reviewed_at AND n.rowid > dna_reviews.rowid))
);
CREATE INDEX dna_reviews_current ON dna_reviews(repo, slug) WHERE superseded_at IS NULL;
-- The review queue is read per task (task context, the per-task cap) and paged in order.
CREATE INDEX dna_proposals_task ON dna_proposals(task_id, status);
CREATE INDEX dna_proposals_order ON dna_proposals(status, created_at, id);
