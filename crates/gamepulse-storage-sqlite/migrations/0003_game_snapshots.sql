CREATE TABLE games (
    source_product_id INTEGER PRIMARY KEY NOT NULL CHECK (source_product_id > 0),
    source_slug TEXT NOT NULL CHECK (length(trim(source_slug)) > 0),
    title TEXT NOT NULL CHECK (length(trim(title)) > 0),
    description TEXT NOT NULL CHECK (length(trim(description)) > 0),
    cover_bucket_path TEXT,
    cover_bucket_type TEXT,
    cover_filename TEXT,
    cover_kind TEXT,
    video_url TEXT,
    CHECK (
        (cover_bucket_path IS NULL
            AND cover_bucket_type IS NULL
            AND cover_filename IS NULL
            AND cover_kind IS NULL)
        OR (cover_bucket_path IS NOT NULL
            AND length(trim(cover_bucket_path)) > 0
            AND cover_bucket_type IS NOT NULL
            AND length(trim(cover_bucket_type)) > 0
            AND cover_filename IS NOT NULL
            AND length(trim(cover_filename)) > 0
            AND cover_kind IS NOT NULL
            AND length(trim(cover_kind)) > 0)
    ),
    CHECK (video_url IS NULL OR length(trim(video_url)) > 0)
);

CREATE TABLE game_platform_scores (
    game_source_product_id INTEGER NOT NULL
        REFERENCES games(source_product_id) ON DELETE CASCADE,
    source_platform_id INTEGER NOT NULL CHECK (source_platform_id > 0),
    source_slug TEXT NOT NULL CHECK (length(trim(source_slug)) > 0),
    metascore INTEGER CHECK (
        metascore IS NULL
        OR (typeof(metascore) = 'integer' AND metascore BETWEEN 0 AND 100)
    ),
    userscore REAL CHECK (
        userscore IS NULL
        OR (typeof(userscore) IN ('integer', 'real') AND userscore >= 0 AND userscore <= 10)
    ),
    PRIMARY KEY (game_source_product_id, source_platform_id)
) WITHOUT ROWID;

CREATE TABLE game_developers (
    game_source_product_id INTEGER NOT NULL
        REFERENCES games(source_product_id) ON DELETE CASCADE,
    developer_name TEXT NOT NULL CHECK (length(trim(developer_name)) > 0),
    PRIMARY KEY (game_source_product_id, developer_name)
) WITHOUT ROWID;
