CREATE TABLE game_cover_assets_rebound (
    game_source_product_id INTEGER PRIMARY KEY NOT NULL
        REFERENCES games(source_product_id) ON DELETE CASCADE,
    content_type TEXT NOT NULL CHECK (content_type IN ('image/jpeg', 'image/png', 'image/webp')),
    content BLOB NOT NULL CHECK (length(content) > 0 AND length(content) <= 2097152),
    descriptor_fingerprint TEXT NOT NULL CHECK (length(descriptor_fingerprint) > 0)
) WITHOUT ROWID;

INSERT INTO game_cover_assets_rebound (
    game_source_product_id,
    content_type,
    content,
    descriptor_fingerprint
)
SELECT
    assets.game_source_product_id,
    assets.content_type,
    assets.content,
    'v1:' || length(CAST(games.cover_bucket_path AS BLOB)) || ':' || lower(hex(games.cover_bucket_path)) ||
    ':' || length(CAST(games.cover_bucket_type AS BLOB)) || ':' || lower(hex(games.cover_bucket_type)) ||
    ':' || length(CAST(games.cover_filename AS BLOB)) || ':' || lower(hex(games.cover_filename)) ||
    ':' || length(CAST(games.cover_kind AS BLOB)) || ':' || lower(hex(games.cover_kind))
FROM game_cover_assets AS assets
JOIN games ON games.source_product_id = assets.game_source_product_id
WHERE games.cover_bucket_path IS NOT NULL
  AND games.cover_bucket_type IS NOT NULL
  AND games.cover_filename IS NOT NULL
  AND games.cover_kind IS NOT NULL;

DROP TABLE game_cover_assets;
ALTER TABLE game_cover_assets_rebound RENAME TO game_cover_assets;
