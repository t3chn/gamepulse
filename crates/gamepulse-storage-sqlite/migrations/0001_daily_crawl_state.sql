CREATE TABLE crawl_days (
    day_key TEXT PRIMARY KEY NOT NULL CHECK (length(trim(day_key)) > 0),
    new_releases_completed INTEGER NOT NULL CHECK (new_releases_completed IN (0, 1)),
    browse_progress TEXT NOT NULL CHECK (browse_progress IN ('initial', 'continue', 'exhausted')),
    browse_cursor TEXT,
    CHECK (
        (browse_progress = 'continue' AND browse_cursor IS NOT NULL)
        OR (browse_progress IN ('initial', 'exhausted') AND browse_cursor IS NULL)
    )
);

CREATE TABLE crawl_day_selected_or_processed (
    day_key TEXT NOT NULL REFERENCES crawl_days(day_key) ON DELETE RESTRICT,
    source_product_id TEXT NOT NULL CHECK (length(source_product_id) > 0),
    PRIMARY KEY (day_key, source_product_id)
) WITHOUT ROWID;

CREATE TABLE crawl_day_selected_candidates (
    day_key TEXT NOT NULL,
    source_product_id TEXT NOT NULL,
    source_slug TEXT NOT NULL CHECK (length(source_slug) > 0),
    PRIMARY KEY (day_key, source_product_id),
    FOREIGN KEY (day_key, source_product_id)
        REFERENCES crawl_day_selected_or_processed(day_key, source_product_id)
        ON DELETE RESTRICT
) WITHOUT ROWID;
