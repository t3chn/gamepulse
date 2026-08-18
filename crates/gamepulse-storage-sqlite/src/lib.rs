#![forbid(unsafe_code)]

//! SQLite implementations of GamePulse application ports.

mod acceptance_cycle;
mod catalogue;
mod game_snapshot;
mod job_queue;
mod review_summary;
mod run_progress;

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use gamepulse_application::{
    BrowseCursor, BrowseProgress, CrawlDayKey, DailyCrawlCommit, DailyCrawlState,
    DailyCrawlStatePort, DiscoveryCandidate, ServiceReadinessPort, SourceProductId,
};
use rusqlite::{
    Connection, Error, OpenFlags, OptionalExtension, Params, Transaction, TransactionBehavior, ffi,
    params,
};

pub use acceptance_cycle::{AcceptanceCycleReadStoreError, SqliteAcceptanceCycleStore};
pub use catalogue::{GameCatalogueReadStoreError, SqliteGameCatalogueReadStore};
pub use game_snapshot::{GameSnapshotStoreError, SqliteGameSnapshotStore};
pub use job_queue::{JobStoreError, SqliteJobStore};
pub use review_summary::{ReviewSummaryStoreError, SqliteReviewSummaryStore};
pub use run_progress::{RunProgressStoreError, SqliteRunProgressStore};

const DAILY_CRAWL_SCHEMA_VERSION: i64 = 1;
const JOB_QUEUE_SCHEMA_VERSION: i64 = 2;
const GAME_SNAPSHOT_SCHEMA_VERSION: i64 = 3;
const REVIEW_SUMMARY_SCHEMA_VERSION: i64 = 4;
const PUBLIC_COVER_URL_SCHEMA_VERSION: i64 = 5;
const REVIEW_EXCERPT_POLARITY_SCHEMA_VERSION: i64 = 6;
const RETRY_BACKOFF_AND_SOURCE_PACING_SCHEMA_VERSION: i64 = 7;
const DURABLE_RUNS_SCHEMA_VERSION: i64 = 8;
const SCHEMA_VERSION: i64 = 9;
const DAILY_CRAWL_MIGRATION_0001: &str = include_str!("../migrations/0001_daily_crawl_state.sql");
const JOB_QUEUE_MIGRATION_0002: &str = include_str!("../migrations/0002_job_queue.sql");
const GAME_SNAPSHOT_MIGRATION_0003: &str = include_str!("../migrations/0003_game_snapshots.sql");
const REVIEW_SUMMARY_MIGRATION_0004: &str = include_str!("../migrations/0004_review_summaries.sql");
const PUBLIC_COVER_URL_MIGRATION_0005: &str =
    include_str!("../migrations/0005_public_cover_url.sql");
const REVIEW_EXCERPT_POLARITY_MIGRATION_0006: &str =
    include_str!("../migrations/0006_review_excerpt_polarity.sql");
const RETRY_BACKOFF_AND_SOURCE_PACING_MIGRATION_0007: &str =
    include_str!("../migrations/0007_retry_backoff_and_source_pacing.sql");
const DURABLE_RUNS_MIGRATION_0008: &str = concat!(
    include_str!("../migrations/0008_durable_runs.sql"),
    include_str!("../migrations/0009_source_unavailable_rejection.sql")
);
const SOURCE_UNAVAILABLE_REJECTION_MIGRATION_0009: &str =
    include_str!("../migrations/0009_source_unavailable_rejection.sql");

/// Read-only SQLite readiness adapter for the configured persistent database.
///
/// It intentionally reopens the path for every check so a replacement, removal,
/// or broken mount cannot be hidden by the process's long-lived write handles.
/// It checks database integrity, the exact migration version, and the required
/// schema structure without invoking startup's write-only constraint probes.
pub struct SqliteReadinessProbe {
    path: PathBuf,
}

impl SqliteReadinessProbe {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }
}

impl ServiceReadinessPort for SqliteReadinessProbe {
    type Error = SqliteReadinessError;

    fn check_readiness(&self) -> Result<(), SqliteReadinessError> {
        let connection = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| SqliteReadinessError)?;
        let version = connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .map_err(|_| SqliteReadinessError)?;
        if version != SCHEMA_VERSION {
            return Err(SqliteReadinessError);
        }
        validate_required_schema_structure(&connection).map_err(|_| SqliteReadinessError)?;
        let integrity = connection
            .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
            .map_err(|_| SqliteReadinessError)?;
        if integrity == "ok" {
            Ok(())
        } else {
            Err(SqliteReadinessError)
        }
    }
}

/// Opaque readiness failure: HTTP callers receive no database or path detail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SqliteReadinessError;

impl fmt::Display for SqliteReadinessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SQLite readiness check failed")
    }
}

impl std::error::Error for SqliteReadinessError {}

type ForeignKeyDefinition<'a> = (
    i64,
    i64,
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    &'a str,
);

#[derive(Clone, Copy, Debug)]
enum ExpectedConstraint {
    Check,
    ForeignKey,
    PrimaryKey,
    Unique,
}

impl ExpectedConstraint {
    fn extended_code(self) -> i32 {
        match self {
            Self::Check => ffi::SQLITE_CONSTRAINT_CHECK,
            Self::ForeignKey => ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
            Self::PrimaryKey => ffi::SQLITE_CONSTRAINT_PRIMARYKEY,
            Self::Unique => ffi::SQLITE_CONSTRAINT_UNIQUE,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Check => "CHECK",
            Self::ForeignKey => "FOREIGN KEY",
            Self::PrimaryKey => "PRIMARY KEY",
            Self::Unique => "UNIQUE",
        }
    }
}

/// A durable SQLite adapter for the application-owned daily-crawl state port.
pub struct SqliteDailyCrawlStateStore {
    connection: Connection,
}

impl SqliteDailyCrawlStateStore {
    /// Open a file-backed database and apply the embedded daily-crawl migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DailyCrawlStateStoreError> {
        let connection = Connection::open(path).map_err(DailyCrawlStateStoreError::database)?;
        Self::from_connection(connection)
    }

    /// Open an isolated in-memory database and apply the embedded daily-crawl migrations.
    pub fn open_in_memory() -> Result<Self, DailyCrawlStateStoreError> {
        let connection =
            Connection::open_in_memory().map_err(DailyCrawlStateStoreError::database)?;
        Self::from_connection(connection)
    }

    fn from_connection(mut connection: Connection) -> Result<Self, DailyCrawlStateStoreError> {
        initialize_connection(&mut connection)?;
        Ok(Self { connection })
    }

    fn commit_daily_crawl(
        &mut self,
        commit: DailyCrawlCommit,
    ) -> Result<(), DailyCrawlStateStoreError> {
        let transaction = self
            .connection
            .transaction()
            .map_err(DailyCrawlStateStoreError::database)?;
        let current_state = load_daily_crawl_state(&transaction, commit.state().day())?;
        verify_commit_against_current_state(current_state.as_ref(), &commit)?;
        let day = commit.state().day().as_str();
        write_state(&transaction, commit.state())?;
        write_selected_or_processed(&transaction, commit.state())?;
        write_selected_candidates(&transaction, day, commit.selected())?;
        for request in commit.jobs() {
            job_queue::enqueue_derived_request(&transaction, request)
                .map_err(|_| DailyCrawlStateStoreError::job_enqueue())?;
        }
        transaction
            .commit()
            .map_err(DailyCrawlStateStoreError::database)
    }

    #[cfg(test)]
    fn selected_candidates_for_day(&self, day: &CrawlDayKey) -> Vec<(u64, String)> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT source_product_id, source_slug
                 FROM crawl_day_selected_candidates
                 WHERE day_key = ?1
                 ORDER BY source_product_id",
            )
            .expect("test query must prepare");
        statement
            .query_map(params![day.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("test query must execute")
            .map(|row| {
                let (source_product_id, source_slug) = row.expect("test row must decode");
                (
                    parse_canonical_u64(&source_product_id, "test source product identity")
                        .expect("test source product identity must be valid"),
                    source_slug,
                )
            })
            .collect()
    }

    #[cfg(test)]
    fn install_candidate_insert_failure_for_test(&self) {
        self.connection
            .execute_batch(
                "CREATE TRIGGER fail_selected_candidate_insert
                 BEFORE INSERT ON crawl_day_selected_candidates
                 BEGIN
                     SELECT RAISE(ABORT, 'test selected candidate insert failure');
                 END;",
            )
            .expect("test trigger must install");
    }

    #[cfg(test)]
    fn install_job_insert_failure_for_test(&self) {
        self.connection
            .execute_batch(
                "CREATE TRIGGER fail_source_ingestion_job_insert
                 BEFORE INSERT ON jobs
                 BEGIN
                     SELECT RAISE(ABORT, 'test source ingestion job insert failure');
                 END;",
            )
            .expect("test trigger must install");
    }

    #[cfg(test)]
    fn job_identities(&self) -> Vec<String> {
        let mut statement = self
            .connection
            .prepare("SELECT job_identity FROM jobs ORDER BY job_identity")
            .expect("test job query must prepare");
        statement
            .query_map([], |row| row.get(0))
            .expect("test job query must execute")
            .collect::<Result<Vec<_>, _>>()
            .expect("test job rows must decode")
    }

    #[cfg(test)]
    fn job_request_for(&self, identity: &str) -> Option<(String, String, u32)> {
        self.connection
            .query_row(
                "SELECT job_type, work_ref, max_attempts FROM jobs WHERE job_identity = ?1",
                params![identity],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        u32::try_from(row.get::<_, i64>(2)?)
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, 0))?,
                    ))
                },
            )
            .optional()
            .expect("test job request query must execute")
    }
}

impl DailyCrawlStatePort for SqliteDailyCrawlStateStore {
    type Error = DailyCrawlStateStoreError;

    fn load(
        &mut self,
        day: &CrawlDayKey,
    ) -> Result<Option<DailyCrawlState>, DailyCrawlStateStoreError> {
        load_daily_crawl_state(&self.connection, day)
    }

    fn commit(&mut self, commit: DailyCrawlCommit) -> Result<(), DailyCrawlStateStoreError> {
        self.commit_daily_crawl(commit)
    }
}

/// The adapter's non-leaking error surface for database, validation, and malformed-data failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DailyCrawlStateStoreError {
    message: String,
}

impl DailyCrawlStateStoreError {
    fn database(error: rusqlite::Error) -> Self {
        Self {
            message: format!("SQLite daily crawl state operation failed: {error}"),
        }
    }

    fn job_enqueue() -> Self {
        Self {
            message: "SQLite daily crawl job enqueue failed".to_owned(),
        }
    }

    fn invalid_commit(message: impl Into<String>) -> Self {
        Self {
            message: format!("invalid daily crawl commit: {}", message.into()),
        }
    }

    fn malformed(message: impl Into<String>) -> Self {
        Self {
            message: format!("malformed persisted daily crawl state: {}", message.into()),
        }
    }
}

impl fmt::Display for DailyCrawlStateStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DailyCrawlStateStoreError {}

pub(crate) fn initialize_connection(
    connection: &mut Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(DailyCrawlStateStoreError::database)?;
    migrate(connection)
}

