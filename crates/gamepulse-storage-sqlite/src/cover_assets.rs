use std::fmt;
use std::path::Path;

use gamepulse_application::{GameCoverDescriptor, SourceProductId, StoredCoverImage};
use rusqlite::{Connection, OpenFlags, params};

/// One stored descriptor that can be resolved by the opt-in bounded cover backfill command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoverBackfillCandidate {
    source_product_id: SourceProductId,
    descriptor: GameCoverDescriptor,
}

impl CoverBackfillCandidate {
    fn new(source_product_id: SourceProductId, descriptor: GameCoverDescriptor) -> Self {
        Self {
            source_product_id,
            descriptor,
        }
    }

    pub const fn source_product_id(&self) -> SourceProductId {
        self.source_product_id
    }

    pub fn descriptor(&self) -> &GameCoverDescriptor {
        &self.descriptor
    }
}

/// A dedicated SQLite adapter for local cover assets and bounded backfill discovery.
pub struct SqliteGameCoverAssetStore {
    connection: Connection,
}

impl SqliteGameCoverAssetStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GameCoverAssetStoreError> {
        let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(GameCoverAssetStoreError::database)?;
        super::initialize_connection(&mut connection)
            .map_err(GameCoverAssetStoreError::migration)?;
        Ok(Self { connection })
    }

    pub fn missing_cover_candidates(
        &mut self,
        limit: usize,
    ) -> Result<Vec<CoverBackfillCandidate>, GameCoverAssetStoreError> {
        let limit = i64::try_from(limit).map_err(|_| GameCoverAssetStoreError::invalid_input())?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT games.source_product_id,
                        games.cover_bucket_path,
                        games.cover_bucket_type,
                        games.cover_filename,
                        games.cover_kind
                 FROM games
                 LEFT JOIN game_cover_assets
                   ON game_cover_assets.game_source_product_id = games.source_product_id
                 WHERE game_cover_assets.game_source_product_id IS NULL
                   AND games.cover_bucket_path IS NOT NULL
                   AND games.cover_bucket_type IS NOT NULL
                   AND games.cover_filename IS NOT NULL
                   AND games.cover_kind IS NOT NULL
                 ORDER BY games.source_product_id ASC
                 LIMIT ?1",
            )
            .map_err(GameCoverAssetStoreError::database)?;
        let rows = statement
            .query_map(params![limit], |row| {
                let product_id = decode_source_product_id(row.get::<_, i64>(0)?)?;
                let descriptor = GameCoverDescriptor::new(
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                )
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
                Ok(CoverBackfillCandidate::new(product_id, descriptor))
            })
            .map_err(GameCoverAssetStoreError::database)?;
        rows.map(|row| row.map_err(GameCoverAssetStoreError::database))
            .collect()
    }

    pub fn store_cover(
        &mut self,
        source_product_id: SourceProductId,
        cover: &StoredCoverImage,
    ) -> Result<(), GameCoverAssetStoreError> {
        let source_product_id = sqlite_identifier(source_product_id)?;
        self.connection
            .execute(
                "INSERT INTO game_cover_assets (game_source_product_id, content_type, content)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(game_source_product_id) DO UPDATE SET
                    content_type = excluded.content_type,
                    content = excluded.content",
                params![
                    source_product_id,
                    cover.content_type().as_str(),
                    cover.bytes()
                ],
            )
            .map_err(GameCoverAssetStoreError::database)?;
        Ok(())
    }
}

#[derive(Debug)]
pub enum GameCoverAssetStoreError {
    Database(rusqlite::Error),
    Migration(super::DailyCrawlStateStoreError),
    InvalidInput,
}

impl GameCoverAssetStoreError {
    fn database(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }

    fn migration(error: super::DailyCrawlStateStoreError) -> Self {
        Self::Migration(error)
    }

    fn invalid_input() -> Self {
        Self::InvalidInput
    }
}

impl fmt::Display for GameCoverAssetStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SQLite cover asset operation failed")
    }
}

impl std::error::Error for GameCoverAssetStoreError {}

fn decode_source_product_id(value: i64) -> rusqlite::Result<SourceProductId> {
    let value =
        u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))?;
    SourceProductId::new(value)
        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value as i64))
}

