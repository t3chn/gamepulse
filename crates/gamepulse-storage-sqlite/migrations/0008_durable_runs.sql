CREATE TABLE runs (
    run_id TEXT PRIMARY KEY NOT NULL CHECK (length(trim(run_id)) > 0 AND length(run_id) <= 128),
    day_key TEXT NOT NULL UNIQUE CHECK (length(trim(day_key)) > 0),
    target_count INTEGER NOT NULL CHECK (target_count > 0),
    accepted_count INTEGER NOT NULL DEFAULT 0 CHECK (accepted_count >= 0 AND accepted_count <= target_count),
    state TEXT NOT NULL CHECK (state IN ('active', 'succeeded', 'failed_exhausted', 'failed_deadline')),
    source_phase TEXT NOT NULL CHECK (source_phase IN ('new_releases', 'browse', 'exhausted')),
    browse_cursor TEXT,
    deadline_at INTEGER NOT NULL CHECK (deadline_at >= 0),
    version INTEGER NOT NULL DEFAULT 0 CHECK (version >= 0),
    progress_fence INTEGER NOT NULL DEFAULT 0 CHECK (progress_fence >= 0),
    next_item_order INTEGER NOT NULL DEFAULT 0 CHECK (next_item_order >= 0),
    browse_page_count INTEGER NOT NULL DEFAULT 0 CHECK (browse_page_count >= 0 AND browse_page_count <= 8),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at),
    CHECK (
        (source_phase = 'browse')
        OR browse_cursor IS NULL
    ),
    CHECK (
        (state = 'succeeded' AND accepted_count = target_count)
        OR (state <> 'succeeded' AND accepted_count <= target_count)
    ),
    -- An active run may have exhausted listing pages while its already discovered items settle.
    -- It becomes failed only after those items cannot fill the exact target.
    CHECK (state <> 'succeeded' OR accepted_count = target_count)
);

CREATE TABLE run_items (
    run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE RESTRICT,
    source_product_id TEXT NOT NULL CHECK (length(source_product_id) > 0),
    source_slug TEXT NOT NULL CHECK (length(trim(source_slug)) > 0),
    discovery_order INTEGER NOT NULL CHECK (discovery_order >= 0),
    state TEXT NOT NULL CHECK (state IN ('pending', 'scheduled', 'complete', 'rejected')),
    job_identity TEXT,
    rejection_category TEXT CHECK (rejection_category IN ('missing_required_video')),
    PRIMARY KEY (run_id, source_product_id),
    CHECK (
        (state = 'pending' AND job_identity IS NULL AND rejection_category IS NULL)
        OR (state = 'scheduled' AND job_identity IS NOT NULL AND rejection_category IS NULL)
        OR (state = 'complete' AND job_identity IS NOT NULL AND rejection_category IS NULL)
        OR (state = 'rejected' AND job_identity IS NOT NULL AND rejection_category IS NOT NULL)
    )
) WITHOUT ROWID;

CREATE INDEX run_items_pending_order
    ON run_items (run_id, state, discovery_order);