fn migrate(connection: &mut Connection) -> Result<(), DailyCrawlStateStoreError> {
    let version = connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .map_err(DailyCrawlStateStoreError::database)?;

    match version {
        0 => {
            let transaction = connection
                .transaction()
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(DAILY_CRAWL_MIGRATION_0001)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(JOB_QUEUE_MIGRATION_0002)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(GAME_SNAPSHOT_MIGRATION_0003)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(REVIEW_SUMMARY_MIGRATION_0004)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(PUBLIC_COVER_URL_MIGRATION_0005)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(REVIEW_EXCERPT_POLARITY_MIGRATION_0006)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(RETRY_BACKOFF_AND_SOURCE_PACING_MIGRATION_0007)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(DURABLE_RUNS_MIGRATION_0008)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .commit()
                .map_err(DailyCrawlStateStoreError::database)
        }
        DAILY_CRAWL_SCHEMA_VERSION => {
            validate_daily_crawl_schema(connection)?;
            let transaction = connection
                .transaction()
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(JOB_QUEUE_MIGRATION_0002)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(GAME_SNAPSHOT_MIGRATION_0003)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(REVIEW_SUMMARY_MIGRATION_0004)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(PUBLIC_COVER_URL_MIGRATION_0005)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(REVIEW_EXCERPT_POLARITY_MIGRATION_0006)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(RETRY_BACKOFF_AND_SOURCE_PACING_MIGRATION_0007)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(DURABLE_RUNS_MIGRATION_0008)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .commit()
                .map_err(DailyCrawlStateStoreError::database)
        }
        JOB_QUEUE_SCHEMA_VERSION => {
            validate_daily_crawl_schema(connection)?;
            validate_job_queue_schema_before_retry_pacing(connection)?;
            let transaction = connection
                .transaction()
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(GAME_SNAPSHOT_MIGRATION_0003)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(REVIEW_SUMMARY_MIGRATION_0004)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(PUBLIC_COVER_URL_MIGRATION_0005)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(REVIEW_EXCERPT_POLARITY_MIGRATION_0006)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(RETRY_BACKOFF_AND_SOURCE_PACING_MIGRATION_0007)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(DURABLE_RUNS_MIGRATION_0008)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .commit()
                .map_err(DailyCrawlStateStoreError::database)
        }
        GAME_SNAPSHOT_SCHEMA_VERSION => {
            validate_daily_crawl_schema(connection)?;
            validate_job_queue_schema_before_retry_pacing(connection)?;
            validate_game_snapshot_schema_before_public_cover(connection)?;
            let transaction = connection
                .transaction()
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(REVIEW_SUMMARY_MIGRATION_0004)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(PUBLIC_COVER_URL_MIGRATION_0005)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(REVIEW_EXCERPT_POLARITY_MIGRATION_0006)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(RETRY_BACKOFF_AND_SOURCE_PACING_MIGRATION_0007)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(DURABLE_RUNS_MIGRATION_0008)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .commit()
                .map_err(DailyCrawlStateStoreError::database)
        }
        REVIEW_SUMMARY_SCHEMA_VERSION => {
            validate_daily_crawl_schema(connection)?;
            validate_job_queue_schema_before_retry_pacing(connection)?;
            validate_game_snapshot_schema_before_public_cover(connection)?;
            validate_review_summary_schema_before_polarity(connection)?;
            let transaction = connection
                .transaction()
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(PUBLIC_COVER_URL_MIGRATION_0005)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(REVIEW_EXCERPT_POLARITY_MIGRATION_0006)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(RETRY_BACKOFF_AND_SOURCE_PACING_MIGRATION_0007)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(DURABLE_RUNS_MIGRATION_0008)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .commit()
                .map_err(DailyCrawlStateStoreError::database)
        }
        PUBLIC_COVER_URL_SCHEMA_VERSION => {
            validate_daily_crawl_schema(connection)?;
            validate_job_queue_schema_before_retry_pacing(connection)?;
            validate_game_snapshot_schema(connection)?;
            validate_review_summary_schema_before_polarity(connection)?;
            let transaction = connection
                .transaction()
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(REVIEW_EXCERPT_POLARITY_MIGRATION_0006)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(RETRY_BACKOFF_AND_SOURCE_PACING_MIGRATION_0007)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(DURABLE_RUNS_MIGRATION_0008)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .commit()
                .map_err(DailyCrawlStateStoreError::database)
        }
        REVIEW_EXCERPT_POLARITY_SCHEMA_VERSION => {
            validate_daily_crawl_schema(connection)?;
            validate_job_queue_schema_before_retry_pacing(connection)?;
            validate_game_snapshot_schema(connection)?;
            validate_review_summary_schema(connection)?;
            let transaction = connection
                .transaction()
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(RETRY_BACKOFF_AND_SOURCE_PACING_MIGRATION_0007)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(DURABLE_RUNS_MIGRATION_0008)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .commit()
                .map_err(DailyCrawlStateStoreError::database)
        }
        RETRY_BACKOFF_AND_SOURCE_PACING_SCHEMA_VERSION => {
            validate_daily_crawl_schema(connection)?;
            validate_job_queue_schema(connection)?;
            validate_game_snapshot_schema(connection)?;
            validate_review_summary_schema(connection)?;
            let transaction = connection
                .transaction()
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(DURABLE_RUNS_MIGRATION_0008)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .commit()
                .map_err(DailyCrawlStateStoreError::database)
        }
        DURABLE_RUNS_SCHEMA_VERSION => {
            validate_owned_schema(connection)?;
            let transaction = connection
                .transaction()
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .execute_batch(SOURCE_UNAVAILABLE_REJECTION_MIGRATION_0009)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(DailyCrawlStateStoreError::database)?;
            transaction
                .commit()
                .map_err(DailyCrawlStateStoreError::database)
        }
        SCHEMA_VERSION => Ok(()),
        other => Err(DailyCrawlStateStoreError::malformed(format!(
            "unsupported schema version {other}"
        ))),
    }?;
    validate_owned_schema(connection)
}

fn validate_owned_schema(connection: &mut Connection) -> Result<(), DailyCrawlStateStoreError> {
    validate_daily_crawl_schema(connection)?;
    validate_job_queue_schema(connection)?;
    validate_game_snapshot_schema(connection)?;
    validate_review_summary_schema(connection)?;
    validate_run_progress_schema(connection)
}

/// Read-only portion of the owned-schema validator used by readiness.
///
/// Startup additionally runs write-only constraint probes; readiness must not.
fn validate_required_schema_structure(
    connection: &Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    validate_daily_crawl_schema_structure(connection)?;
    validate_job_queue_schema_structure(connection)?;
    validate_game_snapshot_schema_structure(connection)?;
    validate_review_summary_schema(connection)?;
    validate_run_progress_schema_structure(connection)
}

fn validate_run_progress_schema(
    connection: &mut Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    validate_run_progress_schema_structure(connection)
}

fn validate_run_progress_schema_structure(
    connection: &Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    validate_table_columns(
        connection,
        "runs",
        &[
            ("run_id", "TEXT", 1, 1),
            ("day_key", "TEXT", 1, 0),
            ("target_count", "INTEGER", 1, 0),
            ("accepted_count", "INTEGER", 1, 0),
            ("state", "TEXT", 1, 0),
            ("source_phase", "TEXT", 1, 0),
            ("browse_cursor", "TEXT", 0, 0),
            ("deadline_at", "INTEGER", 1, 0),
            ("version", "INTEGER", 1, 0),
            ("progress_fence", "INTEGER", 1, 0),
            ("next_item_order", "INTEGER", 1, 0),
            ("browse_page_count", "INTEGER", 1, 0),
            ("created_at", "INTEGER", 1, 0),
            ("updated_at", "INTEGER", 1, 0),
        ],
    )?;
    validate_table_columns(
        connection,
        "run_items",
        &[
            ("run_id", "TEXT", 1, 1),
            ("source_product_id", "TEXT", 1, 2),
            ("source_slug", "TEXT", 1, 0),
            ("discovery_order", "INTEGER", 1, 0),
            ("state", "TEXT", 1, 0),
            ("job_identity", "TEXT", 0, 0),
            ("rejection_category", "TEXT", 0, 0),
        ],
    )?;
    validate_table_layout(connection, "runs", false)?;
    validate_table_layout(connection, "run_items", true)?;
    validate_foreign_key_groups(
        connection,
        "run_items",
        &[(
            0,
            0,
            "runs",
            "run_id",
            "run_id",
            "NO ACTION",
            "RESTRICT",
            "NONE",
        )],
    )
}

fn validate_daily_crawl_schema(
    connection: &mut Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    validate_daily_crawl_schema_structure(connection)?;
    validate_constraint_behavior(connection)
}

fn validate_daily_crawl_schema_structure(
    connection: &Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    validate_table_columns(
        connection,
        "crawl_days",
        &[
            ("day_key", "TEXT", 1, 1),
            ("new_releases_completed", "INTEGER", 1, 0),
            ("browse_progress", "TEXT", 1, 0),
            ("browse_cursor", "TEXT", 0, 0),
        ],
    )?;
    validate_table_columns(
        connection,
        "crawl_day_selected_or_processed",
        &[
            ("day_key", "TEXT", 1, 1),
            ("source_product_id", "TEXT", 1, 2),
        ],
    )?;
    validate_table_columns(
        connection,
        "crawl_day_selected_candidates",
        &[
            ("day_key", "TEXT", 1, 1),
            ("source_product_id", "TEXT", 1, 2),
            ("source_slug", "TEXT", 1, 0),
        ],
    )?;

    validate_table_layout(connection, "crawl_days", false)?;
    validate_table_layout(connection, "crawl_day_selected_or_processed", true)?;
    validate_table_layout(connection, "crawl_day_selected_candidates", true)?;

    validate_foreign_key_groups(
        connection,
        "crawl_day_selected_or_processed",
        &[(
            0,
            0,
            "crawl_days",
            "day_key",
            "day_key",
            "NO ACTION",
            "RESTRICT",
            "NONE",
        )],
    )?;
    validate_foreign_key_groups(
        connection,
        "crawl_day_selected_candidates",
        &[
            (
                0,
                0,
                "crawl_day_selected_or_processed",
                "day_key",
                "day_key",
                "NO ACTION",
                "RESTRICT",
                "NONE",
            ),
            (
                0,
                1,
                "crawl_day_selected_or_processed",
                "source_product_id",
                "source_product_id",
                "NO ACTION",
                "RESTRICT",
                "NONE",
            ),
        ],
    )?;
    Ok(())
}

fn validate_job_queue_schema(connection: &mut Connection) -> Result<(), DailyCrawlStateStoreError> {
    validate_job_queue_schema_structure(connection)?;
    validate_job_queue_constraint_behavior(connection)
}

fn validate_job_queue_schema_before_retry_pacing(
    connection: &mut Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    validate_table_columns(
        connection,
        "jobs",
        &[
            ("job_identity", "TEXT", 1, 1),
            ("job_type", "TEXT", 1, 0),
            ("work_ref", "TEXT", 1, 0),
            ("max_attempts", "INTEGER", 1, 0),
            ("attempt_count", "INTEGER", 1, 0),
            ("state", "TEXT", 1, 0),
            ("created_at", "INTEGER", 1, 0),
            ("updated_at", "INTEGER", 1, 0),
            ("claimed_by", "TEXT", 0, 0),
            ("lease_expires_at", "INTEGER", 0, 0),
            ("claim_token", "INTEGER", 1, 0),
            ("terminal_at", "INTEGER", 0, 0),
            ("last_error", "TEXT", 0, 0),
        ],
    )?;
    validate_table_columns(
        connection,
        "job_attempts",
        &[
            ("job_identity", "TEXT", 1, 1),
            ("attempt_number", "INTEGER", 1, 0),
            ("claim_token", "INTEGER", 1, 2),
            ("worker_id", "TEXT", 1, 0),
            ("started_at", "INTEGER", 1, 0),
            ("finished_at", "INTEGER", 0, 0),
            ("outcome", "TEXT", 1, 0),
            ("error", "TEXT", 0, 0),
        ],
    )?;
    validate_table_layout(connection, "jobs", false)?;
    validate_table_layout(connection, "job_attempts", true)?;
    validate_foreign_key_groups(
        connection,
        "job_attempts",
        &[(
            0,
            0,
            "jobs",
            "job_identity",
            "job_identity",
            "NO ACTION",
            "RESTRICT",
            "NONE",
        )],
    )?;
    validate_job_queue_constraint_behavior(connection)
}

fn validate_job_queue_schema_structure(
    connection: &Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    validate_table_columns(
        connection,
        "jobs",
        &[
            ("job_identity", "TEXT", 1, 1),
            ("job_type", "TEXT", 1, 0),
            ("work_ref", "TEXT", 1, 0),
            ("max_attempts", "INTEGER", 1, 0),
            ("attempt_count", "INTEGER", 1, 0),
            ("state", "TEXT", 1, 0),
            ("created_at", "INTEGER", 1, 0),
            ("updated_at", "INTEGER", 1, 0),
            ("claimed_by", "TEXT", 0, 0),
            ("lease_expires_at", "INTEGER", 0, 0),
            ("claim_token", "INTEGER", 1, 0),
            ("terminal_at", "INTEGER", 0, 0),
            ("last_error", "TEXT", 0, 0),
            ("retry_not_before", "INTEGER", 0, 0),
        ],
    )?;
    validate_table_columns(
        connection,
        "job_attempts",
        &[
            ("job_identity", "TEXT", 1, 1),
            ("attempt_number", "INTEGER", 1, 0),
            ("claim_token", "INTEGER", 1, 2),
            ("worker_id", "TEXT", 1, 0),
            ("started_at", "INTEGER", 1, 0),
            ("finished_at", "INTEGER", 0, 0),
            ("outcome", "TEXT", 1, 0),
            ("error", "TEXT", 0, 0),
        ],
    )?;
    validate_table_columns(
        connection,
        "job_lane_pacing",
        &[
            ("lane_key", "TEXT", 1, 1),
            ("next_claim_at", "INTEGER", 1, 0),
        ],
    )?;

    validate_table_layout(connection, "jobs", false)?;
    validate_table_layout(connection, "job_attempts", true)?;
    validate_table_layout(connection, "job_lane_pacing", true)?;
    validate_foreign_key_groups(
        connection,
        "job_attempts",
        &[(
            0,
            0,
            "jobs",
            "job_identity",
            "job_identity",
            "NO ACTION",
            "RESTRICT",
            "NONE",
        )],
    )?;
    Ok(())
}

