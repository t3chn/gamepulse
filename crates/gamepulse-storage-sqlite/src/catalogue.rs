use std::fmt;
use std::path::Path;

use gamepulse_application::{
    CatalogueCoverDescriptor, CatalogueGameCard, CatalogueGameDetail, CataloguePage,
    CataloguePlatformFilter, CataloguePlatformScore, CatalogueQuery, CatalogueReviewSummary,
    GameCatalogueReadPort, ReviewKind, SimilarCatalogueGame, SourceProductId,
};
use rusqlite::{Connection, OptionalExtension, params};

/// A SQLite implementation of the application-owned catalogue read port.
pub struct SqliteGameCatalogueReadStore {
    connection: Connection,
}

impl SqliteGameCatalogueReadStore {
    /// Open a file-backed database and apply all embedded storage migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GameCatalogueReadStoreError> {
        let mut connection =
            Connection::open(path).map_err(GameCatalogueReadStoreError::database)?;
        super::initialize_connection(&mut connection)
            .map_err(GameCatalogueReadStoreError::migration)?;
        Ok(Self { connection })
    }

    /// Open an isolated in-memory database and apply all embedded storage migrations.
    pub fn open_in_memory() -> Result<Self, GameCatalogueReadStoreError> {
        let mut connection =
            Connection::open_in_memory().map_err(GameCatalogueReadStoreError::database)?;
        super::initialize_connection(&mut connection)
            .map_err(GameCatalogueReadStoreError::migration)?;
        Ok(Self { connection })
    }

    fn list_catalogue(
        &mut self,
        query: &CatalogueQuery,
    ) -> Result<CataloguePage, GameCatalogueReadStoreError> {
        let mut statement = self
            .connection
            .prepare(
                "WITH catalogue_rows AS (
                    SELECT
                        games.source_product_id,
                        games.title,
                        games.public_cover_url,
                        CASE
                            WHEN ?2 IS NULL THEN (
                                SELECT MAX(platform_scores.metascore)
                                FROM game_platform_scores AS platform_scores
                                WHERE platform_scores.game_source_product_id = games.source_product_id
                            )
                            ELSE (
                                SELECT MAX(platform_scores.metascore)
                                FROM game_platform_scores AS platform_scores
                                WHERE platform_scores.game_source_product_id = games.source_product_id
                                  AND lower(platform_scores.source_slug) = lower(?2)
                            )
                        END AS selected_metascore
                    FROM games
                    WHERE (?1 IS NULL OR instr(lower(games.title), lower(?1)) > 0)
                      AND (
                          ?2 IS NULL
                          OR EXISTS (
                              SELECT 1
                              FROM game_platform_scores AS platform_scores
                              WHERE platform_scores.game_source_product_id = games.source_product_id
                                AND lower(platform_scores.source_slug) = lower(?2)
                          )
                      )
                )
                SELECT source_product_id, title, public_cover_url, selected_metascore
                FROM catalogue_rows
                ORDER BY
                    selected_metascore IS NULL ASC,
                    selected_metascore DESC,
                    lower(title) ASC,
                    title ASC,
                    source_product_id ASC",
            )
            .map_err(GameCatalogueReadStoreError::database)?;
        let rows = statement
            .query_map(
                params![query.title_search(), query.platform_slug()],
                |row| {
                    Ok((
                        decode_source_product_id(row.get::<_, i64>(0)?)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<u8>>(3)?,
                    ))
                },
            )
            .map_err(GameCatalogueReadStoreError::database)?;
        let mut games = Vec::new();
        for row in rows {
            let (source_product_id, title, public_cover_url, highest_metascore) =
                row.map_err(GameCatalogueReadStoreError::database)?;
            games.push(CatalogueGameCard::new(
                source_product_id,
                title,
                public_cover_url,
                highest_metascore,
                self.platform_slugs(source_product_id)?,
                self.developers(source_product_id)?,
            ));
        }

        Ok(CataloguePage::new(games, self.platform_filters()?))
    }

    fn game_detail(
        &mut self,
        source_product_id: SourceProductId,
    ) -> Result<Option<CatalogueGameDetail>, GameCatalogueReadStoreError> {
        let identifier = sqlite_identifier(source_product_id)?;
        let stored_game = self
            .connection
            .query_row(
                "SELECT
                    source_product_id,
                    source_slug,
                    title,
                    description,
                    cover_bucket_path,
                    cover_bucket_type,
                    cover_filename,
                    cover_kind,
                    video_url,
                    public_cover_url
                 FROM games
                 WHERE source_product_id = ?1",
                params![identifier],
                read_stored_game,
            )
            .optional()
            .map_err(GameCatalogueReadStoreError::database)?;

        let Some(stored_game) = stored_game else {
            return Ok(None);
        };
        let platforms = self.platform_scores(stored_game.source_product_id)?;
        let developers = self.developers(stored_game.source_product_id)?;
        let similar_games = self.similar_games(stored_game.source_product_id)?;
        let critic_summary =
            self.review_summary(stored_game.source_product_id, ReviewKind::Critic)?;
        let user_summary = self.review_summary(stored_game.source_product_id, ReviewKind::User)?;

        Ok(Some(CatalogueGameDetail::new(
            stored_game.source_product_id,
            stored_game.source_slug,
            stored_game.title,
            stored_game.description,
            stored_game.cover,
            stored_game.public_cover_url,
            stored_game.video_url,
            platforms,
            developers,
            similar_games,
            critic_summary,
            user_summary,
        )))
    }

    fn platform_filters(
        &self,
    ) -> Result<Vec<CataloguePlatformFilter>, GameCatalogueReadStoreError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT source_slug
                 FROM game_platform_scores
                 GROUP BY source_slug
                 ORDER BY lower(source_slug) ASC, source_slug ASC",
            )
            .map_err(GameCatalogueReadStoreError::database)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(GameCatalogueReadStoreError::database)?;
        rows.map(|row| {
            row.map(CataloguePlatformFilter::new)
                .map_err(GameCatalogueReadStoreError::database)
        })
        .collect()
    }

    fn platform_slugs(
        &self,
        source_product_id: SourceProductId,
    ) -> Result<Vec<String>, GameCatalogueReadStoreError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT source_slug
                 FROM game_platform_scores
                 WHERE game_source_product_id = ?1
                 ORDER BY lower(source_slug) ASC, source_slug ASC, source_platform_id ASC",
            )
            .map_err(GameCatalogueReadStoreError::database)?;
        let rows = statement
            .query_map(params![sqlite_identifier(source_product_id)?], |row| {
                row.get::<_, String>(0)
            })
            .map_err(GameCatalogueReadStoreError::database)?;
        rows.map(|row| row.map_err(GameCatalogueReadStoreError::database))
            .collect()
    }

    fn platform_scores(
        &self,
        source_product_id: SourceProductId,
    ) -> Result<Vec<CataloguePlatformScore>, GameCatalogueReadStoreError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT source_platform_id, source_slug, metascore, userscore
                 FROM game_platform_scores
                 WHERE game_source_product_id = ?1
                 ORDER BY lower(source_slug) ASC, source_slug ASC, source_platform_id ASC",
            )
            .map_err(GameCatalogueReadStoreError::database)?;
        let rows = statement
            .query_map(params![sqlite_identifier(source_product_id)?], |row| {
                let source_platform_id = row.get::<_, i64>(0)?;
                let source_platform_id = u64::try_from(source_platform_id)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, source_platform_id))?;
                Ok(CataloguePlatformScore::new(
                    source_platform_id,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<u8>>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                ))
            })
            .map_err(GameCatalogueReadStoreError::database)?;
        rows.map(|row| row.map_err(GameCatalogueReadStoreError::database))
            .collect()
    }

    fn developers(
        &self,
        source_product_id: SourceProductId,
    ) -> Result<Vec<String>, GameCatalogueReadStoreError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT developer_name
                 FROM game_developers
                 WHERE game_source_product_id = ?1
                 ORDER BY lower(developer_name) ASC, developer_name ASC",
            )
            .map_err(GameCatalogueReadStoreError::database)?;
        let rows = statement
            .query_map(params![sqlite_identifier(source_product_id)?], |row| {
                row.get::<_, String>(0)
            })
            .map_err(GameCatalogueReadStoreError::database)?;
        rows.map(|row| row.map_err(GameCatalogueReadStoreError::database))
            .collect()
    }

    fn similar_games(
        &self,
        source_product_id: SourceProductId,
    ) -> Result<Vec<SimilarCatalogueGame>, GameCatalogueReadStoreError> {
        let identifier = sqlite_identifier(source_product_id)?;
        let mut statement = self
            .connection
            .prepare(
                "WITH
                    target_platforms AS (
                        SELECT source_platform_id
                        FROM game_platform_scores
                        WHERE game_source_product_id = ?1
                    ),
                    target_developers AS (
                        SELECT developer_name
                        FROM game_developers
                        WHERE game_source_product_id = ?1
                    ),
                    candidates AS (
                        SELECT
                            games.source_product_id,
                            games.title,
                            (
                                SELECT COUNT(*)
                                FROM game_platform_scores AS candidate_platforms
                                INNER JOIN target_platforms
                                    ON target_platforms.source_platform_id
                                        = candidate_platforms.source_platform_id
                                WHERE candidate_platforms.game_source_product_id
                                    = games.source_product_id
                            ) AS shared_platform_count,
                            (
                                SELECT COUNT(*)
                                FROM game_developers AS candidate_developers
                                INNER JOIN target_developers
                                    ON candidate_developers.developer_name
                                        = target_developers.developer_name COLLATE NOCASE
                                WHERE candidate_developers.game_source_product_id
                                    = games.source_product_id
                            ) AS shared_developer_count
                        FROM games
                        WHERE games.source_product_id != ?1
                    )
                SELECT source_product_id, title
                FROM candidates
                WHERE shared_platform_count > 0 OR shared_developer_count > 0
                ORDER BY
                    shared_platform_count DESC,
                    shared_developer_count DESC,
                    source_product_id ASC
                LIMIT 5",
            )
            .map_err(GameCatalogueReadStoreError::database)?;
        let rows = statement
            .query_map(params![identifier], |row| {
                Ok(SimilarCatalogueGame::new(
                    decode_source_product_id(row.get::<_, i64>(0)?)?,
                    row.get::<_, String>(1)?,
                ))
            })
            .map_err(GameCatalogueReadStoreError::database)?;
        rows.map(|row| row.map_err(GameCatalogueReadStoreError::database))
            .collect()
    }

    fn review_summary(
        &mut self,
        source_product_id: SourceProductId,
        kind: ReviewKind,
    ) -> Result<Option<CatalogueReviewSummary>, GameCatalogueReadStoreError> {
        let identifier = sqlite_identifier(source_product_id)?;
        let transaction = self
            .connection
            .transaction()
            .map_err(GameCatalogueReadStoreError::database)?;
        let state = transaction
            .query_row(
                "SELECT state
                 FROM review_summaries
                 WHERE game_source_product_id = ?1 AND review_kind = ?2",
                params![identifier, kind.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(GameCatalogueReadStoreError::database)?;
        let summary = match state.as_deref() {
            None => None,
            Some("pending") => Some(CatalogueReviewSummary::Pending),
            Some("unavailable") => Some(CatalogueReviewSummary::Unavailable),
            Some("available") => {
                let mut statement = transaction
                    .prepare(
                        "SELECT sentiment, item
                         FROM review_summary_items
                         WHERE game_source_product_id = ?1 AND review_kind = ?2
                         ORDER BY sentiment ASC, item_position ASC",
                    )
                    .map_err(GameCatalogueReadStoreError::database)?;
                let rows = statement
                    .query_map(params![identifier, kind.as_str()], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })
                    .map_err(GameCatalogueReadStoreError::database)?;
                let mut likes = Vec::new();
                let mut dislikes = Vec::new();
                for row in rows {
                    let (sentiment, item) = row.map_err(GameCatalogueReadStoreError::database)?;
                    match sentiment.as_str() {
                        "like" => likes.push(item),
                        "dislike" => dislikes.push(item),
                        _ => return Err(GameCatalogueReadStoreError::malformed_summary()),
                    }
                }
                Some(CatalogueReviewSummary::Available { likes, dislikes })
            }
            Some(_) => return Err(GameCatalogueReadStoreError::malformed_summary()),
        };
        transaction
            .commit()
            .map_err(GameCatalogueReadStoreError::database)?;
        Ok(summary)
    }
}

