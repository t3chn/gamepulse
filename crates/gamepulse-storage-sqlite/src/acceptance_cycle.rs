use std::fmt;
use std::path::Path;

use gamepulse_application::{
    AcceptanceCycleReadPort, AcceptanceCycleSnapshot, AcceptanceFailureCategories,
    AcceptanceJobProgress,
};
use rusqlite::{Connection, params};

const SOURCE_INGESTION_JOB_TYPE: &str = "source.game-ingestion";
const REVIEW_SUMMARY_JOB_TYPE: &str = "llm.review-summary";
const REVIEW_CONTINUATION_LINK: &str = "review_continuation_link";

/// SQLite aggregate read adapter for the fresh one-shot acceptance cycle.
///
/// Its only public output is the application-owned aggregate projection. It
/// never returns raw records, job identities, source values, errors, or paths.
pub struct SqliteAcceptanceCycleStore {
    connection: Connection,
}

impl SqliteAcceptanceCycleStore {
    /// Open a file-backed acceptance projection over the already configured database.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AcceptanceCycleReadStoreError> {
        let mut connection = Connection::open(path).map_err(|_| AcceptanceCycleReadStoreError)?;
        super::initialize_connection(&mut connection).map_err(|_| AcceptanceCycleReadStoreError)?;
        Ok(Self { connection })
    }
}

impl AcceptanceCycleReadPort for SqliteAcceptanceCycleStore {
    type Error = AcceptanceCycleReadStoreError;

    fn acceptance_cycle_snapshot(
        &mut self,
    ) -> Result<AcceptanceCycleSnapshot, AcceptanceCycleReadStoreError> {
        let selected = count(
            &self.connection,
            "SELECT COALESCE(
                (SELECT accepted_count FROM runs ORDER BY created_at DESC, run_id DESC LIMIT 1),
                (SELECT COUNT(*) FROM crawl_day_selected_candidates)
             )",
        )?;
        let source_ingestion = job_progress(&self.connection, SOURCE_INGESTION_JOB_TYPE)?;
        let summaries = job_progress(&self.connection, REVIEW_SUMMARY_JOB_TYPE)?;
        let persisted = count(&self.connection, "SELECT COUNT(*) FROM games")?;
        let complete_video = count(
            &self.connection,
            "SELECT COUNT(*) FROM games
             WHERE video_url IS NOT NULL AND length(trim(video_url)) > 0",
        )?;
        let summaries_ready = count(
            &self.connection,
            "SELECT COUNT(*)
             FROM games AS games
             WHERE EXISTS (
                 SELECT 1 FROM review_summaries AS critic
                 WHERE critic.game_source_product_id = games.source_product_id
                   AND critic.review_kind = 'critic'
                   AND critic.state IN ('available', 'unavailable')
             )
               AND EXISTS (
                 SELECT 1 FROM review_summaries AS user
                 WHERE user.game_source_product_id = games.source_product_id
                   AND user.review_kind = 'user'
                   AND user.state IN ('available', 'unavailable')
             )",
        )?;
        let (source_review_continuation_link, source_other_mandatory_stage) = self
            .connection
            .query_row(
                "SELECT
                    COALESCE(SUM(CASE WHEN last_error = ?1 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE
                        WHEN last_error IS NOT NULL AND last_error <> ?1 THEN 1
                        ELSE 0
                    END), 0)
                 FROM jobs
                 WHERE job_type = ?2",
                params![REVIEW_CONTINUATION_LINK, SOURCE_INGESTION_JOB_TYPE],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .map_err(|_| AcceptanceCycleReadStoreError)?;
        let summary_failures = count_with_parameter(
            &self.connection,
            "SELECT COUNT(*) FROM jobs
             WHERE job_type = ?1 AND last_error IS NOT NULL",
            REVIEW_SUMMARY_JOB_TYPE,
        )?;

        Ok(AcceptanceCycleSnapshot::new(
            selected,
            source_ingestion,
            summaries,
            persisted,
            complete_video,
            summaries_ready,
            persisted.saturating_sub(summaries_ready),
            AcceptanceFailureCategories::new(
                as_usize(source_review_continuation_link)?,
                as_usize(source_other_mandatory_stage)?,
                summary_failures,
            ),
        ))
    }
}

fn job_progress(
    connection: &Connection,
    job_type: &str,
) -> Result<AcceptanceJobProgress, AcceptanceCycleReadStoreError> {
    let values = connection
        .query_row(
            "SELECT
                COUNT(*),
                COALESCE(SUM(attempt_count), 0),
                COALESCE(SUM(CASE WHEN state = 'ready' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN state = 'claimed' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN state = 'succeeded' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN state = 'failed' THEN 1 ELSE 0 END), 0)
             FROM jobs
             WHERE job_type = ?1",
            params![job_type],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .map_err(|_| AcceptanceCycleReadStoreError)?;
    Ok(AcceptanceJobProgress::new(
        as_usize(values.0)?,
        as_usize(values.1)?,
        as_usize(values.2)?,
        as_usize(values.3)?,
        as_usize(values.4)?,
        as_usize(values.5)?,
    ))
}

fn count(connection: &Connection, query: &str) -> Result<usize, AcceptanceCycleReadStoreError> {
    let value = connection
        .query_row(query, [], |row| row.get::<_, i64>(0))
        .map_err(|_| AcceptanceCycleReadStoreError)?;
    as_usize(value)
}

fn count_with_parameter(
    connection: &Connection,
    query: &str,
    parameter: &str,
) -> Result<usize, AcceptanceCycleReadStoreError> {
    let value = connection
        .query_row(query, params![parameter], |row| row.get::<_, i64>(0))
        .map_err(|_| AcceptanceCycleReadStoreError)?;
    as_usize(value)
}

fn as_usize(value: i64) -> Result<usize, AcceptanceCycleReadStoreError> {
    usize::try_from(value).map_err(|_| AcceptanceCycleReadStoreError)
}

/// Opaque aggregate-read failure that cannot carry database detail into reports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcceptanceCycleReadStoreError;

impl fmt::Display for AcceptanceCycleReadStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SQLite acceptance aggregate read failed")
    }
}

impl std::error::Error for AcceptanceCycleReadStoreError {}