fn validate_game_snapshot_schema(
    connection: &mut Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    validate_game_snapshot_schema_structure(connection)?;
    validate_game_snapshot_constraint_behavior(connection)?;
    validate_public_cover_url_constraint_behavior(connection)
}

fn validate_game_snapshot_schema_structure(
    connection: &Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    validate_table_columns(
        connection,
        "games",
        &[
            ("source_product_id", "INTEGER", 1, 1),
            ("source_slug", "TEXT", 1, 0),
            ("title", "TEXT", 1, 0),
            ("description", "TEXT", 1, 0),
            ("cover_bucket_path", "TEXT", 0, 0),
            ("cover_bucket_type", "TEXT", 0, 0),
            ("cover_filename", "TEXT", 0, 0),
            ("cover_kind", "TEXT", 0, 0),
            ("video_url", "TEXT", 0, 0),
            ("public_cover_url", "TEXT", 0, 0),
        ],
    )?;
    validate_game_snapshot_schema_relations_and_constraints_structure(connection)
}

fn validate_game_snapshot_schema_before_public_cover(
    connection: &mut Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    validate_game_snapshot_schema_before_public_cover_structure(connection)?;
    validate_game_snapshot_constraint_behavior(connection)
}

fn validate_game_snapshot_schema_before_public_cover_structure(
    connection: &Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    validate_table_columns(
        connection,
        "games",
        &[
            ("source_product_id", "INTEGER", 1, 1),
            ("source_slug", "TEXT", 1, 0),
            ("title", "TEXT", 1, 0),
            ("description", "TEXT", 1, 0),
            ("cover_bucket_path", "TEXT", 0, 0),
            ("cover_bucket_type", "TEXT", 0, 0),
            ("cover_filename", "TEXT", 0, 0),
            ("cover_kind", "TEXT", 0, 0),
            ("video_url", "TEXT", 0, 0),
        ],
    )?;
    validate_game_snapshot_schema_relations_and_constraints_structure(connection)
}

fn validate_game_snapshot_schema_relations_and_constraints_structure(
    connection: &Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    validate_table_columns(
        connection,
        "game_platform_scores",
        &[
            ("game_source_product_id", "INTEGER", 1, 1),
            ("source_platform_id", "INTEGER", 1, 2),
            ("source_slug", "TEXT", 1, 0),
            ("metascore", "INTEGER", 0, 0),
            ("userscore", "REAL", 0, 0),
        ],
    )?;
    validate_table_columns(
        connection,
        "game_developers",
        &[
            ("game_source_product_id", "INTEGER", 1, 1),
            ("developer_name", "TEXT", 1, 2),
        ],
    )?;

    validate_table_layout(connection, "games", false)?;
    validate_table_layout(connection, "game_platform_scores", true)?;
    validate_table_layout(connection, "game_developers", true)?;
    let game_foreign_key = [(
        0,
        0,
        "games",
        "game_source_product_id",
        "source_product_id",
        "NO ACTION",
        "CASCADE",
        "NONE",
    )];
    validate_foreign_key_groups(connection, "game_platform_scores", &game_foreign_key)?;
    validate_foreign_key_groups(connection, "game_developers", &game_foreign_key)?;
    Ok(())
}

fn validate_public_cover_url_constraint_behavior(
    connection: &mut Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(DailyCrawlStateStoreError::database)?;
    let mut reserved_probe_product_ids = BTreeSet::new();
    let valid_product_id =
        next_absent_probe_game_source_id(&transaction, &mut reserved_probe_product_ids)?;
    let validation = expect_constraint_rejection(
        &transaction,
        "blank public cover URL",
        ExpectedConstraint::Check,
        "INSERT INTO games (
            source_product_id, source_slug, title, description, public_cover_url
         ) VALUES (?1, 'blank-public-cover', 'Blank Public Cover', 'Synthetic', '   ')",
        params![valid_product_id],
    );
    let rollback = transaction.rollback();

    match (validation, rollback) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (_, Err(error)) => Err(DailyCrawlStateStoreError::database(error)),
    }
}

fn validate_review_summary_schema(
    connection: &Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    validate_review_summary_schema_inner(connection, true)
}

fn validate_review_summary_schema_before_polarity(
    connection: &Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    validate_review_summary_schema_inner(connection, false)
}

fn validate_review_summary_schema_inner(
    connection: &Connection,
    includes_polarity: bool,
) -> Result<(), DailyCrawlStateStoreError> {
    validate_table_columns(
        connection,
        "review_inputs",
        &[
            ("game_source_product_id", "INTEGER", 1, 1),
            ("review_kind", "TEXT", 1, 2),
            ("content_hash", "TEXT", 1, 0),
            ("refresh_fingerprint", "TEXT", 1, 0),
        ],
    )?;
    if includes_polarity {
        validate_table_columns(
            connection,
            "review_input_excerpts",
            &[
                ("game_source_product_id", "INTEGER", 1, 1),
                ("review_kind", "TEXT", 1, 2),
                ("excerpt_position", "INTEGER", 1, 3),
                ("excerpt", "TEXT", 1, 0),
                ("polarity", "TEXT", 0, 0),
            ],
        )?;
    } else {
        validate_table_columns(
            connection,
            "review_input_excerpts",
            &[
                ("game_source_product_id", "INTEGER", 1, 1),
                ("review_kind", "TEXT", 1, 2),
                ("excerpt_position", "INTEGER", 1, 3),
                ("excerpt", "TEXT", 1, 0),
            ],
        )?;
    }
    validate_table_columns(
        connection,
        "review_summaries",
        &[
            ("game_source_product_id", "INTEGER", 1, 1),
            ("review_kind", "TEXT", 1, 2),
            ("refresh_fingerprint", "TEXT", 1, 0),
            ("state", "TEXT", 1, 0),
        ],
    )?;
    validate_table_columns(
        connection,
        "review_summary_items",
        &[
            ("game_source_product_id", "INTEGER", 1, 1),
            ("review_kind", "TEXT", 1, 2),
            ("sentiment", "TEXT", 1, 3),
            ("item_position", "INTEGER", 1, 4),
            ("item", "TEXT", 1, 0),
        ],
    )?;
    validate_table_layout(connection, "review_inputs", true)?;
    validate_table_layout(connection, "review_input_excerpts", true)?;
    validate_table_layout(connection, "review_summaries", true)?;
    validate_table_layout(connection, "review_summary_items", true)?;
    validate_foreign_key_groups(
        connection,
        "review_inputs",
        &[(
            0,
            0,
            "games",
            "game_source_product_id",
            "source_product_id",
            "NO ACTION",
            "CASCADE",
            "NONE",
        )],
    )?;
    validate_foreign_key_groups(
        connection,
        "review_input_excerpts",
        &[
            (
                0,
                0,
                "review_inputs",
                "game_source_product_id",
                "game_source_product_id",
                "NO ACTION",
                "CASCADE",
                "NONE",
            ),
            (
                0,
                1,
                "review_inputs",
                "review_kind",
                "review_kind",
                "NO ACTION",
                "CASCADE",
                "NONE",
            ),
        ],
    )?;
    validate_foreign_key_groups(
        connection,
        "review_summaries",
        &[(
            0,
            0,
            "games",
            "game_source_product_id",
            "source_product_id",
            "NO ACTION",
            "CASCADE",
            "NONE",
        )],
    )?;
    validate_foreign_key_groups(
        connection,
        "review_summary_items",
        &[
            (
                0,
                0,
                "review_summaries",
                "game_source_product_id",
                "game_source_product_id",
                "NO ACTION",
                "CASCADE",
                "NONE",
            ),
            (
                0,
                1,
                "review_summaries",
                "review_kind",
                "review_kind",
                "NO ACTION",
                "CASCADE",
                "NONE",
            ),
        ],
    )
}

fn validate_table_columns(
    connection: &Connection,
    table: &str,
    expected: &[(&str, &str, i64, i64)],
) -> Result<(), DailyCrawlStateStoreError> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(DailyCrawlStateStoreError::database)?;
    let columns = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(DailyCrawlStateStoreError::database)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(DailyCrawlStateStoreError::database)?;
    let expected = expected
        .iter()
        .map(|(name, data_type, not_null, primary_key)| {
            (
                (*name).to_owned(),
                (*data_type).to_owned(),
                *not_null,
                *primary_key,
            )
        })
        .collect::<Vec<_>>();
    if columns != expected {
        return Err(DailyCrawlStateStoreError::malformed(format!(
            "managed table {table} has incompatible columns or keys"
        )));
    }
    Ok(())
}

fn validate_table_layout(
    connection: &Connection,
    table: &str,
    expected_without_rowid: bool,
) -> Result<(), DailyCrawlStateStoreError> {
    let mut statement = connection
        .prepare("PRAGMA table_list")
        .map_err(DailyCrawlStateStoreError::database)?;
    let tables = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(DailyCrawlStateStoreError::database)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(DailyCrawlStateStoreError::database)?;
    let matching = tables
        .into_iter()
        .filter(|(schema, name, _, _)| schema == "main" && name == table)
        .collect::<Vec<_>>();
    let expected_without_rowid = i64::from(expected_without_rowid);
    if matching.len() != 1 || matching[0].2 != "table" || matching[0].3 != expected_without_rowid {
        return Err(DailyCrawlStateStoreError::malformed(format!(
            "managed table {table} has incompatible rowid layout"
        )));
    }
    Ok(())
}

fn validate_foreign_key_groups(
    connection: &Connection,
    table: &str,
    expected: &[ForeignKeyDefinition<'_>],
) -> Result<(), DailyCrawlStateStoreError> {
    let mut statement = connection
        .prepare(&format!("PRAGMA foreign_key_list({table})"))
        .map_err(DailyCrawlStateStoreError::database)?;
    let foreign_keys = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })
        .map_err(DailyCrawlStateStoreError::database)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(DailyCrawlStateStoreError::database)?;
    let expected = expected
        .iter()
        .map(
            |(id, sequence, referenced_table, from, to, on_update, on_delete, match_name)| {
                (
                    *id,
                    *sequence,
                    (*referenced_table).to_owned(),
                    (*from).to_owned(),
                    (*to).to_owned(),
                    (*on_update).to_owned(),
                    (*on_delete).to_owned(),
                    (*match_name).to_owned(),
                )
            },
        )
        .collect::<Vec<_>>();
    if foreign_keys != expected {
        return Err(DailyCrawlStateStoreError::malformed(format!(
            "managed table {table} has incompatible foreign keys"
        )));
    }
    Ok(())
}

fn validate_constraint_behavior(
    connection: &mut Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(DailyCrawlStateStoreError::database)?;
    let validation = validate_constraints_in_transaction(&transaction);
    let rollback = transaction.rollback();

    match (validation, rollback) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (_, Err(error)) => Err(DailyCrawlStateStoreError::database(error)),
    }
}

