CREATE TABLE game_cover_assets (
    game_source_product_id INTEGER PRIMARY KEY NOT NULL
        REFERENCES games(source_product_id) ON DELETE CASCADE,
    content_type TEXT NOT NULL CHECK (content_type IN ('image/jpeg', 'image/png', 'image/webp')),
    content BLOB NOT NULL CHECK (length(content) > 0 AND length(content) <= 2097152)
) WITHOUT ROWID;
