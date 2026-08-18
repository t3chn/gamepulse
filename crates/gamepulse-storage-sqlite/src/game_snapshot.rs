use std::fmt;
use std::path::Path;

use gamepulse_application::{CoverDescriptorFingerprint, GameSnapshot, GameSnapshotStore};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};

/// A durable SQLite implementation of the application-owned game snapshot upsert port.
pub struct SqliteGameSnapshotStore {
    connection: Connection,
}

impl SqliteGameSnapshotStore {
    /// Open a file-backed database and apply all embedded storage migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GameSnapshotStoreError> {
        let mut connection = Connection::open(path).map_err(GameSnapshotStoreError::database)?;
        super::initialize_connection(&mut connection).map_err(GameSnapshotStoreError::migration)?;
        Ok(Self { connection })
    }

    /// Open an isolated in-memory database and apply all embedded storage migrations.
    pub fn open_in_memory() -> Result<Self, GameSnapshotStoreError> {
        let mut connection =
            Connection::open_in_memory().map_err(GameSnapshotStoreError::database)?;
        super::initialize_connection(&mut connection).map_err(GameSnapshotStoreError::migration)?;
        Ok(Self { connection })
    }

    fn upsert_game_snapshot(
        &mut self,
        snapshot: &GameSnapshot,
    ) -> Result<(), GameSnapshotStoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(GameSnapshotStoreError::database)?;
        upsert_snapshot_in_transaction(&transaction, snapshot)?;
        transaction
            .commit()
            .map_err(GameSnapshotStoreError::database)
    }

    #[cfg(test)]
    fn install_platform_insert_failure_for_test(&self) {
        self.connection
            .execute_batch(
                "CREATE TRIGGER fail_game_platform_insert
                 BEFORE INSERT ON game_platform_scores
                 BEGIN
                     SELECT RAISE(ABORT, 'test game platform insert failure');
                 END;",
            )
            .expect("test trigger must install");
    }
}

pub(crate) fn upsert_snapshot_in_transaction(
    transaction: &Transaction<'_>,
    snapshot: &GameSnapshot,
) -> Result<(), GameSnapshotStoreError> {
    let source_product_id = sqlite_identifier(
        snapshot.source_product_id().value(),
        "game source product identity",
    )?;
    let cover = snapshot.cover();
    transaction
        .execute(
            "INSERT INTO games (
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
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(source_product_id) DO UPDATE SET
                source_slug = excluded.source_slug,
                title = excluded.title,
                description = excluded.description,
                cover_bucket_path = excluded.cover_bucket_path,
                cover_bucket_type = excluded.cover_bucket_type,
                cover_filename = excluded.cover_filename,
                cover_kind = excluded.cover_kind,
                video_url = excluded.video_url,
                public_cover_url = COALESCE(excluded.public_cover_url, games.public_cover_url)",
            params![
                source_product_id,
                snapshot.source_slug(),
                snapshot.title(),
                snapshot.description(),
                cover.map(|value| value.bucket_path()),
                cover.map(|value| value.bucket_type()),
                cover.map(|value| value.filename()),
                cover.map(|value| value.kind()),
                snapshot.video().map(|value| value.as_str()),
                snapshot.public_cover_url().map(|value| value.as_str()),
            ],
        )
        .map_err(GameSnapshotStoreError::database)?;
    match cover {
        Some(cover) => {
            let fingerprint = CoverDescriptorFingerprint::from_descriptor(cover);
            transaction
                .execute(
                    "DELETE FROM game_cover_assets
                     WHERE game_source_product_id = ?1
                       AND descriptor_fingerprint <> ?2",
                    params![source_product_id, fingerprint.as_str()],
                )
                .map_err(GameSnapshotStoreError::database)?;
        }
        None => {
            transaction
                .execute(
                    "DELETE FROM game_cover_assets WHERE game_source_product_id = ?1",
                    params![source_product_id],
                )
                .map_err(GameSnapshotStoreError::database)?;
        }
    }
    transaction
        .execute(
            "DELETE FROM game_platform_scores WHERE game_source_product_id = ?1",
            params![source_product_id],
        )
        .map_err(GameSnapshotStoreError::database)?;
    transaction
        .execute(
            "DELETE FROM game_developers WHERE game_source_product_id = ?1",
            params![source_product_id],
        )
        .map_err(GameSnapshotStoreError::database)?;
    for platform in snapshot.platform_scores() {
        let source_platform_id =
            sqlite_identifier(platform.source_platform_id(), "platform source identity")?;
        transaction
            .execute(
                "INSERT INTO game_platform_scores (
                    game_source_product_id,
                    source_platform_id,
                    source_slug,
                    metascore,
                    userscore
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    source_product_id,
                    source_platform_id,
                    platform.source_slug(),
                    platform.metascore().map(|value| i64::from(value.value())),
                    platform.userscore().map(|value| value.value()),
                ],
            )
            .map_err(GameSnapshotStoreError::database)?;
    }
    for developer in snapshot.developers() {
        transaction
            .execute(
                "INSERT INTO game_developers (game_source_product_id, developer_name)
                 VALUES (?1, ?2)",
                params![source_product_id, developer.as_str()],
            )
            .map_err(GameSnapshotStoreError::database)?;
    }
    Ok(())
}