fn validate_constraints_in_transaction(
    connection: &Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    ensure_probe_day_key_is_absent(connection, "   ")?;
    let mut reserved_probe_day_keys = BTreeSet::new();
    let check_day_key = next_absent_probe_day_key(connection, &mut reserved_probe_day_keys)?;
    let first_relation_day_key =
        next_absent_probe_day_key(connection, &mut reserved_probe_day_keys)?;
    let second_relation_day_key =
        next_absent_probe_day_key(connection, &mut reserved_probe_day_keys)?;
    let missing_relation_day_key =
        next_absent_probe_day_key(connection, &mut reserved_probe_day_keys)?;

    expect_constraint_rejection(
        connection,
        "blank day key",
        ExpectedConstraint::Check,
        "INSERT INTO crawl_days (day_key, new_releases_completed, browse_progress, browse_cursor)
         VALUES (?1, ?2, ?3, ?4)",
        params!["   ", 0_i64, "initial", None::<&str>],
    )?;
    expect_constraint_rejection(
        connection,
        "invalid completion value",
        ExpectedConstraint::Check,
        "INSERT INTO crawl_days (day_key, new_releases_completed, browse_progress, browse_cursor)
         VALUES (?1, ?2, ?3, ?4)",
        params![&check_day_key, 2_i64, "initial", None::<&str>],
    )?;
    expect_constraint_rejection(
        connection,
        "unknown browse progress",
        ExpectedConstraint::Check,
        "INSERT INTO crawl_days (day_key, new_releases_completed, browse_progress, browse_cursor)
         VALUES (?1, ?2, ?3, ?4)",
        params![&check_day_key, 0_i64, "unknown", None::<&str>],
    )?;
    expect_constraint_rejection(
        connection,
        "continue without cursor",
        ExpectedConstraint::Check,
        "INSERT INTO crawl_days (day_key, new_releases_completed, browse_progress, browse_cursor)
         VALUES (?1, ?2, ?3, ?4)",
        params![&check_day_key, 0_i64, "continue", None::<&str>],
    )?;
    expect_constraint_rejection(
        connection,
        "initial with cursor",
        ExpectedConstraint::Check,
        "INSERT INTO crawl_days (day_key, new_releases_completed, browse_progress, browse_cursor)
         VALUES (?1, ?2, ?3, ?4)",
        params![&check_day_key, 0_i64, "initial", "1"],
    )?;
    expect_constraint_rejection(
        connection,
        "exhausted with cursor",
        ExpectedConstraint::Check,
        "INSERT INTO crawl_days (day_key, new_releases_completed, browse_progress, browse_cursor)
         VALUES (?1, ?2, ?3, ?4)",
        params![&check_day_key, 0_i64, "exhausted", "1"],
    )?;

    connection
        .execute(
            "INSERT INTO crawl_days (day_key, new_releases_completed, browse_progress, browse_cursor)
             VALUES (?1, ?2, ?3, ?4)",
            params![&first_relation_day_key, 0_i64, "initial", None::<&str>],
        )
        .map_err(DailyCrawlStateStoreError::database)?;
    expect_constraint_rejection(
        connection,
        "empty source product identity",
        ExpectedConstraint::Check,
        "INSERT INTO crawl_day_selected_or_processed (day_key, source_product_id)
         VALUES (?1, ?2)",
        params![&first_relation_day_key, ""],
    )?;
    connection
        .execute(
            "INSERT INTO crawl_day_selected_or_processed (day_key, source_product_id)
             VALUES (?1, ?2)",
            params![&first_relation_day_key, "1"],
        )
        .map_err(DailyCrawlStateStoreError::database)?;
    connection
        .execute(
            "INSERT INTO crawl_days (day_key, new_releases_completed, browse_progress, browse_cursor)
             VALUES (?1, ?2, ?3, ?4)",
            params![&second_relation_day_key, 0_i64, "initial", None::<&str>],
        )
        .map_err(DailyCrawlStateStoreError::database)?;
    connection
        .execute(
            "INSERT INTO crawl_day_selected_or_processed (day_key, source_product_id)
             VALUES (?1, ?2)",
            params![&second_relation_day_key, "2"],
        )
        .map_err(DailyCrawlStateStoreError::database)?;
    expect_constraint_rejection(
        connection,
        "empty selected slug",
        ExpectedConstraint::Check,
        "INSERT INTO crawl_day_selected_candidates (day_key, source_product_id, source_slug)
         VALUES (?1, ?2, ?3)",
        params![&first_relation_day_key, "1", ""],
    )?;
    expect_constraint_rejection(
        connection,
        "active day foreign key",
        ExpectedConstraint::ForeignKey,
        "INSERT INTO crawl_day_selected_or_processed (day_key, source_product_id)
         VALUES (?1, ?2)",
        params![&missing_relation_day_key, "2"],
    )?;
    expect_constraint_rejection(
        connection,
        "active composite candidate foreign key",
        ExpectedConstraint::ForeignKey,
        "INSERT INTO crawl_day_selected_candidates (day_key, source_product_id, source_slug)
         VALUES (?1, ?2, ?3)",
        params![&first_relation_day_key, "2", "cross-pair"],
    )
}

fn validate_job_queue_constraint_behavior(
    connection: &mut Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(DailyCrawlStateStoreError::database)?;
    let validation = validate_job_queue_constraints_in_transaction(&transaction);
    let rollback = transaction.rollback();

    match (validation, rollback) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (_, Err(error)) => Err(DailyCrawlStateStoreError::database(error)),
    }
}

fn validate_job_queue_constraints_in_transaction(
    connection: &Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    let mut reserved_probe_job_identities = BTreeSet::new();
    let valid_job_identity =
        next_absent_probe_job_identity(connection, &mut reserved_probe_job_identities)?;
    let missing_job_identity =
        next_absent_probe_job_identity(connection, &mut reserved_probe_job_identities)?;

    expect_constraint_rejection(
        connection,
        "blank job identity",
        ExpectedConstraint::Check,
        "INSERT INTO jobs (
            job_identity, job_type, work_ref, max_attempts, attempt_count, state,
            created_at, updated_at, claimed_by, lease_expires_at, claim_token, terminal_at,
            last_error
         ) VALUES (?1, 'queue', 'work', 1, 0, 'ready', 1, 1, NULL, NULL, 0, NULL, NULL)",
        params!["   "],
    )?;
    expect_constraint_rejection(
        connection,
        "unknown job state",
        ExpectedConstraint::Check,
        "INSERT INTO jobs (
            job_identity, job_type, work_ref, max_attempts, attempt_count, state,
            created_at, updated_at, claimed_by, lease_expires_at, claim_token, terminal_at,
            last_error
         ) VALUES (?1, 'queue', 'work', 1, 0, 'unknown', 1, 1, NULL, NULL, 0, NULL, NULL)",
        params![&valid_job_identity],
    )?;
    expect_constraint_rejection(
        connection,
        "claimed job without owner",
        ExpectedConstraint::Check,
        "INSERT INTO jobs (
            job_identity, job_type, work_ref, max_attempts, attempt_count, state,
            created_at, updated_at, claimed_by, lease_expires_at, claim_token, terminal_at,
            last_error
         ) VALUES (?1, 'queue', 'work', 1, 1, 'claimed', 1, 1, NULL, 2, 1, NULL, NULL)",
        params![&valid_job_identity],
    )?;
    expect_constraint_rejection(
        connection,
        "job timestamp regression",
        ExpectedConstraint::Check,
        "INSERT INTO jobs (
            job_identity, job_type, work_ref, max_attempts, attempt_count, state,
            created_at, updated_at, claimed_by, lease_expires_at, claim_token, terminal_at,
            last_error
         ) VALUES (?1, 'queue', 'work', 1, 0, 'ready', 2, 1, NULL, NULL, 0, NULL, NULL)",
        params![&valid_job_identity],
    )?;
    connection
        .execute(
            "INSERT INTO jobs (
                job_identity, job_type, work_ref, max_attempts, attempt_count, state,
                created_at, updated_at, claimed_by, lease_expires_at, claim_token, terminal_at,
                last_error
             ) VALUES (?1, 'queue', 'work', 1, 0, 'ready', 1, 1, NULL, NULL, 0, NULL, NULL)",
            params![&valid_job_identity],
        )
        .map_err(DailyCrawlStateStoreError::database)?;
    expect_constraint_rejection(
        connection,
        "zero queue attempt number",
        ExpectedConstraint::Check,
        "INSERT INTO job_attempts (
            job_identity, attempt_number, claim_token, worker_id, started_at, finished_at,
            outcome, error
         ) VALUES (?1, 0, 1, 'worker', 1, NULL, 'active', NULL)",
        params![&valid_job_identity],
    )?;
    expect_constraint_rejection(
        connection,
        "queue attempt foreign key",
        ExpectedConstraint::ForeignKey,
        "INSERT INTO job_attempts (
            job_identity, attempt_number, claim_token, worker_id, started_at, finished_at,
            outcome, error
         ) VALUES (?1, 1, 1, 'worker', 1, NULL, 'active', NULL)",
        params![&missing_job_identity],
    )?;
    connection
        .execute(
            "INSERT INTO job_attempts (
                job_identity, attempt_number, claim_token, worker_id, started_at, finished_at,
                outcome, error
             ) VALUES (?1, 1, 1, 'worker', 1, NULL, 'active', NULL)",
            params![&valid_job_identity],
        )
        .map_err(DailyCrawlStateStoreError::database)?;
    expect_constraint_rejection(
        connection,
        "duplicate queue attempt number",
        ExpectedConstraint::Unique,
        "INSERT INTO job_attempts (
            job_identity, attempt_number, claim_token, worker_id, started_at, finished_at,
            outcome, error
         ) VALUES (?1, 1, 2, 'worker', 1, NULL, 'active', NULL)",
        params![&valid_job_identity],
    )
}

fn validate_game_snapshot_constraint_behavior(
    connection: &mut Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(DailyCrawlStateStoreError::database)?;
    let validation = validate_game_snapshot_constraints_in_transaction(&transaction);
    let rollback = transaction.rollback();

    match (validation, rollback) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (_, Err(error)) => Err(DailyCrawlStateStoreError::database(error)),
    }
}

fn validate_game_snapshot_constraints_in_transaction(
    connection: &Connection,
) -> Result<(), DailyCrawlStateStoreError> {
    let mut reserved_probe_product_ids = BTreeSet::new();
    let valid_product_id =
        next_absent_probe_game_source_id(connection, &mut reserved_probe_product_ids)?;
    let missing_product_id =
        next_absent_probe_game_source_id(connection, &mut reserved_probe_product_ids)?;

    expect_constraint_rejection(
        connection,
        "zero game source identity",
        ExpectedConstraint::Check,
        "INSERT INTO games (source_product_id, source_slug, title, description)
         VALUES (0, 'zero', 'Zero', 'Synthetic')",
        [],
    )?;
    expect_constraint_rejection(
        connection,
        "blank game title",
        ExpectedConstraint::Check,
        "INSERT INTO games (source_product_id, source_slug, title, description)
         VALUES (?1, 'blank-title', '   ', 'Synthetic')",
        params![valid_product_id],
    )?;
    expect_constraint_rejection(
        connection,
        "partial cover descriptor",
        ExpectedConstraint::Check,
        "INSERT INTO games (
            source_product_id, source_slug, title, description, cover_bucket_path
         ) VALUES (?1, 'partial-cover', 'Partial Cover', 'Synthetic', 'bucket')",
        params![valid_product_id],
    )?;
    connection
        .execute(
            "INSERT INTO games (source_product_id, source_slug, title, description)
             VALUES (?1, 'valid-game', 'Valid Game', 'Synthetic')",
            params![valid_product_id],
        )
        .map_err(DailyCrawlStateStoreError::database)?;
    expect_constraint_rejection(
        connection,
        "platform source identity",
        ExpectedConstraint::Check,
        "INSERT INTO game_platform_scores (
            game_source_product_id, source_platform_id, source_slug, metascore, userscore
         ) VALUES (?1, 0, 'pc', 80, 8.0)",
        params![valid_product_id],
    )?;
    expect_constraint_rejection(
        connection,
        "Metascore above upper bound",
        ExpectedConstraint::Check,
        "INSERT INTO game_platform_scores (
            game_source_product_id, source_platform_id, source_slug, metascore, userscore
         ) VALUES (?1, 1, 'pc', 101, 8.0)",
        params![valid_product_id],
    )?;
    expect_constraint_rejection(
        connection,
        "Userscore above upper bound",
        ExpectedConstraint::Check,
        "INSERT INTO game_platform_scores (
            game_source_product_id, source_platform_id, source_slug, metascore, userscore
         ) VALUES (?1, 1, 'pc', 80, 10.1)",
        params![valid_product_id],
    )?;
    expect_constraint_rejection(
        connection,
        "platform game foreign key",
        ExpectedConstraint::ForeignKey,
        "INSERT INTO game_platform_scores (
            game_source_product_id, source_platform_id, source_slug, metascore, userscore
         ) VALUES (?1, 1, 'pc', 80, 8.0)",
        params![missing_product_id],
    )?;
    connection
        .execute(
            "INSERT INTO game_platform_scores (
                game_source_product_id, source_platform_id, source_slug, metascore, userscore
             ) VALUES (?1, 1, 'pc', 80, 8.0)",
            params![valid_product_id],
        )
        .map_err(DailyCrawlStateStoreError::database)?;
    expect_constraint_rejection(
        connection,
        "duplicate platform identity",
        ExpectedConstraint::PrimaryKey,
        "INSERT INTO game_platform_scores (
            game_source_product_id, source_platform_id, source_slug, metascore, userscore
         ) VALUES (?1, 1, 'pc-renamed', 80, 8.0)",
        params![valid_product_id],
    )?;
    expect_constraint_rejection(
        connection,
        "developer game foreign key",
        ExpectedConstraint::ForeignKey,
        "INSERT INTO game_developers (game_source_product_id, developer_name)
         VALUES (?1, 'Missing Parent')",
        params![missing_product_id],
    )
}

