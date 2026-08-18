ALTER TABLE run_items RENAME TO run_items_v8;

CREATE TABLE run_items (
    run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE RESTRICT,
    source_product_id TEXT NOT NULL CHECK (length(source_product_id) > 0),
    source_slug TEXT NOT NULL CHECK (length(trim(source_slug)) > 0),
    discovery_order INTEGER NOT NULL CHECK (discovery_order >= 0),
    state TEXT NOT NULL CHECK (state IN ('pending', 'scheduled', 'complete', 'rejected')),
    job_identity TEXT,
    rejection_category TEXT CHECK (
        rejection_category IN ('missing_required_video', 'source_unavailable')
    ),
    PRIMARY KEY (run_id, source_product_id),
    CHECK (
        (state = 'pending' AND job_identity IS NULL AND rejection_category IS NULL)
        OR (state = 'scheduled' AND job_identity IS NOT NULL AND rejection_category IS NULL)
        OR (state = 'complete' AND job_identity IS NOT NULL AND rejection_category IS NULL)
        OR (state = 'rejected' AND job_identity IS NOT NULL AND rejection_category IS NOT NULL)
    )
) WITHOUT ROWID;

INSERT INTO run_items (
    run_id,
    source_product_id,
    source_slug,
    discovery_order,
    state,
    job_identity,
    rejection_category
)
SELECT
    run_id,
    source_product_id,
    source_slug,
    discovery_order,
    state,
    job_identity,
    rejection_category
FROM run_items_v8;

DROP TABLE run_items_v8;

CREATE INDEX run_items_pending_order
    ON run_items (run_id, state, discovery_order);
