use std::fmt;
use std::path::Path;

use gamepulse_application::{
    CoverBackfillCandidate, CoverBackfillPersistOutcome, CoverBackfillStorePort,
    GameCoverDescriptor, SourceProductId, StoredCoverImage,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

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

    fn cover_backfill_candidates(
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
                 WHERE games.cover_bucket_path IS NOT NULL
                   AND games.cover_bucket_type IS NOT NULL
                   AND games.cover_filename IS NOT NULL
                   AND games.cover_kind IS NOT NULL
                   AND (
                       game_cover_assets.game_source_product_id IS NULL
                       OR game_cover_assets.descriptor_fingerprint <> (
                           'v1:' || length(CAST(games.cover_bucket_path AS BLOB)) || ':' || lower(hex(games.cover_bucket_path)) ||
                           ':' || length(CAST(games.cover_bucket_type AS BLOB)) || ':' || lower(hex(games.cover_bucket_type)) ||
                           ':' || length(CAST(games.cover_filename AS BLOB)) || ':' || lower(hex(games.cover_filename)) ||
                           ':' || length(CAST(games.cover_kind AS BLOB)) || ':' || lower(hex(games.cover_kind))
                       )
                   )
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

    fn store_cover_if_current(
        &mut self,
        candidate: &CoverBackfillCandidate,
        cover: &StoredCoverImage,
    ) -> Result<CoverBackfillPersistOutcome, GameCoverAssetStoreError> {
        let source_product_id = sqlite_identifier(candidate.source_product_id())?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(GameCoverAssetStoreError::database)?;
        let current_descriptor = transaction
            .query_row(
                "SELECT cover_bucket_path, cover_bucket_type, cover_filename, cover_kind
                 FROM games WHERE source_product_id = ?1",
                params![source_product_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(GameCoverAssetStoreError::database)?;
        let descriptor_is_current =
            current_descriptor.is_some_and(|(bucket_path, bucket_type, filename, kind)| {
                bucket_path.as_deref() == Some(candidate.descriptor().bucket_path())
                    && bucket_type.as_deref() == Some(candidate.descriptor().bucket_type())
                    && filename.as_deref() == Some(candidate.descriptor().filename())
                    && kind.as_deref() == Some(candidate.descriptor().kind())
            });
        if !descriptor_is_current {
            transaction
                .commit()
                .map_err(GameCoverAssetStoreError::database)?;
            return Ok(CoverBackfillPersistOutcome::Stale);
        }
        let existing_fingerprint = transaction
            .query_row(
                "SELECT descriptor_fingerprint FROM game_cover_assets
                 WHERE game_source_product_id = ?1",
                params![source_product_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(GameCoverAssetStoreError::database)?;
        if existing_fingerprint.as_deref() == Some(candidate.descriptor_fingerprint().as_str()) {
            transaction
                .commit()
                .map_err(GameCoverAssetStoreError::database)?;
            return Ok(CoverBackfillPersistOutcome::AlreadyCurrent);
        }
        transaction
            .execute(
                "INSERT INTO game_cover_assets (
                    game_source_product_id, content_type, content, descriptor_fingerprint
                 ) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(game_source_product_id) DO UPDATE SET
                    content_type = excluded.content_type,
                    content = excluded.content,
                    descriptor_fingerprint = excluded.descriptor_fingerprint",
                params![
                    source_product_id,
                    cover.content_type().as_str(),
                    cover.bytes(),
                    candidate.descriptor_fingerprint().as_str(),
                ],
            )
            .map_err(GameCoverAssetStoreError::database)?;
        transaction
            .commit()
            .map_err(GameCoverAssetStoreError::database)?;
        Ok(CoverBackfillPersistOutcome::Stored)
    }
}

impl CoverBackfillStorePort for SqliteGameCoverAssetStore {
    type Error = GameCoverAssetStoreError;

    fn cover_backfill_candidates(
        &mut self,
        limit: usize,
    ) -> Result<Vec<CoverBackfillCandidate>, GameCoverAssetStoreError> {
        self.cover_backfill_candidates(limit)
    }

    fn store_cover_if_current(
        &mut self,
        candidate: &CoverBackfillCandidate,
        cover: &StoredCoverImage,
    ) -> Result<CoverBackfillPersistOutcome, GameCoverAssetStoreError> {
        self.store_cover_if_current(candidate, cover)
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
    use std::fs;
    use std::path::PathBuf;
    use std::process;
    use std::sync::atomic::{AtomicU64, Ordering};

    use gamepulse_application::{
        CoverDescriptorFingerprint, CoverImageContentType, GameSnapshot, upsert_game_snapshot,
    };

    use super::*;

    static NEXT_TEMPORARY_DATABASE: AtomicU64 = AtomicU64::new(0);

    struct TemporaryDatabase {
        path: PathBuf,
    }

    impl TemporaryDatabase {
        fn new() -> Self {
            let sequence = NEXT_TEMPORARY_DATABASE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "gamepulse-cover-assets-{}-{sequence}.sqlite3",
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

    fn apply_version_nine_schema(connection: &Connection) {
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
    }

    fn insert_game_with_descriptor(connection: &Connection, filename: &str) {
        connection
            .execute(
                "INSERT INTO games (
                    source_product_id, source_slug, title, description,
                    cover_bucket_path, cover_bucket_type, cover_filename, cover_kind
                 ) VALUES (101, 'example-game', 'Example', 'Stored game',
                    ?1, 'catalog', ?2, 'cardImage')",
                params![format!("/provider/7/2/{filename}"), filename],
            )
            .expect("legacy game must persist");
    }

    fn descriptor(filename: &str) -> GameCoverDescriptor {
        GameCoverDescriptor::new(
            format!("/provider/7/2/{filename}"),
            "catalog",
            filename,
            "cardImage",
        )
        .expect("test descriptor must be valid")
    }

    fn snapshot(descriptor: GameCoverDescriptor) -> GameSnapshot {
        GameSnapshot::new(
            SourceProductId::new(101).expect("test identity must be valid"),
            "example-game",
            "Example",
            "Stored game",
            Some(descriptor),
            None,
            Vec::new(),
            Vec::new(),
        )
        .expect("test snapshot must be valid")
    }

    fn png() -> StoredCoverImage {
        StoredCoverImage::new(
            CoverImageContentType::Png,
            vec![137, 80, 78, 71, 13, 10, 26, 10],
        )
        .expect("test cover must be valid")
    }

    #[test]
    fn version_nine_database_adds_current_asset_schema_without_touching_games() {
        let mut connection = Connection::open_in_memory().expect("test database must open");
        apply_version_nine_schema(&connection);
        insert_game_with_descriptor(&connection, "7-example.png");
        connection
            .pragma_update(None, "user_version", super::super::PREVIOUS_SCHEMA_VERSION)
            .expect("version nine must be recorded");

        super::super::initialize_connection(&mut connection).expect("current migration must apply");

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
    fn interrupted_v8_upgrade_reopens_from_the_v9_checkpoint_without_data_loss() {
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
        insert_game_with_descriptor(&connection, "interrupted.png");
        connection
            .execute_batch(super::super::SOURCE_UNAVAILABLE_REJECTION_MIGRATION_0009)
            .expect("v9 migration must apply before interruption");
        connection
            .pragma_update(None, "user_version", super::super::PREVIOUS_SCHEMA_VERSION)
            .expect("recoverable v9 checkpoint must be recorded");

        super::super::initialize_connection(&mut connection)
            .expect("interrupted migration must recover");

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
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM games", [], |row| row.get::<_, i64>(0))
                .expect("game count must read"),
            1
        );
    }

    #[test]
    fn incorrectly_marked_v10_database_from_the_prior_release_recovers_on_reopen() {
        let mut connection = Connection::open_in_memory().expect("test database must open");
        apply_version_nine_schema(&connection);
        insert_game_with_descriptor(&connection, "legacy.png");
        connection
            .pragma_update(
                None,
                "user_version",
                super::super::LOCAL_COVER_ASSETS_SCHEMA_VERSION,
            )
            .expect("prior-release interrupted marker must be recorded");

        super::super::initialize_connection(&mut connection)
            .expect("prior-release interrupted state must recover");

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
                .expect("asset table must exist"),
            0
        );
    }

    #[test]
    fn v10_asset_migration_binds_preserved_bytes_to_the_current_descriptor() {
        let mut connection = Connection::open_in_memory().expect("test database must open");
        apply_version_nine_schema(&connection);
        insert_game_with_descriptor(&connection, "legacy.png");
        connection
            .execute_batch(super::super::LOCAL_COVER_ASSETS_MIGRATION_0010)
            .expect("v10 table must apply");
        connection
            .execute(
                "INSERT INTO game_cover_assets (game_source_product_id, content_type, content)
                 VALUES (101, 'image/png', ?1)",
                params![png().bytes()],
            )
            .expect("legacy asset must persist");
        connection
            .pragma_update(
                None,
                "user_version",
                super::super::LOCAL_COVER_ASSETS_SCHEMA_VERSION,
            )
            .expect("version ten must be recorded");

        super::super::initialize_connection(&mut connection)
            .expect("fingerprint migration must apply");

        let (fingerprint, bytes) = connection
            .query_row(
                "SELECT descriptor_fingerprint, content FROM game_cover_assets
                 WHERE game_source_product_id = 101",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .expect("migrated asset must remain");
        assert_eq!(
            fingerprint,
            CoverDescriptorFingerprint::from_descriptor(&descriptor("legacy.png")).as_str()
        );
        assert_eq!(bytes, png().bytes());
    }

    #[test]
    fn descriptor_refresh_invalidates_old_asset_and_rejects_a_late_old_fetch() {
        let database = TemporaryDatabase::new();
        let descriptor_a = descriptor("a.png");
        let descriptor_b = descriptor("b.png");
        let mut snapshots = super::super::SqliteGameSnapshotStore::open(&database.path)
            .expect("snapshot store must open");
        upsert_game_snapshot(&mut snapshots, &snapshot(descriptor_a.clone()))
            .expect("first snapshot must persist");
        drop(snapshots);

        let mut assets =
            SqliteGameCoverAssetStore::open(&database.path).expect("asset store must open");
        let candidate_a = assets
            .cover_backfill_candidates(20)
            .expect("candidate selection must work")
            .pop()
            .expect("descriptor A must be selected");
        assert_eq!(
            assets
                .store_cover_if_current(&candidate_a, &png())
                .expect("A asset must store"),
            CoverBackfillPersistOutcome::Stored
        );
        drop(assets);

        let mut snapshots = super::super::SqliteGameSnapshotStore::open(&database.path)
            .expect("snapshot store must reopen");
        upsert_game_snapshot(&mut snapshots, &snapshot(descriptor_b.clone()))
            .expect("descriptor B refresh must persist");
        drop(snapshots);

        let mut assets =
            SqliteGameCoverAssetStore::open(&database.path).expect("asset store must reopen");
        assert_eq!(
            assets
                .store_cover_if_current(&candidate_a, &png())
                .expect("late A persistence must settle"),
            CoverBackfillPersistOutcome::Stale
        );
        let candidate_b = assets
            .cover_backfill_candidates(20)
            .expect("B must be selected after refresh")
            .pop()
            .expect("descriptor B must be stale/missing");
        assert_eq!(candidate_b.descriptor(), &descriptor_b);
        assert_eq!(
            assets
                .store_cover_if_current(&candidate_b, &png())
                .expect("B asset must store"),
            CoverBackfillPersistOutcome::Stored
        );
        assert!(
            assets
                .cover_backfill_candidates(20)
                .expect("repeated selection must work")
                .is_empty()
        );
    }
}