fn sqlite_identifier(source_product_id: SourceProductId) -> Result<i64, GameCoverAssetStoreError> {
    i64::try_from(source_product_id.value()).map_err(|_| GameCoverAssetStoreError::invalid_input())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_nine_database_adds_an_empty_local_asset_table_without_touching_games() {
        let mut connection = Connection::open_in_memory().expect("test database must open");
        connection
            .execute_batch(super::super::DAILY_CRAWL_MIGRATION_0001)
            .expect("v1 schema must apply");
        connection
            .execute_batch(super::super::JOB_QUEUE_MIGRATION_0002)
            .expect("v2 schema must apply");
        connection
            .execute_batch(super::super::GAME_SNAPSHOT_MIGRATION_0003)
            .expect("v3 schema must apply");
        connection
            .execute_batch(super::super::REVIEW_SUMMARY_MIGRATION_0004)
            .expect("v4 schema must apply");
        connection
            .execute_batch(super::super::PUBLIC_COVER_URL_MIGRATION_0005)
            .expect("v5 schema must apply");
        connection
            .execute_batch(super::super::REVIEW_EXCERPT_POLARITY_MIGRATION_0006)
            .expect("v6 schema must apply");
        connection
            .execute_batch(super::super::RETRY_BACKOFF_AND_SOURCE_PACING_MIGRATION_0007)
            .expect("v7 schema must apply");
        connection
            .execute_batch(super::super::DURABLE_RUNS_MIGRATION_0008)
            .expect("v9 schema must apply");
        connection
            .execute(
                "INSERT INTO games (
                    source_product_id, source_slug, title, description,
                    cover_bucket_path, cover_bucket_type, cover_filename, cover_kind
                 ) VALUES (101, 'example-game', 'Example', 'Stored game',
                    '/provider/7/2/7-example.png', 'catalog', '7-example.png', 'cardImage')",
                [],
            )
            .expect("v9 game must persist");
        connection
            .pragma_update(None, "user_version", super::super::PREVIOUS_SCHEMA_VERSION)
            .expect("version nine must be recorded");

        super::super::initialize_connection(&mut connection).expect("v10 migration must apply");

        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .expect("version must read"),
            super::super::SCHEMA_VERSION
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM games", [], |row| row.get::<_, i64>(0))
                .expect("game count must read"),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM game_cover_assets", [], |row| row
                    .get::<_, i64>(0))
                .expect("asset count must read"),
            0
        );
    }

    #[test]
    fn version_eight_database_advances_through_both_pending_migrations() {
        let mut connection = Connection::open_in_memory().expect("test database must open");
        connection
            .execute_batch(super::super::DAILY_CRAWL_MIGRATION_0001)
            .expect("v1 schema must apply");
        connection
            .execute_batch(super::super::JOB_QUEUE_MIGRATION_0002)
            .expect("v2 schema must apply");
        connection
            .execute_batch(super::super::GAME_SNAPSHOT_MIGRATION_0003)
            .expect("v3 schema must apply");
        connection
            .execute_batch(super::super::REVIEW_SUMMARY_MIGRATION_0004)
            .expect("v4 schema must apply");
        connection
            .execute_batch(super::super::PUBLIC_COVER_URL_MIGRATION_0005)
            .expect("v5 schema must apply");
        connection
            .execute_batch(super::super::REVIEW_EXCERPT_POLARITY_MIGRATION_0006)
            .expect("v6 schema must apply");
        connection
            .execute_batch(super::super::RETRY_BACKOFF_AND_SOURCE_PACING_MIGRATION_0007)
            .expect("v7 schema must apply");
        connection
            .execute_batch(include_str!("../migrations/0008_durable_runs.sql"))
            .expect("v8 schema must apply");
        connection
            .pragma_update(
                None,
                "user_version",
                super::super::DURABLE_RUNS_SCHEMA_VERSION,
            )
            .expect("version eight must be recorded");

        super::super::initialize_connection(&mut connection)
            .expect("v8 database must migrate through v9 and v10");

        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .expect("version must read"),
            super::super::SCHEMA_VERSION
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM game_cover_assets", [], |row| row
                    .get::<_, i64>(0))
                .expect("asset table must exist"),
            0
        );
    }
}
