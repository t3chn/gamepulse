CREATE TABLE jobs (
    job_identity TEXT PRIMARY KEY NOT NULL CHECK (length(trim(job_identity)) > 0),
    job_type TEXT NOT NULL CHECK (length(trim(job_type)) > 0),
    work_ref TEXT NOT NULL CHECK (length(trim(work_ref)) > 0),
    max_attempts INTEGER NOT NULL CHECK (max_attempts > 0),
    attempt_count INTEGER NOT NULL DEFAULT 0
        CHECK (attempt_count >= 0 AND attempt_count <= max_attempts),
    state TEXT NOT NULL CHECK (state IN ('ready', 'claimed', 'succeeded', 'failed')),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0),
    claimed_by TEXT,
    lease_expires_at INTEGER,
    claim_token INTEGER NOT NULL DEFAULT 0 CHECK (claim_token >= 0),
    terminal_at INTEGER,
    last_error TEXT,
    CHECK (created_at <= updated_at),
    CHECK (claim_token <= attempt_count),
    CHECK (
        (state = 'ready'
            AND claimed_by IS NULL
            AND lease_expires_at IS NULL
            AND terminal_at IS NULL)
        OR (state = 'claimed'
            AND claimed_by IS NOT NULL
            AND length(trim(claimed_by)) > 0
            AND lease_expires_at IS NOT NULL
            AND lease_expires_at > updated_at
            AND terminal_at IS NULL)
        OR (state = 'succeeded'
            AND claimed_by IS NULL
            AND lease_expires_at IS NULL
            AND terminal_at IS NOT NULL
            AND terminal_at >= updated_at
            AND last_error IS NULL)
        OR (state = 'failed'
            AND claimed_by IS NULL
            AND lease_expires_at IS NULL
            AND terminal_at IS NOT NULL
            AND terminal_at >= updated_at
            AND last_error IS NOT NULL
            AND length(last_error) > 0)
    )
);

CREATE INDEX jobs_ready_claim_order
    ON jobs (state, created_at, job_identity);

CREATE TABLE job_attempts (
    job_identity TEXT NOT NULL REFERENCES jobs(job_identity) ON DELETE RESTRICT,
    attempt_number INTEGER NOT NULL CHECK (attempt_number > 0),
    claim_token INTEGER NOT NULL CHECK (claim_token > 0),
    worker_id TEXT NOT NULL CHECK (length(trim(worker_id)) > 0),
    started_at INTEGER NOT NULL CHECK (started_at >= 0),
    finished_at INTEGER,
    outcome TEXT NOT NULL CHECK (
        outcome IN ('active', 'succeeded', 'retryable_failure', 'terminal_failure', 'expired')
    ),
    error TEXT,
    PRIMARY KEY (job_identity, claim_token),
    UNIQUE (job_identity, attempt_number),
    CHECK (finished_at IS NULL OR finished_at >= started_at),
    CHECK (
        (outcome = 'active' AND finished_at IS NULL AND error IS NULL)
        OR (outcome IN ('succeeded', 'expired') AND finished_at IS NOT NULL AND error IS NULL)
        OR (outcome IN ('retryable_failure', 'terminal_failure')
            AND finished_at IS NOT NULL
            AND error IS NOT NULL
            AND length(error) > 0)
    )
) WITHOUT ROWID;
