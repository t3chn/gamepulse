CREATE TABLE review_inputs (
    game_source_product_id INTEGER NOT NULL
        REFERENCES games(source_product_id) ON DELETE CASCADE,
    review_kind TEXT NOT NULL CHECK (review_kind IN ('critic', 'user')),
    content_hash TEXT NOT NULL CHECK (length(content_hash) = 64),
    refresh_fingerprint TEXT NOT NULL CHECK (length(refresh_fingerprint) = 64),
    PRIMARY KEY (game_source_product_id, review_kind)
) WITHOUT ROWID;

CREATE TABLE review_input_excerpts (
    game_source_product_id INTEGER NOT NULL,
    review_kind TEXT NOT NULL,
    excerpt_position INTEGER NOT NULL CHECK (excerpt_position >= 0 AND excerpt_position < 20),
    excerpt TEXT NOT NULL CHECK (length(trim(excerpt)) > 0 AND length(excerpt) <= 1024),
    PRIMARY KEY (game_source_product_id, review_kind, excerpt_position),
    FOREIGN KEY (game_source_product_id, review_kind)
        REFERENCES review_inputs(game_source_product_id, review_kind) ON DELETE CASCADE
) WITHOUT ROWID;

CREATE TABLE review_summaries (
    game_source_product_id INTEGER NOT NULL
        REFERENCES games(source_product_id) ON DELETE CASCADE,
    review_kind TEXT NOT NULL CHECK (review_kind IN ('critic', 'user')),
    refresh_fingerprint TEXT NOT NULL CHECK (length(refresh_fingerprint) = 64),
    state TEXT NOT NULL CHECK (state IN ('pending', 'unavailable', 'available')),
    PRIMARY KEY (game_source_product_id, review_kind)
) WITHOUT ROWID;

CREATE TABLE review_summary_items (
    game_source_product_id INTEGER NOT NULL,
    review_kind TEXT NOT NULL,
    sentiment TEXT NOT NULL CHECK (sentiment IN ('like', 'dislike')),
    item_position INTEGER NOT NULL CHECK (item_position >= 0 AND item_position < 3),
    item TEXT NOT NULL CHECK (length(trim(item)) > 0 AND length(item) <= 1024),
    PRIMARY KEY (game_source_product_id, review_kind, sentiment, item_position),
    FOREIGN KEY (game_source_product_id, review_kind)
        REFERENCES review_summaries(game_source_product_id, review_kind) ON DELETE CASCADE
) WITHOUT ROWID;