impl GameCatalogueReadPort for SqliteGameCatalogueReadStore {
    type Error = GameCatalogueReadStoreError;

    fn list_catalogue(
        &mut self,
        query: &CatalogueQuery,
    ) -> Result<CataloguePage, GameCatalogueReadStoreError> {
        self.list_catalogue(query)
    }

    fn game_detail(
        &mut self,
        source_product_id: SourceProductId,
    ) -> Result<Option<CatalogueGameDetail>, GameCatalogueReadStoreError> {
        self.game_detail(source_product_id)
    }
}

struct StoredGame {
    source_product_id: SourceProductId,
    source_slug: String,
    title: String,
    description: String,
    cover: Option<CatalogueCoverDescriptor>,
    video_url: Option<String>,
    public_cover_url: Option<String>,
}

fn read_stored_game(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredGame> {
    let cover_bucket_path = row.get::<_, Option<String>>(4)?;
    let cover_bucket_type = row.get::<_, Option<String>>(5)?;
    let cover_filename = row.get::<_, Option<String>>(6)?;
    let cover_kind = row.get::<_, Option<String>>(7)?;
    let cover = match (
        cover_bucket_path,
        cover_bucket_type,
        cover_filename,
        cover_kind,
    ) {
        (None, None, None, None) => None,
        (Some(bucket_path), Some(bucket_type), Some(filename), Some(kind)) => Some(
            CatalogueCoverDescriptor::new(bucket_path, bucket_type, filename, kind),
        ),
        _ => return Err(rusqlite::Error::InvalidQuery),
    };

    Ok(StoredGame {
        source_product_id: decode_source_product_id(row.get::<_, i64>(0)?)?,
        source_slug: row.get(1)?,
        title: row.get(2)?,
        description: row.get(3)?,
        cover,
        video_url: row.get(8)?,
        public_cover_url: row.get(9)?,
    })
}

fn decode_source_product_id(value: i64) -> rusqlite::Result<SourceProductId> {
    let value =
        u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))?;
    SourceProductId::new(value)
        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value as i64))
}