impl GameSnapshotStore for SqliteGameSnapshotStore {
    type Error = GameSnapshotStoreError;

    fn upsert_snapshot(&mut self, snapshot: &GameSnapshot) -> Result<(), GameSnapshotStoreError> {
        self.upsert_game_snapshot(snapshot)
    }
}

/// The adapter's non-leaking error surface for snapshot persistence and migration failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameSnapshotStoreError {
    message: String,
}

impl GameSnapshotStoreError {
    fn database(error: rusqlite::Error) -> Self {
        Self {
            message: format!("SQLite game snapshot operation failed: {error}"),
        }
    }

    fn migration(_: super::DailyCrawlStateStoreError) -> Self {
        Self {
            message: "SQLite game snapshot migration failed".to_owned(),
        }
    }

    fn identity_out_of_range(field: &'static str) -> Self {
        Self {
            message: format!("{field} exceeds SQLite INTEGER range"),
        }
    }
}

impl fmt::Display for GameSnapshotStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GameSnapshotStoreError {}

fn sqlite_identifier(value: u64, field: &'static str) -> Result<i64, GameSnapshotStoreError> {
    i64::try_from(value).map_err(|_| GameSnapshotStoreError::identity_out_of_range(field))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process;
    use std::sync::atomic::{AtomicU64, Ordering};

    use gamepulse_application::{
        GameCoverDescriptor, GameDeveloper, GamePlatformScore, GamePublicCoverUrl, GameSnapshot,
        GameVideoLink, Metascore, SourceProductId, Userscore, upsert_game_snapshot,
    };
    use rusqlite::params;

    use super::*;

    static NEXT_TEMPORARY_DATABASE: AtomicU64 = AtomicU64::new(0);

    struct TemporaryDatabase {
        path: PathBuf,
    }

    impl TemporaryDatabase {
        fn new(name: &str) -> Self {
            let sequence = NEXT_TEMPORARY_DATABASE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "gamepulse-game-snapshot-{name}-{}-{sequence}.sqlite3",
                process::id()
            ));
            let _ = fs::remove_file(&path);
            Self { path }
        }

        fn open(&self) -> SqliteGameSnapshotStore {
            SqliteGameSnapshotStore::open(&self.path).expect("test snapshot store must open")
        }
    }

    impl Drop for TemporaryDatabase {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn platform(
        id: u64,
        slug: &str,
        metascore: Option<u8>,
        userscore: Option<f64>,
    ) -> GamePlatformScore {
        GamePlatformScore::new(
            id,
            slug,
            metascore.map(|value| Metascore::new(value).expect("test Metascore must be valid")),
            userscore.map(|value| Userscore::new(value).expect("test Userscore must be valid")),
        )
        .expect("test platform must be valid")
    }

    fn snapshot(
        slug: &str,
        title: &str,
        platforms: Vec<GamePlatformScore>,
        developers: &[&str],
    ) -> GameSnapshot {
        GameSnapshot::new(
            SourceProductId::new(101).expect("test product identity must be valid"),
            slug,
            title,
            "Synthetic description",
            Some(
                GameCoverDescriptor::new("products/example", "image", "cover.jpg", "cardImage")
                    .expect("test cover descriptor must be valid"),
            ),
            Some(
                GameVideoLink::new("https://video.example.test/embed")
                    .expect("test video must be valid"),
            ),
            platforms,
            developers
                .iter()
                .map(|value| GameDeveloper::new(*value).expect("test developer must be valid"))
                .collect(),
        )
        .expect("test snapshot must be valid")
    }

    fn initial_snapshot() -> GameSnapshot {
        snapshot(
            "example-game",
            "Example Game",
            vec![
                platform(7, "pc", Some(82), Some(8.4)),
                platform(8, "console", None, None),
            ],
            &["Example Studio", "Second Studio"],
        )
    }

    fn replacement_snapshot() -> GameSnapshot {
        snapshot(
            "renamed-example-game",
            "Renamed Example Game",
            vec![platform(7, "pc", Some(90), Some(9.0))],
            &["Replacement Studio"],
        )
        .with_public_cover_url(Some(
            GamePublicCoverUrl::new("https://www.metacritic.com/images/replacement.jpg")
                .expect("test public cover URL must be valid"),
        ))
    }

    fn row_count(store: &SqliteGameSnapshotStore, table: &str) -> i64 {
        store
            .connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("test row count must load")
    }

    fn source_slug(store: &SqliteGameSnapshotStore) -> String {
        store
            .connection
            .query_row(
                "SELECT source_slug FROM games WHERE source_product_id = 101",
                [],
                |row| row.get(0),
            )
            .expect("test game slug must load")
    }

    fn public_cover_url(store: &SqliteGameSnapshotStore) -> Option<String> {
        store
            .connection
            .query_row(
                "SELECT public_cover_url FROM games WHERE source_product_id = 101",
                [],
                |row| row.get(0),
            )
            .expect("test public cover URL must load")
    }

    fn platform_ids(store: &SqliteGameSnapshotStore) -> Vec<i64> {
        let mut statement = store
            .connection
            .prepare(
                "SELECT source_platform_id
                 FROM game_platform_scores
                 WHERE game_source_product_id = 101
                 ORDER BY source_platform_id",
            )
            .expect("test platform query must prepare");
        statement
            .query_map([], |row| row.get(0))
            .expect("test platform query must execute")
            .collect::<Result<Vec<_>, _>>()
            .expect("test platform rows must decode")
    }

    fn developer_names(store: &SqliteGameSnapshotStore) -> Vec<String> {
        let mut statement = store
            .connection
            .prepare(
                "SELECT developer_name
                 FROM game_developers
                 WHERE game_source_product_id = 101
                 ORDER BY developer_name",
            )
            .expect("test developer query must prepare");
        statement
            .query_map([], |row| row.get(0))
            .expect("test developer query must execute")
            .collect::<Result<Vec<_>, _>>()
            .expect("test developer rows must decode")
    }

    #[test]
    fn numeric_identity_updates_mutable_slug_and_identical_replay_is_idempotent() {
        let mut store = SqliteGameSnapshotStore::open_in_memory().expect("store must open");
        let first = initial_snapshot();
        let replacement = replacement_snapshot();

        upsert_game_snapshot(&mut store, &first).expect("first snapshot must persist");
        upsert_game_snapshot(&mut store, &replacement).expect("same numeric identity must update");
        upsert_game_snapshot(&mut store, &replacement).expect("identical replay must persist");

        assert_eq!(row_count(&store, "games"), 1);
        assert_eq!(source_slug(&store), "renamed-example-game");
        assert_eq!(
            public_cover_url(&store).as_deref(),
            Some("https://www.metacritic.com/images/replacement.jpg")
        );
        assert_eq!(platform_ids(&store), [7]);
        assert_eq!(developer_names(&store), ["Replacement Studio"]);
    }

    #[test]
    fn replacement_removes_stale_platform_scores_and_developers() {
        let mut store = SqliteGameSnapshotStore::open_in_memory().expect("store must open");

        upsert_game_snapshot(&mut store, &initial_snapshot())
            .expect("initial snapshot must persist");
        upsert_game_snapshot(&mut store, &replacement_snapshot())
            .expect("replacement snapshot must persist");

        assert_eq!(row_count(&store, "game_platform_scores"), 1);
        assert_eq!(platform_ids(&store), [7]);
        assert_eq!(row_count(&store, "game_developers"), 1);
        assert_eq!(developer_names(&store), ["Replacement Studio"]);
    }

    #[test]
    fn available_public_cover_survives_a_later_snapshot_without_optional_enrichment() {
        let mut store = SqliteGameSnapshotStore::open_in_memory().expect("store must open");

        upsert_game_snapshot(&mut store, &replacement_snapshot())
            .expect("enriched snapshot must persist");
        upsert_game_snapshot(&mut store, &initial_snapshot())
            .expect("mandatory snapshot without optional cover must persist");

        assert_eq!(
            public_cover_url(&store).as_deref(),
            Some("https://www.metacritic.com/images/replacement.jpg")
        );
    }

    #[test]
    fn forced_child_write_failure_rolls_back_the_entire_replacement() {
        let mut store = SqliteGameSnapshotStore::open_in_memory().expect("store must open");
        upsert_game_snapshot(&mut store, &initial_snapshot())
            .expect("initial snapshot must persist");
        store.install_platform_insert_failure_for_test();

        assert!(upsert_game_snapshot(&mut store, &replacement_snapshot()).is_err());

        assert_eq!(source_slug(&store), "example-game");
        assert_eq!(public_cover_url(&store), None);
        assert_eq!(platform_ids(&store), [7, 8]);
        assert_eq!(developer_names(&store), ["Example Studio", "Second Studio"]);
    }

    #[test]
    fn file_backed_snapshot_survives_close_and_reopen() {
        let database = TemporaryDatabase::new("reopen");
        {
            let mut store = database.open();
            upsert_game_snapshot(&mut store, &replacement_snapshot())
                .expect("snapshot must persist before close");
        }

        let reopened = database.open();
        assert_eq!(row_count(&reopened, "games"), 1);
        assert_eq!(source_slug(&reopened), "renamed-example-game");
        assert_eq!(
            public_cover_url(&reopened).as_deref(),
            Some("https://www.metacritic.com/images/replacement.jpg")
        );
        assert_eq!(platform_ids(&reopened), [7]);
        assert_eq!(developer_names(&reopened), ["Replacement Studio"]);
    }

    #[test]
    fn out_of_range_domain_identity_cannot_be_coerced_into_sqlite() {
        let mut store = SqliteGameSnapshotStore::open_in_memory().expect("store must open");
        let snapshot = GameSnapshot::new(
            SourceProductId::new(u64::MAX).expect("domain identity remains numeric"),
            "maximum",
            "Maximum",
            "Synthetic description",
            None,
            None,
            Vec::new(),
            Vec::new(),
        )
        .expect("test snapshot must be valid");

        assert!(upsert_game_snapshot(&mut store, &snapshot).is_err());
        assert_eq!(row_count(&store, "games"), 0);
    }

    #[test]
    fn snapshot_schema_rejects_invalid_child_scores() {
        let store = SqliteGameSnapshotStore::open_in_memory().expect("store must open");

        assert!(
            store
                .connection
                .execute(
                    "INSERT INTO games (source_product_id, source_slug, title, description)
                 VALUES (999, 'score-check', 'Score Check', 'Synthetic')",
                    [],
                )
                .is_ok()
        );
        assert!(
            store
                .connection
                .execute(
                    "INSERT INTO game_platform_scores (
                    game_source_product_id, source_platform_id, source_slug, metascore, userscore
                 ) VALUES (999, 1, 'pc', 101, 8.4)",
                    params![],
                )
                .is_err()
        );
    }
}