fn next_absent_probe_game_source_id(
    connection: &Connection,
    reserved_probe_product_ids: &mut BTreeSet<i64>,
) -> Result<i64, DailyCrawlStateStoreError> {
    let mut suffix = 0_i64;
    loop {
        let candidate = 9_000_000_000_000_000_000_i64
            .checked_sub(suffix)
            .ok_or_else(|| {
                DailyCrawlStateStoreError::malformed(
                    "no absent game source identity is available for validation",
                )
            })?;
        suffix = suffix.checked_add(1).ok_or_else(|| {
            DailyCrawlStateStoreError::malformed(
                "no absent game source identity is available for validation",
            )
        })?;
        let exists = connection
            .query_row(
                "SELECT 1 FROM games WHERE source_product_id = ?1",
                params![candidate],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(DailyCrawlStateStoreError::database)?
            .is_some();
        if !exists && reserved_probe_product_ids.insert(candidate) {
            return Ok(candidate);
        }
    }
}

fn next_absent_probe_job_identity(
    connection: &Connection,
    reserved_probe_job_identities: &mut BTreeSet<String>,
) -> Result<String, DailyCrawlStateStoreError> {
    let mut suffix = 0_u64;
    loop {
        let candidate = format!("__gamepulse_job_queue_validation_probe_{suffix}");
        suffix = suffix.checked_add(1).ok_or_else(|| {
            DailyCrawlStateStoreError::malformed(
                "no absent job identity is available for validation",
            )
        })?;
        let exists = connection
            .query_row(
                "SELECT 1 FROM jobs WHERE job_identity = ?1",
                params![&candidate],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(DailyCrawlStateStoreError::database)?
            .is_some();
        if !exists && reserved_probe_job_identities.insert(candidate.clone()) {
            return Ok(candidate);
        }
    }
}

fn next_absent_probe_day_key(
    connection: &Connection,
    reserved_probe_day_keys: &mut BTreeSet<String>,
) -> Result<String, DailyCrawlStateStoreError> {
    let mut suffix = 0_u64;
    loop {
        let candidate = format!("__gamepulse_daily_crawl_validation_probe_{suffix}");
        suffix = suffix.checked_add(1).ok_or_else(|| {
            DailyCrawlStateStoreError::malformed("no absent day key is available for validation")
        })?;
        if !probe_day_key_exists(connection, &candidate)?
            && reserved_probe_day_keys.insert(candidate.clone())
        {
            return Ok(candidate);
        }
    }
}

fn ensure_probe_day_key_is_absent(
    connection: &Connection,
    day_key: &str,
) -> Result<(), DailyCrawlStateStoreError> {
    if probe_day_key_exists(connection, day_key)? {
        return Err(DailyCrawlStateStoreError::malformed(
            "validation probe day key is already present",
        ));
    }
    Ok(())
}

fn probe_day_key_exists(
    connection: &Connection,
    day_key: &str,
) -> Result<bool, DailyCrawlStateStoreError> {
    let exists = connection
        .query_row(
            "SELECT 1 FROM crawl_days WHERE day_key = ?1",
            params![day_key],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(DailyCrawlStateStoreError::database)?;
    Ok(exists.is_some())
}

fn expect_constraint_rejection<P>(
    connection: &Connection,
    description: &str,
    expected_constraint: ExpectedConstraint,
    statement: &str,
    parameters: P,
) -> Result<(), DailyCrawlStateStoreError>
where
    P: Params,
{
    match connection.execute(statement, parameters) {
        Err(Error::SqliteFailure(error, _))
            if error.extended_code == expected_constraint.extended_code() =>
        {
            Ok(())
        }
        Ok(_) => Err(DailyCrawlStateStoreError::malformed(format!(
            "managed schema accepted {description}"
        ))),
        Err(Error::SqliteFailure(error, _)) => Err(DailyCrawlStateStoreError::malformed(format!(
            "managed schema returned {error} instead of a {} constraint for {description}",
            expected_constraint.name()
        ))),
        Err(error) => Err(DailyCrawlStateStoreError::database(error)),
    }
}

fn verify_commit_against_current_state(
    current_state: Option<&DailyCrawlState>,
    commit: &DailyCrawlCommit,
) -> Result<(), DailyCrawlStateStoreError> {
    let expected_previous_state = commit.expected_previous_state();
    if current_state != expected_previous_state && current_state != Some(commit.state()) {
        return Err(DailyCrawlStateStoreError::invalid_commit(
            "stale or conflicting expected previous state",
        ));
    }

    let current_ids = current_state
        .map(DailyCrawlState::selected_or_processed)
        .cloned()
        .unwrap_or_default();
    let selected_ids = commit
        .selected()
        .iter()
        .map(DiscoveryCandidate::source_product_id)
        .collect::<BTreeSet<_>>();
    for source_product_id in commit.state().selected_or_processed() {
        if !current_ids.contains(source_product_id) && !selected_ids.contains(source_product_id) {
            return Err(DailyCrawlStateStoreError::invalid_commit(
                "newly selected or processed identity has no selected candidate",
            ));
        }
    }
    Ok(())
}

fn load_daily_crawl_state(
    connection: &Connection,
    day: &CrawlDayKey,
) -> Result<Option<DailyCrawlState>, DailyCrawlStateStoreError> {
    let stored_state = connection
        .query_row(
            "SELECT new_releases_completed, browse_progress, browse_cursor
             FROM crawl_days
             WHERE day_key = ?1",
            params![day.as_str()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()
        .map_err(DailyCrawlStateStoreError::database)?;

    let Some((new_releases_completed, browse_progress, browse_cursor)) = stored_state else {
        return Ok(None);
    };

    Ok(Some(DailyCrawlState::restored(
        day.clone(),
        read_selected_or_processed(connection, day)?,
        parse_completion(new_releases_completed)?,
        parse_browse_progress(&browse_progress, browse_cursor.as_deref())?,
    )))
}

fn write_state(
    transaction: &Transaction<'_>,
    state: &DailyCrawlState,
) -> Result<(), DailyCrawlStateStoreError> {
    let (browse_progress, browse_cursor) = match state.browse_progress() {
        BrowseProgress::Initial => ("initial", None),
        BrowseProgress::Continue(cursor) => ("continue", Some(cursor.value().to_string())),
        BrowseProgress::Exhausted => ("exhausted", None),
    };
    transaction
        .execute(
            "INSERT INTO crawl_days (
                day_key,
                new_releases_completed,
                browse_progress,
                browse_cursor
            ) VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(day_key) DO UPDATE SET
                new_releases_completed = excluded.new_releases_completed,
                browse_progress = excluded.browse_progress,
                browse_cursor = excluded.browse_cursor",
            params![
                state.day().as_str(),
                i64::from(state.new_releases_completed()),
                browse_progress,
                browse_cursor,
            ],
        )
        .map_err(DailyCrawlStateStoreError::database)?;
    Ok(())
}

fn write_selected_or_processed(
    transaction: &Transaction<'_>,
    state: &DailyCrawlState,
) -> Result<(), DailyCrawlStateStoreError> {
    for source_product_id in state.selected_or_processed() {
        transaction
            .execute(
                "INSERT INTO crawl_day_selected_or_processed (day_key, source_product_id)
                 VALUES (?1, ?2)
                 ON CONFLICT(day_key, source_product_id) DO NOTHING",
                params![state.day().as_str(), source_product_id.value().to_string()],
            )
            .map_err(DailyCrawlStateStoreError::database)?;
    }
    Ok(())
}

fn write_selected_candidates(
    transaction: &Transaction<'_>,
    day: &str,
    selected: &[DiscoveryCandidate],
) -> Result<(), DailyCrawlStateStoreError> {
    for candidate in selected {
        transaction
            .execute(
                "INSERT INTO crawl_day_selected_candidates (
                    day_key,
                    source_product_id,
                    source_slug
                ) VALUES (?1, ?2, ?3)
                ON CONFLICT(day_key, source_product_id) DO UPDATE SET
                    source_slug = excluded.source_slug",
                params![
                    day,
                    candidate.source_product_id().value().to_string(),
                    candidate.source_slug(),
                ],
            )
            .map_err(DailyCrawlStateStoreError::database)?;
    }
    Ok(())
}

fn read_selected_or_processed(
    connection: &Connection,
    day: &CrawlDayKey,
) -> Result<BTreeSet<SourceProductId>, DailyCrawlStateStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT source_product_id
             FROM crawl_day_selected_or_processed
             WHERE day_key = ?1",
        )
        .map_err(DailyCrawlStateStoreError::database)?;
    let rows = statement
        .query_map(params![day.as_str()], |row| row.get::<_, String>(0))
        .map_err(DailyCrawlStateStoreError::database)?;
    let mut selected_or_processed = BTreeSet::new();
    for row in rows {
        let source_product_id = row.map_err(DailyCrawlStateStoreError::database)?;
        let source_product_id = parse_canonical_u64(&source_product_id, "source product identity")?;
        let source_product_id = SourceProductId::new(source_product_id)
            .map_err(|error| DailyCrawlStateStoreError::malformed(error.to_string()))?;
        if !selected_or_processed.insert(source_product_id) {
            return Err(DailyCrawlStateStoreError::malformed(
                "duplicate source product identity",
            ));
        }
    }
    Ok(selected_or_processed)
}

fn parse_completion(value: i64) -> Result<bool, DailyCrawlStateStoreError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(DailyCrawlStateStoreError::malformed(format!(
            "new releases completion flag {other}"
        ))),
    }
}

fn parse_browse_progress(
    browse_progress: &str,
    browse_cursor: Option<&str>,
) -> Result<BrowseProgress, DailyCrawlStateStoreError> {
    match (browse_progress, browse_cursor) {
        ("initial", None) => Ok(BrowseProgress::Initial),
        ("continue", Some(cursor)) => Ok(BrowseProgress::Continue(BrowseCursor::new(
            parse_canonical_u64(cursor, "browse cursor")?,
        ))),
        ("exhausted", None) => Ok(BrowseProgress::Exhausted),
        ("initial" | "exhausted", Some(_)) => Err(DailyCrawlStateStoreError::malformed(
            "browse cursor is present for a non-continuation state",
        )),
        ("continue", None) => Err(DailyCrawlStateStoreError::malformed(
            "continuation state has no browse cursor",
        )),
        (other, _) => Err(DailyCrawlStateStoreError::malformed(format!(
            "unknown browse progress {other:?}"
        ))),
    }
}