fn sqlite_identifier(
    source_product_id: SourceProductId,
) -> Result<i64, GameCatalogueReadStoreError> {
    i64::try_from(source_product_id.value()).map_err(|_| GameCatalogueReadStoreError::identifier())
}

/// The adapter's error surface for catalogue reads and migration failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameCatalogueReadStoreError {
    message: String,
}

impl GameCatalogueReadStoreError {
    fn database(error: rusqlite::Error) -> Self {
        Self {
            message: format!("SQLite catalogue read failed: {error}"),
        }
    }

    fn migration(_: super::DailyCrawlStateStoreError) -> Self {
        Self {
            message: "SQLite catalogue migration failed".to_owned(),
        }
    }

    fn identifier() -> Self {
        Self {
            message: "game source product identity exceeds SQLite INTEGER range".to_owned(),
        }
    }

    fn malformed_summary() -> Self {
        Self {
            message: "SQLite catalogue review summary is malformed".to_owned(),
        }
    }
}

impl fmt::Display for GameCatalogueReadStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GameCatalogueReadStoreError {}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process;
    use std::sync::atomic::{AtomicU64, Ordering};

    use gamepulse_application::{
        CatalogueQuery, GameDeveloper, GamePlatformScore, GameSnapshot, GameVideoLink, Metascore,
        SourceProductId, upsert_game_snapshot,
    };

    use super::*;
    use crate::SqliteGameSnapshotStore;

    static NEXT_TEMPORARY_DATABASE: AtomicU64 = AtomicU64::new(0);

    struct TemporaryDatabase {
        path: PathBuf,
    }

    impl TemporaryDatabase {
        fn new() -> Self {
            let sequence = NEXT_TEMPORARY_DATABASE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "gamepulse-catalogue-read-adapter-{}-{sequence}.sqlite3",
                process::id()
            ));
            let _ = fs::remove_file(&path);
            Self { path }
        }
    }

    impl Drop for TemporaryDatabase {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
            let _ = fs::remove_file(self.path.with_extension("sqlite3-shm"));
            let _ = fs::remove_file(self.path.with_extension("sqlite3-wal"));
        }
    }

    fn snapshot(
        source_product_id: u64,
        title: &str,
        platforms: &[(u64, &str, Option<u8>)],
        developers: &[&str],
    ) -> GameSnapshot {
        GameSnapshot::new(
            SourceProductId::new(source_product_id).expect("test source identity must be valid"),
            format!("game-{source_product_id}"),
            title,
            "Stored fixture description",
            None,
            Some(GameVideoLink::new("https://video.example.test/embed").expect("test video")),
            platforms
                .iter()
                .map(|(platform_id, slug, metascore)| {
                    GamePlatformScore::new(
                        *platform_id,
                        *slug,
                        metascore.map(|value| {
                            Metascore::new(value).expect("test Metascore must be valid")
                        }),
                        None,
                    )
                    .expect("test platform must be valid")
                })
                .collect(),
            developers
                .iter()
                .map(|developer| GameDeveloper::new(*developer).expect("test developer"))
                .collect(),
        )
        .expect("test snapshot must be valid")
    }

    #[test]
    fn reads_case_insensitive_search_platform_rating_and_sqlite_only_similar_games() {
        let database = TemporaryDatabase::new();
        let mut snapshots =
            SqliteGameSnapshotStore::open(&database.path).expect("snapshot store must open");
        for game in [
            snapshot(101, "Alpha", &[(7, "pc", Some(80))], &["Studio A"]),
            snapshot(102, "Beta", &[(7, "pc", Some(90))], &["Studio B"]),
            snapshot(103, "Gamma", &[(8, "console", Some(95))], &["Studio A"]),
            snapshot(104, "Delta", &[(7, "pc", Some(85))], &["Studio A"]),
        ] {
            upsert_game_snapshot(&mut snapshots, &game).expect("fixture snapshot must persist");
        }
        drop(snapshots);

        let mut catalogue =
            SqliteGameCatalogueReadStore::open(&database.path).expect("catalogue must open");
        let all = catalogue
            .list_catalogue(&CatalogueQuery::default())
            .expect("catalogue query must succeed");
        assert_eq!(
            all.games()
                .iter()
                .map(|game| game.title())
                .collect::<Vec<_>>(),
            ["Gamma", "Beta", "Delta", "Alpha"]
        );

        let search = catalogue
            .list_catalogue(&CatalogueQuery::new(Some("aLpHa".to_owned()), None))
            .expect("case-insensitive search must succeed");
        assert_eq!(
            search
                .games()
                .iter()
                .map(|game| game.source_product_id().value())
                .collect::<Vec<_>>(),
            [101]
        );

        let platform = catalogue
            .list_catalogue(&CatalogueQuery::new(None, Some("PC".to_owned())))
            .expect("platform query must succeed");
        assert_eq!(
            platform
                .games()
                .iter()
                .map(|game| game.source_product_id().value())
                .collect::<Vec<_>>(),
            [102, 104, 101]
        );

        let detail = catalogue
            .game_detail(SourceProductId::new(101).expect("test identity must be valid"))
            .expect("detail query must succeed")
            .expect("stored game must be found");
        assert_eq!(
            detail
                .similar_games()
                .iter()
                .map(|game| game.source_product_id().value())
                .collect::<Vec<_>>(),
            [104, 102, 103]
        );
    }
}
