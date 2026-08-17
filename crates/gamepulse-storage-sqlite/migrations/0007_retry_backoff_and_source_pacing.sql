ALTER TABLE jobs ADD COLUMN retry_not_before INTEGER;

UPDATE jobs
SET retry_not_before = updated_at
WHERE state = 'ready';

CREATE INDEX jobs_ready_retry_claim_order
    ON jobs (state, retry_not_before, created_at, job_identity);

CREATE TABLE job_lane_pacing (
    lane_key TEXT PRIMARY KEY NOT NULL CHECK (length(trim(lane_key)) > 0),
    next_claim_at INTEGER NOT NULL CHECK (next_claim_at >= 0)
) WITHOUT ROWID;