fn parse_canonical_u64(value: &str, field: &str) -> Result<u64, DailyCrawlStateStoreError> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| DailyCrawlStateStoreError::malformed(format!("invalid {field}")))?;
    if parsed.to_string() != value {
        return Err(DailyCrawlStateStoreError::malformed(format!(
            "non-canonical {field}"
        )));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process;
    use std::sync::atomic::{AtomicU64, Ordering};

    use gamepulse_application::{JobTimestamp, SourceIngestionJobSchedule};

    use super::*;

    static NEXT_TEMPORARY_DATABASE: AtomicU64 = AtomicU64::new(0);

    struct TemporaryDatabase {
        path: PathBuf,
    }

    impl TemporaryDatabase {
        fn new(name: &str) -> Self {
            let sequence = NEXT_TEMPORARY_DATABASE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "gamepulse-storage-sqlite-{name}-{}-{sequence}.sqlite3",
                process::id()
            ));
            let _ = fs::remove_file(&path);
            Self { path }
        }

        fn open(&self) -> SqliteDailyCrawlStateStore {
            SqliteDailyCrawlStateStore::open(&self.path).expect("test database must open")
        }
    }

    impl Drop for TemporaryDatabase {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn day(value: &str) -> CrawlDayKey {
        CrawlDayKey::new(value).expect("test day must be valid")
    }

    fn product(value: u64) -> SourceProductId {
        SourceProductId::new(value).expect("test source product ID must be valid")
    }

    fn candidate(value: u64, slug: &str) -> DiscoveryCandidate {
        DiscoveryCandidate::new(value, slug).expect("test candidate must be valid")
    }

    fn state(
        day: CrawlDayKey,
        source_product_ids: impl IntoIterator<Item = u64>,
        new_releases_completed: bool,
        browse_progress: BrowseProgress,
    ) -> DailyCrawlState {
        DailyCrawlState::restored(
            day,
            source_product_ids.into_iter().map(product),
            new_releases_completed,
            browse_progress,
        )
    }

    fn commit(
        expected_previous_state: Option<DailyCrawlState>,
        state: DailyCrawlState,
        selected: Vec<DiscoveryCandidate>,
    ) -> DailyCrawlCommit {
        DailyCrawlCommit::new(expected_previous_state, state, selected)
            .expect("test commit must be valid")
    }

    fn commit_with_source_ingestion_jobs(
        expected_previous_state: Option<DailyCrawlState>,
        state: DailyCrawlState,
        selected: Vec<DiscoveryCandidate>,
        created_at: i64,
    ) -> DailyCrawlCommit {
        commit(expected_previous_state, state, selected)
            .with_source_ingestion_jobs(
                SourceIngestionJobSchedule::new(2).expect("schedule must be valid"),
                JobTimestamp::new(created_at).expect("timestamp must be valid"),
            )
            .expect("source ingestion jobs must be valid")
    }

    fn create_version_one_schema(database: &TemporaryDatabase, schema: &str) {
        let connection = Connection::open(&database.path).expect("raw test database must open");
        connection
            .execute_batch(schema)
            .expect("version-one test schema must create");
        connection
            .pragma_update(None, "user_version", DAILY_CRAWL_SCHEMA_VERSION)
            .expect("test schema version must set");
    }

    fn create_version_two_schema(database: &TemporaryDatabase, queue_schema: &str) {
        let connection = Connection::open(&database.path).expect("raw test database must open");
        connection
            .execute_batch(DAILY_CRAWL_MIGRATION_0001)
            .expect("daily-crawl test schema must create");
        connection
            .execute_batch(queue_schema)
            .expect("queue test schema must create");
        connection
            .pragma_update(None, "user_version", JOB_QUEUE_SCHEMA_VERSION)
            .expect("test schema version must set");
    }

    fn create_version_four_schema(database: &TemporaryDatabase) {
        let connection = Connection::open(&database.path).expect("raw test database must open");
        connection
            .execute_batch(DAILY_CRAWL_MIGRATION_0001)
            .expect("daily-crawl test schema must create");
        connection
            .execute_batch(JOB_QUEUE_MIGRATION_0002)
            .expect("queue test schema must create");
        connection
            .execute_batch(GAME_SNAPSHOT_MIGRATION_0003)
            .expect("snapshot test schema must create");
        connection
            .execute_batch(REVIEW_SUMMARY_MIGRATION_0004)
            .expect("review-summary test schema must create");
        connection
            .pragma_update(None, "user_version", REVIEW_SUMMARY_SCHEMA_VERSION)
            .expect("test schema version must set");
    }

    fn create_version_five_schema_without_public_cover_check(database: &TemporaryDatabase) {
        let connection = Connection::open(&database.path).expect("raw test database must open");
        connection
            .execute_batch(DAILY_CRAWL_MIGRATION_0001)
            .expect("daily-crawl test schema must create");
        connection
            .execute_batch(JOB_QUEUE_MIGRATION_0002)
            .expect("queue test schema must create");
        connection
            .execute_batch(GAME_SNAPSHOT_MIGRATION_0003)
            .expect("snapshot test schema must create");
        connection
            .execute_batch(REVIEW_SUMMARY_MIGRATION_0004)
            .expect("review-summary test schema must create");
        connection
            .execute_batch("ALTER TABLE games ADD COLUMN public_cover_url TEXT;")
            .expect("malformed public-cover schema must create");
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .expect("test schema version must set");
    }

    fn create_version_three_schema(database: &TemporaryDatabase) {
        let connection = Connection::open(&database.path).expect("raw test database must open");
        connection
            .execute_batch(DAILY_CRAWL_MIGRATION_0001)
            .expect("daily-crawl test schema must create");
        connection
            .execute_batch(JOB_QUEUE_MIGRATION_0002)
            .expect("queue test schema must create");
        connection
            .execute_batch(GAME_SNAPSHOT_MIGRATION_0003)
            .expect("snapshot test schema must create");
        connection
            .pragma_update(None, "user_version", GAME_SNAPSHOT_SCHEMA_VERSION)
            .expect("test schema version must set");
    }

    fn insert_old_fixed_probe_collision_rows(connection: &Connection) {
        for day_key in [
            "completion-probe",
            "progress-probe",
            "continue-probe",
            "initial-cursor-probe",
            "exhausted-cursor-probe",
            "relation-probe",
            "relation-probe-two",
            "missing-day",
        ] {
            connection
                .execute(
                    "INSERT INTO crawl_days (day_key, new_releases_completed, browse_progress, browse_cursor)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![day_key, 0_i64, "initial", None::<&str>],
                )
                .expect("old probe collision day must insert");
        }
        for (day_key, source_product_id) in [
            ("relation-probe", "1"),
            ("relation-probe-two", "2"),
            ("missing-day", "2"),
        ] {
            connection
                .execute(
                    "INSERT INTO crawl_day_selected_or_processed (day_key, source_product_id)
                     VALUES (?1, ?2)",
                    params![day_key, source_product_id],
                )
                .expect("old probe collision identity must insert");
        }
        connection
            .execute(
                "INSERT INTO crawl_day_selected_candidates (day_key, source_product_id, source_slug)
                 VALUES (?1, ?2, ?3)",
                params!["relation-probe", "1", "cross-pair"],
            )
            .expect("old probe collision candidate must insert");
    }

    fn assert_old_fixed_probe_collision_rows(connection: &Connection) {
        for day_key in [
            "completion-probe",
            "progress-probe",
            "continue-probe",
            "initial-cursor-probe",
            "exhausted-cursor-probe",
            "relation-probe",
            "relation-probe-two",
            "missing-day",
        ] {
            let row = connection
                .query_row(
                    "SELECT new_releases_completed, browse_progress, browse_cursor
                     FROM crawl_days WHERE day_key = ?1",
                    params![day_key],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .expect("old probe collision day must remain");
            assert_eq!(row, (0, "initial".to_owned(), None));
        }
        let days = connection
            .query_row("SELECT COUNT(*) FROM crawl_days", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("day count must load");
        let identities = connection
            .query_row(
                "SELECT COUNT(*) FROM crawl_day_selected_or_processed",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("identity count must load");
        let candidates = connection
            .query_row(
                "SELECT COUNT(*) FROM crawl_day_selected_candidates",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("candidate count must load");
        assert_eq!((days, identities, candidates), (8, 3, 1));
    }

    #[test]
    fn fresh_in_memory_database_migrates_and_returns_no_unknown_day_state() {
        let mut store = SqliteDailyCrawlStateStore::open_in_memory().expect("store must open");
        let schema_version = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("schema version must load");

        assert_eq!(schema_version, SCHEMA_VERSION);
        assert_eq!(store.load(&day("2026-08-14")), Ok(None));
        let managed_rows = store
            .connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM crawl_days) +
                    (SELECT COUNT(*) FROM crawl_day_selected_or_processed) +
                    (SELECT COUNT(*) FROM crawl_day_selected_candidates)",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("managed row count must load");
        assert_eq!(managed_rows, 0);
    }

    #[test]
    fn version_one_daily_crawl_database_upgrades_to_the_queue_schema_without_rewriting_state() {
        let database = TemporaryDatabase::new("upgrade-v1-to-v2");
        create_version_one_schema(&database, DAILY_CRAWL_MIGRATION_0001);
        let crawl_day = day("2026-08-14");
        let connection = Connection::open(&database.path).expect("raw test database must open");
        connection
            .execute(
                "INSERT INTO crawl_days (day_key, new_releases_completed, browse_progress, browse_cursor)
                 VALUES (?1, 1, 'continue', '24')",
                params![crawl_day.as_str()],
            )
            .expect("test crawl state must insert");
        connection
            .execute(
                "INSERT INTO crawl_day_selected_or_processed (day_key, source_product_id)
                 VALUES (?1, '101')",
                params![crawl_day.as_str()],
            )
            .expect("test crawl identity must insert");
        drop(connection);

        let mut store = database.open();
        assert_eq!(
            store.load(&crawl_day),
            Ok(Some(state(
                crawl_day.clone(),
                [101],
                true,
                BrowseProgress::Continue(BrowseCursor::new(24)),
            )))
        );
        let version = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("schema version must load");
        assert_eq!(version, SCHEMA_VERSION);
        let jobs = store
            .connection
            .query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get::<_, i64>(0))
            .expect("queue table must be available");
        assert_eq!(jobs, 0);
    }

    #[test]
    fn version_two_schema_without_unique_attempt_number_rejects_reopen() {
        let database = TemporaryDatabase::new("missing-queue-attempt-unique");
        let schema =
            JOB_QUEUE_MIGRATION_0002.replace("    UNIQUE (job_identity, attempt_number),\n", "");
        create_version_two_schema(&database, &schema);

        assert!(SqliteDailyCrawlStateStore::open(&database.path).is_err());
    }

    #[test]
    fn version_four_database_adds_public_cover_and_review_polarity_columns_on_reopen() {
        let database = TemporaryDatabase::new("upgrade-v4-to-v6-cover-and-polarity");
        create_version_four_schema(&database);

        drop(SqliteDailyCrawlStateStore::open(&database.path).expect("database must migrate"));

        let connection = Connection::open(&database.path).expect("migrated database must reopen");
        let version = connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("schema version must load");
        let public_cover_column = connection
            .prepare("PRAGMA table_info(games)")
            .expect("table info must prepare")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("table info must load")
            .collect::<Result<Vec<_>, _>>()
            .expect("table info values must decode");
        let review_excerpt_columns = connection
            .prepare("PRAGMA table_info(review_input_excerpts)")
            .expect("review excerpt table info must prepare")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("review excerpt table info must load")
            .collect::<Result<Vec<_>, _>>()
            .expect("review excerpt columns must decode");

        assert_eq!(version, SCHEMA_VERSION);
        assert!(public_cover_column.contains(&"public_cover_url".to_owned()));
        assert!(review_excerpt_columns.contains(&"polarity".to_owned()));
    }

    #[test]
    fn public_cover_url_check_rejects_blank_and_whitespace_values() {
        let database = TemporaryDatabase::new("public-cover-url-check");
        drop(SqliteDailyCrawlStateStore::open(&database.path).expect("database must open"));
        let connection = Connection::open(&database.path).expect("database must reopen");

        for (source_product_id, value) in [(101_i64, ""), (102_i64, "   ")] {
            expect_constraint_rejection(
                &connection,
                "blank public cover URL",
                ExpectedConstraint::Check,
                "INSERT INTO games (
                    source_product_id, source_slug, title, description, public_cover_url
                 ) VALUES (?1, 'blank-public-cover', 'Blank Public Cover', 'Synthetic', ?2)",
                params![source_product_id, value],
            )
            .expect("public-cover check must reject blank values");
        }
    }

    #[test]
    fn version_five_schema_without_public_cover_check_rejects_reopen() {
        let database = TemporaryDatabase::new("missing-public-cover-check");
        create_version_five_schema_without_public_cover_check(&database);

        assert!(SqliteDailyCrawlStateStore::open(&database.path).is_err());
    }

    #[test]
    fn version_three_database_applies_the_review_and_public_cover_migrations_on_reopen() {
        let database = TemporaryDatabase::new("upgrade-v3-to-v5-public-cover");
        create_version_three_schema(&database);

        drop(SqliteDailyCrawlStateStore::open(&database.path).expect("database must migrate"));

        let connection = Connection::open(&database.path).expect("migrated database must reopen");
        let version = connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("schema version must load");
        let public_cover_column = connection
            .prepare("PRAGMA table_info(games)")
            .expect("table info must prepare")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("table info must load")
            .collect::<Result<Vec<_>, _>>()
            .expect("table info values must decode");

        assert_eq!(version, SCHEMA_VERSION);
        assert!(public_cover_column.contains(&"public_cover_url".to_owned()));
    }

    #[test]
    fn version_one_reopen_uses_absent_probe_keys_without_changing_old_fixed_key_rows() {
        let database = TemporaryDatabase::new("probe-key-collision");
        {
            let store = database.open();
            drop(store);
            let connection =
                Connection::open(&database.path).expect("raw test database must reopen");
            insert_old_fixed_probe_collision_rows(&connection);
            assert_old_fixed_probe_collision_rows(&connection);
        }

        {
            let reopened = database.open();
            drop(reopened);
        }

        let connection = Connection::open(&database.path).expect("raw test database must reopen");
        assert_old_fixed_probe_collision_rows(&connection);
    }

    #[test]
    fn missing_completion_check_is_not_masked_by_an_old_probe_primary_key_conflict() {
        let database = TemporaryDatabase::new("missing-completion-check-probe-key-collision");
        let schema =
            DAILY_CRAWL_MIGRATION_0001.replace(" CHECK (new_releases_completed IN (0, 1))", "");
        create_version_one_schema(&database, &schema);
        let connection = Connection::open(&database.path).expect("raw test database must open");
        connection
            .execute(
                "INSERT INTO crawl_days (day_key, new_releases_completed, browse_progress, browse_cursor)
                 VALUES (?1, ?2, ?3, ?4)",
                params!["completion-probe", 0_i64, "initial", None::<&str>],
            )
            .expect("old completion probe key must insert");
        drop(connection);

        assert!(SqliteDailyCrawlStateStore::open(&database.path).is_err());

        let connection = Connection::open(&database.path).expect("raw test database must reopen");
        let rows = connection
            .query_row("SELECT COUNT(*) FROM crawl_days", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("row count must load");
        assert_eq!(rows, 1);
    }

    #[test]
    fn exact_constraint_matching_rejects_other_constraint_classes() {
        let store = SqliteDailyCrawlStateStore::open_in_memory().expect("store must open");
        let connection = &store.connection;
        connection
            .execute(
                "INSERT INTO crawl_days (day_key, new_releases_completed, browse_progress, browse_cursor)
                 VALUES (?1, ?2, ?3, ?4)",
                params!["exact-existing", 0_i64, "initial", None::<&str>],
            )
            .expect("existing day must insert");
        connection
            .execute(
                "INSERT INTO crawl_day_selected_or_processed (day_key, source_product_id)
                 VALUES (?1, ?2)",
                params!["exact-existing", "1"],
            )
            .expect("existing identity must insert");
        connection
            .execute_batch(
                "CREATE TEMP TABLE exact_constraint_unique (value TEXT UNIQUE NOT NULL);",
            )
            .expect("unique test table must create");
        connection
            .execute(
                "INSERT INTO exact_constraint_unique (value) VALUES (?1)",
                params!["existing"],
            )
            .expect("unique test row must insert");

        assert!(expect_constraint_rejection(
            connection,
            "primary key as check",
            ExpectedConstraint::Check,
            "INSERT INTO crawl_days (day_key, new_releases_completed, browse_progress, browse_cursor)
             VALUES (?1, ?2, ?3, ?4)",
            params!["exact-existing", 0_i64, "initial", None::<&str>],
        )
        .is_err());
        assert!(
            expect_constraint_rejection(
                connection,
                "unique as check",
                ExpectedConstraint::Check,
                "INSERT INTO exact_constraint_unique (value) VALUES (?1)",
                params!["existing"],
            )
            .is_err()
        );
        assert!(expect_constraint_rejection(
            connection,
            "not null as check",
            ExpectedConstraint::Check,
            "INSERT INTO crawl_days (day_key, new_releases_completed, browse_progress, browse_cursor)
             VALUES (?1, ?2, ?3, ?4)",
            params!["check-not-null", 0_i64, None::<&str>, None::<&str>],
        )
        .is_err());
        assert!(
            expect_constraint_rejection(
                connection,
                "foreign key as check",
                ExpectedConstraint::Check,
                "INSERT INTO crawl_day_selected_or_processed (day_key, source_product_id)
             VALUES (?1, ?2)",
                params!["missing-check-parent", "1"],
            )
            .is_err()
        );

        assert!(expect_constraint_rejection(
            connection,
            "check as foreign key",
            ExpectedConstraint::ForeignKey,
            "INSERT INTO crawl_days (day_key, new_releases_completed, browse_progress, browse_cursor)
             VALUES (?1, ?2, ?3, ?4)",
            params!["   ", 0_i64, "initial", None::<&str>],
        )
        .is_err());
        assert!(expect_constraint_rejection(
            connection,
            "primary key as foreign key",
            ExpectedConstraint::ForeignKey,
            "INSERT INTO crawl_days (day_key, new_releases_completed, browse_progress, browse_cursor)
             VALUES (?1, ?2, ?3, ?4)",
            params!["exact-existing", 0_i64, "initial", None::<&str>],
        )
        .is_err());
        assert!(
            expect_constraint_rejection(
                connection,
                "unique as foreign key",
                ExpectedConstraint::ForeignKey,
                "INSERT INTO exact_constraint_unique (value) VALUES (?1)",
                params!["existing"],
            )
            .is_err()
        );
        assert!(expect_constraint_rejection(
            connection,
            "not null as foreign key",
            ExpectedConstraint::ForeignKey,
            "INSERT INTO crawl_days (day_key, new_releases_completed, browse_progress, browse_cursor)
             VALUES (?1, ?2, ?3, ?4)",
            params!["foreign-key-not-null", 0_i64, None::<&str>, None::<&str>],
        )
        .is_err());
    }

    #[test]
    fn restores_initial_continue_and_exhausted_states_without_cross_day_changes() {
        let mut store = SqliteDailyCrawlStateStore::open_in_memory().expect("store must open");
        let initial_day = day("2026-08-14");
        let continue_day = day("2026-08-15");
        let exhausted_day = day("2026-08-16");
        let initial = state(initial_day.clone(), [1], true, BrowseProgress::Initial);
        let continued = state(
            continue_day.clone(),
            [2],
            true,
            BrowseProgress::Continue(BrowseCursor::new(24)),
        );
        let exhausted = state(exhausted_day.clone(), [3], true, BrowseProgress::Exhausted);

        store
            .commit(commit(None, initial.clone(), vec![candidate(1, "initial")]))
            .expect("initial state must commit");
        store
            .commit(commit(
                None,
                continued.clone(),
                vec![candidate(2, "continued")],
            ))
            .expect("continued state must commit");
        store
            .commit(commit(
                None,
                exhausted.clone(),
                vec![candidate(3, "exhausted")],
            ))
            .expect("exhausted state must commit");

        assert_eq!(store.load(&initial_day), Ok(Some(initial)));
        assert_eq!(store.load(&continue_day), Ok(Some(continued)));
        assert_eq!(store.load(&exhausted_day), Ok(Some(exhausted)));
        assert_eq!(
            store.selected_candidates_for_day(&continue_day),
            vec![(2, "continued".to_owned())]
        );
    }

    #[test]
    fn maximum_u64_identity_and_cursor_round_trip_without_signed_conversion() {
        let mut store = SqliteDailyCrawlStateStore::open_in_memory().expect("store must open");
        let crawl_day = day("2026-08-14");
        let maximum = u64::MAX;
        let expected = state(
            crawl_day.clone(),
            [maximum],
            true,
            BrowseProgress::Continue(BrowseCursor::new(maximum)),
        );

        store
            .commit(commit(
                None,
                expected.clone(),
                vec![candidate(maximum, "maximum")],
            ))
            .expect("maximum identity must commit");

        assert_eq!(store.load(&crawl_day), Ok(Some(expected)));
        assert_eq!(
            store.selected_candidates_for_day(&crawl_day),
            vec![(maximum, "maximum".to_owned())]
        );
    }

    #[test]
    fn same_identity_changed_slug_replaces_slug_and_identical_replay_is_idempotent() {
        let mut store = SqliteDailyCrawlStateStore::open_in_memory().expect("store must open");
        let crawl_day = day("2026-08-14");
        let expected = state(crawl_day.clone(), [41], true, BrowseProgress::Initial);

        store
            .commit(commit(
                None,
                expected.clone(),
                vec![candidate(41, "first-slug")],
            ))
            .expect("first candidate must commit");
        store
            .commit(commit(
                Some(expected.clone()),
                expected.clone(),
                vec![candidate(41, "renamed-slug")],
            ))
            .expect("changed slug replay must commit");
        store
            .commit(commit(
                Some(expected.clone()),
                expected.clone(),
                vec![candidate(41, "renamed-slug")],
            ))
            .expect("identical replay must commit");

        assert_eq!(store.load(&crawl_day), Ok(Some(expected)));
        assert_eq!(
            store.selected_candidates_for_day(&crawl_day),
            vec![(41, "renamed-slug".to_owned())]
        );
    }

    #[test]
    fn newly_added_identity_without_candidate_fails_without_publishing_any_part() {
        let mut store = SqliteDailyCrawlStateStore::open_in_memory().expect("store must open");
        let crawl_day = day("2026-08-14");
        let committed = state(crawl_day.clone(), [1], true, BrowseProgress::Initial);
        let invalid_next = state(
            crawl_day.clone(),
            [1, 42],
            true,
            BrowseProgress::Continue(BrowseCursor::new(24)),
        );
        store
            .commit(commit(None, committed.clone(), vec![candidate(1, "first")]))
            .expect("initial state must commit");

        assert!(
            store
                .commit(commit(Some(committed.clone()), invalid_next, vec![]))
                .is_err()
        );
        assert_eq!(store.load(&crawl_day), Ok(Some(committed)));
        assert_eq!(
            store.selected_candidates_for_day(&crawl_day),
            vec![(1, "first".to_owned())]
        );
    }

    #[test]
    fn candidate_insert_failure_rolls_back_the_entire_commit() {
        let mut store = SqliteDailyCrawlStateStore::open_in_memory().expect("store must open");
        let crawl_day = day("2026-08-14");
        let committed = state(crawl_day.clone(), [1], true, BrowseProgress::Initial);
        let next = state(
            crawl_day.clone(),
            [1, 2],
            true,
            BrowseProgress::Continue(BrowseCursor::new(24)),
        );
        store
            .commit(commit(None, committed.clone(), vec![candidate(1, "first")]))
            .expect("initial state must commit");
        store.install_candidate_insert_failure_for_test();

        assert!(
            store
                .commit(commit(
                    Some(committed.clone()),
                    next,
                    vec![candidate(2, "second")],
                ))
                .is_err()
        );
        assert_eq!(store.load(&crawl_day), Ok(Some(committed)));
        assert_eq!(
            store.selected_candidates_for_day(&crawl_day),
            vec![(1, "first".to_owned())]
        );
    }

    #[test]
    fn source_ingestion_job_insert_failure_rolls_back_state_candidates_and_jobs() {
        let mut store = SqliteDailyCrawlStateStore::open_in_memory().expect("store must open");
        let crawl_day = day("2026-08-14");
        let next = state(crawl_day.clone(), [2], true, BrowseProgress::Initial);
        store.install_job_insert_failure_for_test();

        assert!(
            store
                .commit(commit_with_source_ingestion_jobs(
                    None,
                    next,
                    vec![candidate(2, "second")],
                    0,
                ))
                .is_err()
        );

        assert_eq!(store.load(&crawl_day), Ok(None));
        assert!(store.selected_candidates_for_day(&crawl_day).is_empty());
        assert!(store.job_identities().is_empty());
    }

    #[test]
    fn source_ingestion_jobs_deduplicate_replay_and_allow_later_day_reprocess() {
        let mut store = SqliteDailyCrawlStateStore::open_in_memory().expect("store must open");
        let first_day = day("2026-08-14");
        let first_state = state(first_day.clone(), [41], true, BrowseProgress::Initial);

        store
            .commit(commit_with_source_ingestion_jobs(
                None,
                first_state.clone(),
                vec![candidate(41, "first-slug")],
                0,
            ))
            .expect("first state and job must commit");
        store
            .commit(commit_with_source_ingestion_jobs(
                Some(first_state.clone()),
                first_state,
                vec![candidate(41, "first-slug")],
                1,
            ))
            .expect("replay must deduplicate the job");

        let second_day = day("2026-08-15");
        let second_state = state(second_day.clone(), [41], true, BrowseProgress::Initial);
        store
            .commit(commit_with_source_ingestion_jobs(
                None,
                second_state,
                vec![candidate(41, "later-day-slug")],
                2,
            ))
            .expect("later day must create a reprocess job");

        assert_eq!(
            store.job_identities(),
            [
                "source.game-ingestion:2026-08-14:41",
                "source.game-ingestion:2026-08-15:41",
            ]
        );
    }

    #[test]
    fn stale_same_day_slug_job_conflict_rolls_back_state_candidates_and_queue() {
        let mut store = SqliteDailyCrawlStateStore::open_in_memory().expect("store must open");
        let crawl_day = day("2026-08-14");
        let committed = state(crawl_day.clone(), [41], true, BrowseProgress::Initial);

        store
            .commit(commit_with_source_ingestion_jobs(
                None,
                committed.clone(),
                vec![candidate(41, "first-slug")],
                0,
            ))
            .expect("initial state and job must commit");

        assert!(
            store
                .commit(commit_with_source_ingestion_jobs(
                    Some(committed.clone()),
                    committed.clone(),
                    vec![candidate(41, "renamed-slug")],
                    1,
                ))
                .is_err()
        );

        assert_eq!(store.load(&crawl_day), Ok(Some(committed)));
        assert_eq!(
            store.selected_candidates_for_day(&crawl_day),
            vec![(41, "first-slug".to_owned())]
        );
        assert_eq!(
            store.job_request_for("source.game-ingestion:2026-08-14:41"),
            Some((
                "source.game-ingestion".to_owned(),
                "metacritic-game:41:first-slug".to_owned(),
                2,
            ))
        );
    }

    #[test]
    fn file_backed_store_survives_close_and_reopen() {
        let database = TemporaryDatabase::new("reopen");
        let crawl_day = day("2026-08-14");
        let expected = state(
            crawl_day.clone(),
            [7],
            true,
            BrowseProgress::Continue(BrowseCursor::new(99)),
        );

        {
            let mut store = database.open();
            store
                .commit(commit(
                    None,
                    expected.clone(),
                    vec![candidate(7, "durable")],
                ))
                .expect("state must commit");
        }

        let mut reopened = database.open();
        assert_eq!(reopened.load(&crawl_day), Ok(Some(expected)));
        assert_eq!(
            reopened.selected_candidates_for_day(&crawl_day),
            vec![(7, "durable".to_owned())]
        );
    }

    #[test]
    fn two_synchronous_stores_observe_serialized_commits() {
        let database = TemporaryDatabase::new("serialized");
        let crawl_day = day("2026-08-14");
        let first_state = state(crawl_day.clone(), [1], true, BrowseProgress::Initial);
        let second_state = state(
            crawl_day.clone(),
            [1, 2],
            true,
            BrowseProgress::Continue(BrowseCursor::new(24)),
        );
        let mut first_store = database.open();
        let mut second_store = database.open();

        first_store
            .commit(commit(
                None,
                first_state.clone(),
                vec![candidate(1, "first")],
            ))
            .expect("first store must commit");
        assert_eq!(second_store.load(&crawl_day), Ok(Some(first_state.clone())));
        second_store
            .commit(commit(
                Some(first_state.clone()),
                second_state.clone(),
                vec![candidate(2, "second")],
            ))
            .expect("second store must commit");

        assert_eq!(first_store.load(&crawl_day), Ok(Some(second_state)));
        assert_eq!(
            first_store.selected_candidates_for_day(&crawl_day),
            vec![(1, "first".to_owned()), (2, "second".to_owned())]
        );
    }

    #[test]
    fn stale_same_day_progress_commit_is_rejected_without_overwriting_the_winner() {
        let database = TemporaryDatabase::new("stale-progress");
        let crawl_day = day("2026-08-14");
        let previous = state(
            crawl_day.clone(),
            [1],
            true,
            BrowseProgress::Continue(BrowseCursor::new(24)),
        );
        let winner = state(crawl_day.clone(), [1], true, BrowseProgress::Exhausted);
        let stale_next = state(
            crawl_day.clone(),
            [1],
            true,
            BrowseProgress::Continue(BrowseCursor::new(48)),
        );
        let mut first_store = database.open();
        let mut second_store = database.open();

        first_store
            .commit(commit(None, previous.clone(), vec![candidate(1, "first")]))
            .expect("initial state must commit");
        let first_previous = first_store
            .load(&crawl_day)
            .expect("first store must load")
            .expect("previous state must exist");
        let second_previous = second_store
            .load(&crawl_day)
            .expect("second store must load")
            .expect("previous state must exist");
        assert_eq!(first_previous, second_previous);

        first_store
            .commit(commit(Some(first_previous), winner.clone(), vec![]))
            .expect("winning progress update must commit");
        assert!(
            second_store
                .commit(commit(Some(second_previous), stale_next, vec![]))
                .is_err()
        );

        assert_eq!(second_store.load(&crawl_day), Ok(Some(winner)));
        assert_eq!(
            second_store.selected_candidates_for_day(&crawl_day),
            vec![(1, "first".to_owned())]
        );
    }

    #[test]
    fn incompatible_version_zero_schema_fails_without_advancing_the_version() {
        let database = TemporaryDatabase::new("incompatible-version-zero");
        let connection = Connection::open(&database.path).expect("raw test database must open");
        connection
            .execute_batch("CREATE TABLE crawl_days (day_key TEXT PRIMARY KEY);")
            .expect("incompatible test table must create");
        drop(connection);

        assert!(SqliteDailyCrawlStateStore::open(&database.path).is_err());

        let connection = Connection::open(&database.path).expect("raw test database must reopen");
        let version = connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("schema version must load");
        assert_eq!(version, 0);
    }

    #[test]
    fn incomplete_version_one_schema_fails_during_reopen() {
        let database = TemporaryDatabase::new("incomplete-version-one");
        let connection = Connection::open(&database.path).expect("raw test database must open");
        connection
            .execute_batch(
                "CREATE TABLE crawl_days (
                    day_key TEXT PRIMARY KEY NOT NULL CHECK (length(trim(day_key)) > 0),
                    new_releases_completed INTEGER NOT NULL CHECK (new_releases_completed IN (0, 1)),
                    browse_progress TEXT NOT NULL CHECK (browse_progress IN ('initial', 'continue', 'exhausted')),
                    browse_cursor TEXT,
                    CHECK (
                        (browse_progress = 'continue' AND browse_cursor IS NOT NULL)
                        OR (browse_progress IN ('initial', 'exhausted') AND browse_cursor IS NULL)
                    )
                );
                PRAGMA user_version = 1;",
            )
            .expect("incomplete version-one test schema must create");
        drop(connection);

        assert!(SqliteDailyCrawlStateStore::open(&database.path).is_err());
    }

    #[test]
    fn version_one_schema_with_rowid_relations_fails_during_reopen() {
        let database = TemporaryDatabase::new("rowid-relations-version-one");
        let schema = DAILY_CRAWL_MIGRATION_0001.replace(") WITHOUT ROWID;", ");");
        create_version_one_schema(&database, &schema);

        assert!(SqliteDailyCrawlStateStore::open(&database.path).is_err());
    }

    #[test]
    fn version_one_schema_with_split_candidate_foreign_keys_fails_during_reopen() {
        let database = TemporaryDatabase::new("split-candidate-foreign-keys-version-one");
        let schema = DAILY_CRAWL_MIGRATION_0001.replace(
            "    FOREIGN KEY (day_key, source_product_id)\n        REFERENCES crawl_day_selected_or_processed(day_key, source_product_id)\n        ON DELETE RESTRICT",
            "    FOREIGN KEY (day_key)\n        REFERENCES crawl_day_selected_or_processed(day_key)\n        ON DELETE RESTRICT,\n    FOREIGN KEY (source_product_id)\n        REFERENCES crawl_day_selected_or_processed(source_product_id)\n        ON DELETE RESTRICT",
        );
        let schema = format!(
            "{schema}\nCREATE UNIQUE INDEX crawl_day_selected_or_processed_day_key_unique\n ON crawl_day_selected_or_processed(day_key);\nCREATE UNIQUE INDEX crawl_day_selected_or_processed_source_product_id_unique\n ON crawl_day_selected_or_processed(source_product_id);"
        );
        create_version_one_schema(&database, &schema);

        assert!(SqliteDailyCrawlStateStore::open(&database.path).is_err());
    }

    #[test]
    fn version_one_schema_with_extra_composite_candidate_foreign_key_fails_during_reopen() {
        let database = TemporaryDatabase::new("extra-candidate-foreign-key-version-one");
        let schema = DAILY_CRAWL_MIGRATION_0001.replace(
            "        REFERENCES crawl_day_selected_or_processed(day_key, source_product_id)\n        ON DELETE RESTRICT\n) WITHOUT ROWID;",
            "        REFERENCES crawl_day_selected_or_processed(day_key, source_product_id)\n        ON DELETE RESTRICT,\n    FOREIGN KEY (day_key, source_product_id)\n        REFERENCES crawl_day_selected_or_processed(day_key, source_product_id)\n        ON DELETE RESTRICT\n) WITHOUT ROWID;",
        );
        create_version_one_schema(&database, &schema);

        assert!(SqliteDailyCrawlStateStore::open(&database.path).is_err());
    }

    #[test]
    fn version_one_schema_without_required_checks_fails_during_reopen() {
        let database = TemporaryDatabase::new("missing-checks-version-one");
        let schema = DAILY_CRAWL_MIGRATION_0001
            .replace(" CHECK (length(trim(day_key)) > 0)", "")
            .replace(" CHECK (new_releases_completed IN (0, 1))", "")
            .replace(
                " CHECK (browse_progress IN ('initial', 'continue', 'exhausted'))",
                "",
            )
            .replace(
                "    browse_cursor TEXT,\n    CHECK (\n        (browse_progress = 'continue' AND browse_cursor IS NOT NULL)\n        OR (browse_progress IN ('initial', 'exhausted') AND browse_cursor IS NULL)\n    )\n",
                "    browse_cursor TEXT\n",
            )
            .replace(" CHECK (length(source_product_id) > 0)", "")
            .replace(" CHECK (length(source_slug) > 0)", "");
        create_version_one_schema(&database, &schema);

        assert!(SqliteDailyCrawlStateStore::open(&database.path).is_err());
    }

    #[test]
    fn complete_weakened_version_one_lookalike_fails_during_reopen() {
        let database = TemporaryDatabase::new("weakened-lookalike-version-one");
        create_version_one_schema(
            &database,
            "CREATE TABLE crawl_days (
                day_key TEXT PRIMARY KEY NOT NULL /* CHECK (length(trim(day_key)) > 0) */,
                new_releases_completed INTEGER NOT NULL /* CHECK (new_releases_completed IN (0, 1)) */,
                browse_progress TEXT NOT NULL /* CHECK (browse_progress IN ('initial', 'continue', 'exhausted')) */,
                browse_cursor TEXT /* CHECK ((browse_progress = 'continue' AND browse_cursor IS NOT NULL) OR (browse_progress IN ('initial', 'exhausted') AND browse_cursor IS NULL)) */
            );
            CREATE TABLE crawl_day_selected_or_processed (
                day_key TEXT NOT NULL REFERENCES crawl_days(day_key) ON DELETE RESTRICT,
                source_product_id TEXT NOT NULL /* CHECK (length(source_product_id) > 0) */,
                PRIMARY KEY (day_key, source_product_id)
            ) /* WITHOUT ROWID */;
            CREATE TABLE crawl_day_selected_candidates (
                day_key TEXT NOT NULL,
                source_product_id TEXT NOT NULL,
                source_slug TEXT NOT NULL /* CHECK (length(source_slug) > 0) */,
                PRIMARY KEY (day_key, source_product_id),
                FOREIGN KEY (day_key)
                    REFERENCES crawl_day_selected_or_processed(day_key)
                    ON DELETE RESTRICT,
                FOREIGN KEY (source_product_id)
                    REFERENCES crawl_day_selected_or_processed(source_product_id)
                    ON DELETE RESTRICT
            ) /* WITHOUT ROWID */;",
        );

        assert!(SqliteDailyCrawlStateStore::open(&database.path).is_err());
    }

    #[test]
    fn malformed_persisted_numeric_values_fail_explicitly() {
        let mut store = SqliteDailyCrawlStateStore::open_in_memory().expect("store must open");
        let crawl_day = day("2026-08-14");
        store
            .connection
            .execute(
                "INSERT INTO crawl_days (
                    day_key,
                    new_releases_completed,
                    browse_progress,
                    browse_cursor
                ) VALUES (?1, 1, 'initial', NULL)",
                params![crawl_day.as_str()],
            )
            .expect("test state must insert");
        store
            .connection
            .execute(
                "INSERT INTO crawl_day_selected_or_processed (day_key, source_product_id)
                 VALUES (?1, '001')",
                params![crawl_day.as_str()],
            )
            .expect("test malformed identity must insert");

        let error = store
            .load(&crawl_day)
            .expect_err("malformed numeric identity must fail");
        assert!(
            error
                .to_string()
                .contains("malformed persisted daily crawl state")
        );
    }
}
