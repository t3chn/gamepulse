#![forbid(unsafe_code)]

#[allow(dead_code)]
#[path = "../src/runtime.rs"]
mod runtime;

use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::to_bytes;
use gamepulse_application::{
    AsyncReviewSourceIngestionPort, FailureCategoryCounts, GameReviewRefresh,
    GameReviewRefreshStore, JobHandler, JobHandlerRegistry, JobRequest, JobStatus, JobStore,
    JobTimestamp, ReviewExcerpt, ReviewInput, ReviewKind, ReviewRefreshFingerprint, ReviewSummary,
    ReviewSummaryJobSchedule, ReviewSummaryOutput, ReviewSummaryRequest, ReviewSummaryStore,
    RuntimeJobType, RuntimeJobTypeFilter, SourceIngestionRequest, SourceProductId,
};
use gamepulse_storage_sqlite::{
    SqliteGameCatalogueReadStore, SqliteJobStore, SqliteReviewSummaryStore,
};
use gamepulse_worker_llm::{LocalExtractiveReviewSummarizer, ReviewSummaryHandler};
use gamepulse_worker_source::{
    GameDetail, GameIdentity, GameIngestionTransport, MetacriticGameReviewSource, PlatformDetail,
    PlatformUserScore, ReviewPage, ReviewSourceIngestionHandler, SourceIngestionFailureCategory,
    parse_game_detail, parse_platform_user_score_for_snapshot, parse_review_page,
};
use runtime::{Runtime, RuntimeClock, RuntimeClockError, RuntimeConfig, RuntimeTaskOutcome};
use rusqlite::{Connection, params};

const DETAIL: &str =
    include_str!("../../gamepulse-worker-source/tests/fixtures/product-detail.json");
const USER_SCORE: &str =
    include_str!("../../gamepulse-worker-source/tests/fixtures/user-score.json");
const CRITIC_REVIEWS: &str =
    include_str!("../../gamepulse-worker-source/tests/fixtures/m011-critic-review-page.json");
const USER_REVIEWS: &str =
    include_str!("../../gamepulse-worker-source/tests/fixtures/m011-user-review-page.json");
const DAILY_CRAWL_MIGRATION_0001: &str =
    include_str!("../../gamepulse-storage-sqlite/migrations/0001_daily_crawl_state.sql");
const JOB_QUEUE_MIGRATION_0002: &str =
    include_str!("../../gamepulse-storage-sqlite/migrations/0002_job_queue.sql");
const GAME_SNAPSHOT_MIGRATION_0003: &str =
    include_str!("../../gamepulse-storage-sqlite/migrations/0003_game_snapshots.sql");
const REVIEW_SUMMARY_MIGRATION_0004: &str =
    include_str!("../../gamepulse-storage-sqlite/migrations/0004_review_summaries.sql");
const PUBLIC_COVER_URL_MIGRATION_0005: &str =
    include_str!("../../gamepulse-storage-sqlite/migrations/0005_public_cover_url.sql");
const LEGACY_REVIEW_EXCERPT: &str = "A great legacy review excerpt.";
const LEGACY_REVIEW_CONTENT_HASH: &str =
    "00bcb53e4dcdb6a2fb1614b107de5495101bd18f9fc776b19713e16eb6c437f1";
const LEGACY_REVIEW_FINGERPRINT: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

static NEXT_DATABASE: AtomicU64 = AtomicU64::new(0);

struct TemporaryDatabase {
    path: PathBuf,
}

impl TemporaryDatabase {
    fn new() -> Self {
        let sequence = NEXT_DATABASE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "gamepulse-m011-review-summary-{}-{sequence}.sqlite3",
            std::process::id()
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

fn seed_version_five_pending_review_summary(database: &TemporaryDatabase) {
    let connection = Connection::open(&database.path).expect("legacy database must open");
    for migration in [
        DAILY_CRAWL_MIGRATION_0001,
        JOB_QUEUE_MIGRATION_0002,
        GAME_SNAPSHOT_MIGRATION_0003,
        REVIEW_SUMMARY_MIGRATION_0004,
        PUBLIC_COVER_URL_MIGRATION_0005,
    ] {
        connection
            .execute_batch(migration)
            .expect("legacy migration must apply");
    }
    connection
        .execute(
            "INSERT INTO games (source_product_id, source_slug, title, description)
             VALUES (?1, 'legacy-review-game', 'Legacy Review Game', 'Synthetic description')",
            params![101_i64],
        )
        .expect("legacy game must persist");
    connection
        .execute(
            "INSERT INTO review_inputs (
                game_source_product_id, review_kind, content_hash, refresh_fingerprint
             ) VALUES (?1, 'critic', ?2, ?3)",
            params![
                101_i64,
                LEGACY_REVIEW_CONTENT_HASH,
                LEGACY_REVIEW_FINGERPRINT
            ],
        )
        .expect("legacy review input must persist");
    connection
        .execute(
            "INSERT INTO review_input_excerpts (
                game_source_product_id, review_kind, excerpt_position, excerpt
             ) VALUES (?1, 'critic', 0, ?2)",
            params![101_i64, LEGACY_REVIEW_EXCERPT],
        )
        .expect("legacy review excerpt must persist");
    connection
        .execute(
            "INSERT INTO review_summaries (
                game_source_product_id, review_kind, refresh_fingerprint, state
             ) VALUES (?1, 'critic', ?2, 'pending')",
            params![101_i64, LEGACY_REVIEW_FINGERPRINT],
        )
        .expect("legacy pending summary must persist");
    connection
        .pragma_update(None, "user_version", 5_i64)
        .expect("legacy schema version must persist");
}

#[test]
fn seeded_v5_nonempty_review_input_reopens_and_settles_pending_local_summary() {
    let database = TemporaryDatabase::new();
    seed_version_five_pending_review_summary(&database);

    let source_product_id = SourceProductId::new(101).expect("test identity must be valid");
    let request = ReviewSummaryRequest::new(
        source_product_id,
        ReviewKind::Critic,
        ReviewRefreshFingerprint::parse(LEGACY_REVIEW_FINGERPRINT)
            .expect("legacy fingerprint must be valid"),
    );
    let mut store = SqliteReviewSummaryStore::open(&database.path)
        .expect("v5 database must migrate and reopen");
    let input = store
        .load_review_input(&request)
        .expect("migrated legacy review input must validate")
        .expect("pending legacy review input must remain current");

    assert_eq!(input.content_hash().as_str(), LEGACY_REVIEW_CONTENT_HASH);
    assert_eq!(input.excerpts()[0].as_str(), LEGACY_REVIEW_EXCERPT);
    assert_eq!(input.excerpts()[0].polarity(), None);

    let output = gamepulse_application::ReviewSummarizer::summarize(
        &LocalExtractiveReviewSummarizer,
        &input,
    )
    .expect("local summary must remain available after migration");
    assert_eq!(
        output,
        ReviewSummaryOutput::available(vec![LEGACY_REVIEW_EXCERPT.to_owned()], Vec::new())
            .expect("test summary must be valid")
    );
    assert_eq!(
        store
            .persist_review_summary(&ReviewSummary::new(request, output))
            .expect("local summary must settle"),
        gamepulse_application::FencedSummaryWrite::Applied
    );
}

#[derive(Clone, Copy)]
struct FixedClock(i64);

impl RuntimeClock for FixedClock {
    fn now(&self) -> Result<JobTimestamp, RuntimeClockError> {
        JobTimestamp::new(self.0).map_err(|_| RuntimeClockError::Overflow)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FixtureError {
    Unavailable,
    ReviewContinuationLink,
}

#[derive(Clone, Default)]
struct FixtureTransport {
    calls: Arc<Mutex<Vec<String>>>,
    review_failure: Option<FixtureError>,
    missing_video: bool,
}

impl FixtureTransport {
    fn calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .expect("fixture calls must not be poisoned")
            .clone()
    }

    fn with_review_continuation_failure() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            review_failure: Some(FixtureError::ReviewContinuationLink),
            missing_video: false,
        }
    }

    fn with_missing_video() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            review_failure: None,
            missing_video: true,
        }
    }
}

impl GameIngestionTransport for FixtureTransport {
    type Error = FixtureError;
    type FetchDetailFuture<'a>
        = Pin<Box<dyn Future<Output = Result<GameDetail, Self::Error>> + Send + 'a>>
    where
        Self: 'a;
    type FetchPlatformUserScoreFuture<'a>
        = Pin<Box<dyn Future<Output = Result<PlatformUserScore, Self::Error>> + Send + 'a>>
    where
        Self: 'a;
    type FetchReviewPageFuture<'a>
        = Pin<Box<dyn Future<Output = Result<ReviewPage, Self::Error>> + Send + 'a>>
    where
        Self: 'a;

    fn fetch_game_detail(&self, expected: GameIdentity) -> Self::FetchDetailFuture<'_> {
        self.calls
            .lock()
            .expect("fixture calls must not be poisoned")
            .push(format!("detail:{}", expected.slug));
        let missing_video = self.missing_video;
        Box::pin(async move {
            let mut detail =
                parse_game_detail(&expected, DETAIL).map_err(|_| FixtureError::Unavailable)?;
            if missing_video {
                detail.video = None;
            }
            Ok(detail)
        })
    }

    fn fetch_platform_user_score(
        &self,
        expected_game: GameIdentity,
        expected_platform: PlatformDetail,
    ) -> Self::FetchPlatformUserScoreFuture<'_> {
        self.calls
            .lock()
            .expect("fixture calls must not be poisoned")
            .push(format!("score:{}", expected_platform.slug));
        let body = if expected_platform.slug == "pc" {
            USER_SCORE.to_owned()
        } else {
            USER_SCORE
                .replace("/platform/pc/", "/platform/console/")
                .replace("\"score\": 8.4", "\"score\": null")
        };
        Box::pin(async move {
            parse_platform_user_score_for_snapshot(&expected_game, &expected_platform, &body)
                .map_err(|_| FixtureError::Unavailable)
        })
    }

    fn fetch_review_page(
        &self,
        expected_game: GameIdentity,
        kind: ReviewKind,
        offset: u32,
        limit: u32,
    ) -> Self::FetchReviewPageFuture<'_> {
        self.calls
            .lock()
            .expect("fixture calls must not be poisoned")
            .push(format!("review:{}:{offset}:{limit}", kind.as_str()));
        if let Some(error) = self.review_failure {
            return Box::pin(async move { Err(error) });
        }
        let body = match kind {
            ReviewKind::Critic => CRITIC_REVIEWS,
            ReviewKind::User => USER_REVIEWS,
        };
        let slug = expected_game.slug;
        Box::pin(async move {
            parse_review_page(kind, &slug, offset, limit, body)
                .map_err(|_| FixtureError::Unavailable)
        })
    }

    fn review_page_failure_category(&self, error: &Self::Error) -> SourceIngestionFailureCategory {
        match error {
            FixtureError::ReviewContinuationLink => {
                SourceIngestionFailureCategory::ReviewContinuationLink
            }
            FixtureError::Unavailable => SourceIngestionFailureCategory::OtherMandatoryStage,
        }
    }
}

fn timestamp(value: i64) -> JobTimestamp {
    JobTimestamp::new(value).expect("test timestamp must be valid")
}

fn source_config() -> RuntimeConfig {
    RuntimeConfig::new(
        "m011-source-worker",
        30,
        1,
        gamepulse_application::HourlyJobSchedule::new(RuntimeJobType::SourceHourlyDiscovery, 1)
            .expect("test schedule must be valid"),
    )
    .expect("test runtime config must be valid")
    .with_claim_filter(RuntimeJobTypeFilter::source_lane())
}

fn llm_config() -> RuntimeConfig {
    RuntimeConfig::worker_only("m011-llm-worker", 30, 1)
        .expect("test runtime config must be valid")
        .with_claim_filter(RuntimeJobTypeFilter::llm_lane())
}

async fn response_text(response: axum::response::Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response must collect")
            .to_vec(),
    )
    .expect("response must be UTF-8")
}

#[tokio::test]
async fn fixture_refresh_creates_two_separated_summary_jobs_and_renders_persisted_results() {
    let database = TemporaryDatabase::new();
    let queue = Arc::new(Mutex::new(
        SqliteJobStore::open(&database.path).expect("queue database must open"),
    ));
    let reviews = Arc::new(Mutex::new(
        SqliteReviewSummaryStore::open(&database.path).expect("review database must open"),
    ));
    let transport = FixtureTransport::default();
    queue
        .lock()
        .expect("queue must not be poisoned")
        .enqueue(
            JobRequest::new(
                "m011-source-refresh",
                RuntimeJobType::SourceGameIngestion.as_str(),
                "metacritic-game:101:example-game",
                1,
                timestamp(10),
            )
            .expect("source job must be valid"),
        )
        .expect("source job must enqueue");
    let source_handler: Arc<dyn JobHandler> = Arc::new(ReviewSourceIngestionHandler::new(
        reviews.clone(),
        MetacriticGameReviewSource::new(transport.clone()),
        ReviewSummaryJobSchedule::new(1).expect("summary schedule must be valid"),
    ));
    let llm_handler: Arc<dyn JobHandler> = Arc::new(ReviewSummaryHandler::new(
        reviews.clone(),
        LocalExtractiveReviewSummarizer,
    ));
    let mut source_runtime = Runtime::new(
        queue.clone(),
        Arc::new(FixedClock(20)),
        source_config(),
        Arc::new(JobHandlerRegistry::new([source_handler]).expect("source registry must be valid")),
    );
    let mut llm_runtime = Runtime::new(
        queue.clone(),
        Arc::new(FixedClock(20)),
        llm_config(),
        Arc::new(JobHandlerRegistry::new([llm_handler]).expect("LLM registry must be valid")),
    );

    assert_eq!(
        llm_runtime
            .dispatch_available()
            .expect("filtered claim must work")
            .claimed,
        0
    );
    assert_eq!(
        source_runtime
            .dispatch_available()
            .expect("source must claim")
            .claimed,
        1
    );
    assert_eq!(
        source_runtime
            .join_all()
            .await
            .expect("source task must join")
            .settled,
        [RuntimeTaskOutcome::Succeeded]
    );
    assert_eq!(
        source_runtime
            .dispatch_available()
            .expect("source filter must work")
            .claimed,
        0
    );
    assert_eq!(
        llm_runtime
            .dispatch_available()
            .expect("LLM must claim first summary")
            .claimed,
        1
    );
    assert_eq!(
        llm_runtime
            .join_all()
            .await
            .expect("first summary must join")
            .settled,
        [RuntimeTaskOutcome::Succeeded]
    );
    assert_eq!(
        llm_runtime
            .dispatch_available()
            .expect("LLM must claim second summary")
            .claimed,
        1
    );
    assert_eq!(
        llm_runtime
            .join_all()
            .await
            .expect("second summary must join")
            .settled,
        [RuntimeTaskOutcome::Succeeded]
    );
    drop(source_runtime);
    drop(llm_runtime);
    drop(queue);
    drop(reviews);

    assert_eq!(
        transport.calls(),
        [
            "detail:example-game",
            "score:pc",
            "score:console",
            "review:critic:0:20",
            "review:user:0:20",
        ]
    );
    let reopened = rusqlite::Connection::open(&database.path).expect("database must reopen");
    let inputs = reopened
        .query_row(
            "SELECT COUNT(*) FROM review_inputs WHERE game_source_product_id = 101",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("review inputs must reopen");
    let jobs = reopened
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE job_type = 'llm.review-summary'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("summary jobs must reopen");
    let polarities = reopened
        .prepare(
            "SELECT review_kind, excerpt_position, polarity
             FROM review_input_excerpts
             WHERE game_source_product_id = 101
             ORDER BY review_kind ASC, excerpt_position ASC",
        )
        .expect("polarity query must prepare")
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .expect("polarity query must execute")
        .collect::<Result<Vec<_>, _>>()
        .expect("polarity rows must decode");
    assert_eq!(inputs, 2);
    assert_eq!(jobs, 2);
    let games_with_video = reopened
        .query_row(
            "SELECT COUNT(*) FROM games WHERE video_url IS NOT NULL",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("mandatory video count must reopen");
    assert_eq!(games_with_video, 1);
    assert_eq!(
        polarities,
        [
            ("critic".to_owned(), 0, Some("positive".to_owned())),
            ("critic".to_owned(), 1, None),
            ("user".to_owned(), 0, Some("positive".to_owned())),
            ("user".to_owned(), 1, Some("negative".to_owned())),
        ]
    );
    let mut queue = SqliteJobStore::open(&database.path).expect("queue must reopen");
    assert_eq!(
        queue
            .job("m011-source-refresh")
            .expect("source job must load")
            .expect("source job must exist")
            .status(),
        JobStatus::Succeeded
    );

    let catalogue = Arc::new(Mutex::new(
        SqliteGameCatalogueReadStore::open(&database.path).expect("catalogue must open"),
    ));
    let detail = response_text(gamepulse_web::game_detail_response(catalogue, 101).await).await;
    assert!(detail.contains("Critics praise the synthetic controls."));
    assert!(detail.contains("Critics dislike the boring synthetic finale."));
    assert!(detail.contains("Users praise the synthetic co-op mode."));
    assert!(detail.contains("Users report poor synthetic performance."));
}

#[tokio::test]
async fn source_review_continuation_failure_is_durable_and_leaves_no_partial_refresh() {
    let database = TemporaryDatabase::new();
    let queue = Arc::new(Mutex::new(
        SqliteJobStore::open(&database.path).expect("queue database must open"),
    ));
    let reviews = Arc::new(Mutex::new(
        SqliteReviewSummaryStore::open(&database.path).expect("review database must open"),
    ));
    queue
        .lock()
        .expect("queue must not be poisoned")
        .enqueue(
            JobRequest::new(
                "m011-source-failure",
                RuntimeJobType::SourceGameIngestion.as_str(),
                "metacritic-game:101:example-game",
                1,
                timestamp(10),
            )
            .expect("source job must be valid"),
        )
        .expect("source job must enqueue");
    let source_handler: Arc<dyn JobHandler> = Arc::new(ReviewSourceIngestionHandler::new(
        reviews.clone(),
        MetacriticGameReviewSource::new(FixtureTransport::with_review_continuation_failure()),
        ReviewSummaryJobSchedule::new(1).expect("summary schedule must be valid"),
    ));
    let mut runtime = Runtime::new(
        queue.clone(),
        Arc::new(FixedClock(20)),
        source_config(),
        Arc::new(JobHandlerRegistry::new([source_handler]).expect("source registry must be valid")),
    );

    assert_eq!(
        runtime
            .dispatch_available()
            .expect("source must claim")
            .claimed,
        1
    );
    let _ = runtime.join_all().await.expect("source task must join");
    drop(runtime);
    drop(queue);
    drop(reviews);

    let reopened = rusqlite::Connection::open(&database.path).expect("database must reopen");
    for table in ["games", "review_inputs", "review_summaries"] {
        let count = reopened
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count must load");
        assert_eq!(count, 0, "{table} must remain empty after a source failure");
    }
    let mut attempts = SqliteJobStore::open(&database.path).expect("queue database must reopen");
    assert_eq!(
        attempts
            .attempts("m011-source-failure")
            .expect("attempt history must load")
            .as_slice()
            .first()
            .and_then(gamepulse_application::JobAttempt::error),
        Some("review_continuation_link")
    );
}

#[tokio::test]
async fn valid_missing_video_fixture_remains_ingestible() {
    let transport = FixtureTransport::with_missing_video();
    let source = MetacriticGameReviewSource::new(transport.clone());
    let outcome = source
        .ingest_reviews(
            SourceIngestionRequest::new(101, "example-game")
                .expect("fixture request must be valid"),
        )
        .await;

    let ingested = outcome.expect("source-omitted video must not reject a valid game");
    assert!(ingested.snapshot().video().is_none());
    assert_eq!(
        transport.calls(),
        [
            "detail:example-game",
            "score:pc",
            "score:console",
            "review:critic:0:20",
            "review:user:0:20",
        ]
    );
}

#[tokio::test]
async fn source_omitted_video_persists_and_enqueues_summaries() {
    let database = TemporaryDatabase::new();
    let queue = Arc::new(Mutex::new(
        SqliteJobStore::open(&database.path).expect("queue database must open"),
    ));
    let reviews = Arc::new(Mutex::new(
        SqliteReviewSummaryStore::open(&database.path).expect("review database must open"),
    ));
    let transport = FixtureTransport::with_missing_video();
    queue
        .lock()
        .expect("queue must not be poisoned")
        .enqueue(
            JobRequest::new(
                "m035-source-missing-video",
                RuntimeJobType::SourceGameIngestion.as_str(),
                "metacritic-game:101:example-game",
                1,
                timestamp(10),
            )
            .expect("source job must be valid"),
        )
        .expect("source job must enqueue");
    let source_handler: Arc<dyn JobHandler> = Arc::new(ReviewSourceIngestionHandler::new(
        reviews.clone(),
        MetacriticGameReviewSource::new(transport.clone()),
        ReviewSummaryJobSchedule::new(1).expect("summary schedule must be valid"),
    ));
    let mut runtime = Runtime::new(
        queue.clone(),
        Arc::new(FixedClock(20)),
        source_config(),
        Arc::new(JobHandlerRegistry::new([source_handler]).expect("source registry must be valid")),
    );

    assert_eq!(
        runtime
            .dispatch_available()
            .expect("source must claim")
            .claimed,
        1
    );
    let settled = runtime.join_all().await.expect("source task must join");
    assert_eq!(settled.settled, [RuntimeTaskOutcome::Succeeded]);
    assert_eq!(settled.observed_failures.missing_required_video(), 0);
    drop(runtime);
    drop(queue);
    drop(reviews);

    let reopened = rusqlite::Connection::open(&database.path).expect("database must reopen");
    for (table, expected) in [("games", 1), ("review_inputs", 2), ("review_summaries", 2)] {
        let count = reopened
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count must load");
        assert_eq!(count, expected, "{table} must persist source data");
    }
    let summary_jobs = reopened
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE job_type = 'llm.review-summary'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("summary job count must load");
    assert_eq!(summary_jobs, 2);
    let mut attempts = SqliteJobStore::open(&database.path).expect("queue database must reopen");
    assert_eq!(
        attempts
            .job("m035-source-missing-video")
            .expect("source job must load")
            .expect("source job must exist")
            .status(),
        JobStatus::Succeeded
    );
    assert_eq!(
        attempts
            .attempts("m035-source-missing-video")
            .expect("attempt history must load")
            .as_slice()
            .first()
            .and_then(gamepulse_application::JobAttempt::error),
        None
    );
    assert_eq!(transport.calls().len(), 5);
}

#[tokio::test]
async fn source_omitted_video_is_consistently_ingestible() {
    let mut observed = FailureCategoryCounts::zero();
    for _ in 0..2 {
        let database = TemporaryDatabase::new();
        let queue = Arc::new(Mutex::new(
            SqliteJobStore::open(&database.path).expect("queue database must open"),
        ));
        let reviews = Arc::new(Mutex::new(
            SqliteReviewSummaryStore::open(&database.path).expect("review database must open"),
        ));
        let transport = FixtureTransport::with_missing_video();
        queue
            .lock()
            .expect("queue must not be poisoned")
            .enqueue(
                JobRequest::new(
                    "m035-source-missing-video",
                    RuntimeJobType::SourceGameIngestion.as_str(),
                    "metacritic-game:101:example-game",
                    1,
                    timestamp(10),
                )
                .expect("source job must be valid"),
            )
            .expect("source job must enqueue");
        let source_handler: Arc<dyn JobHandler> = Arc::new(ReviewSourceIngestionHandler::new(
            reviews,
            MetacriticGameReviewSource::new(transport.clone()),
            ReviewSummaryJobSchedule::new(1).expect("summary schedule must be valid"),
        ));
        let mut runtime = Runtime::new(
            queue.clone(),
            Arc::new(FixedClock(20)),
            source_config(),
            Arc::new(JobHandlerRegistry::new([source_handler]).expect("registry must be valid")),
        );

        assert_eq!(
            runtime
                .dispatch_available()
                .expect("job must claim")
                .claimed,
            1
        );
        let settled = runtime.join_all().await.expect("source task must join");
        assert_eq!(settled.settled, [RuntimeTaskOutcome::Succeeded]);
        observed.merge(settled.observed_failures);
        assert_eq!(transport.calls().len(), 5);

        drop(runtime);
        drop(queue);
        let mut attempts =
            SqliteJobStore::open(&database.path).expect("queue database must reopen");
        let record = attempts
            .job("m035-source-missing-video")
            .expect("source job must load")
            .expect("source job must exist");
        assert_eq!(record.status(), JobStatus::Succeeded);
        assert_eq!(
            attempts
                .attempts("m035-source-missing-video")
                .expect("attempt history must load")
                .first()
                .and_then(gamepulse_application::JobAttempt::error),
            None
        );
    }
    assert_eq!(observed.missing_required_video(), 0);
}

#[test]
fn replay_is_idempotent_changed_input_gets_two_new_jobs_and_stale_summary_cannot_overwrite() {
    let mut store = SqliteReviewSummaryStore::open_in_memory().expect("review store must open");
    let snapshot = gamepulse_application::GameSnapshot::new(
        SourceProductId::new(101).expect("test identity must be valid"),
        "example-game",
        "Example Game",
        "Synthetic description",
        None,
        None,
        Vec::new(),
        Vec::new(),
    )
    .expect("snapshot must be valid");
    let make_input = |kind, text: &str| {
        ReviewInput::new(
            SourceProductId::new(101).expect("test identity must be valid"),
            kind,
            vec![ReviewExcerpt::new(text).expect("test excerpt must be valid")],
        )
        .expect("input must be valid")
    };
    let schedule = ReviewSummaryJobSchedule::new(1).expect("schedule must be valid");
    let first = GameReviewRefresh::new(
        snapshot.clone(),
        make_input(ReviewKind::Critic, "First critic synthetic praise."),
        make_input(ReviewKind::User, "First user synthetic praise."),
        schedule,
        timestamp(1),
    )
    .expect("first refresh must be valid");
    store
        .persist_review_refresh(&first)
        .expect("first refresh must persist");
    store
        .persist_review_refresh(&first)
        .expect("exact replay must persist idempotently");
    let stale_request = ReviewSummaryRequest::from_work_reference(first.jobs()[0].work_ref())
        .expect("first job reference must parse");
    let changed = GameReviewRefresh::new(
        snapshot,
        make_input(ReviewKind::Critic, "Changed critic synthetic praise."),
        make_input(ReviewKind::User, "First user synthetic praise."),
        schedule,
        timestamp(2),
    )
    .expect("changed refresh must be valid");
    assert_ne!(first.fingerprint(), changed.fingerprint());
    store
        .persist_review_refresh(&changed)
        .expect("changed refresh must persist");
    assert_eq!(
        store
            .persist_review_summary(&ReviewSummary::new(
                stale_request,
                ReviewSummaryOutput::available(
                    vec!["stale invented output".to_owned()],
                    Vec::new()
                )
                .expect("test output must be valid"),
            ))
            .expect("stale write must be fenced"),
        gamepulse_application::FencedSummaryWrite::Stale
    );
    let current_request = ReviewSummaryRequest::from_work_reference(changed.jobs()[0].work_ref())
        .expect("changed job reference must parse");
    assert!(
        store
            .load_review_input(&current_request)
            .expect("current input must load")
            .is_some()
    );
}

#[tokio::test]
async fn exact_refresh_replay_preserves_ready_summary_items_in_detail() {
    let database = TemporaryDatabase::new();
    let mut store = SqliteReviewSummaryStore::open(&database.path).expect("review store must open");
    let source_product_id = SourceProductId::new(303).expect("test identity must be valid");
    let refresh = GameReviewRefresh::new(
        gamepulse_application::GameSnapshot::new(
            source_product_id,
            "replay-ready-game",
            "Replay Ready Game",
            "Synthetic description",
            None,
            None,
            Vec::new(),
            Vec::new(),
        )
        .expect("test snapshot must be valid"),
        ReviewInput::new(
            source_product_id,
            ReviewKind::Critic,
            vec![
                ReviewExcerpt::new("Stored synthetic critic praise.")
                    .expect("test excerpt must be valid"),
            ],
        )
        .expect("critic input must be valid"),
        ReviewInput::new(source_product_id, ReviewKind::User, Vec::new())
            .expect("user input must be valid"),
        ReviewSummaryJobSchedule::new(1).expect("summary schedule must be valid"),
        timestamp(1),
    )
    .expect("refresh must be valid");
    store
        .persist_review_refresh(&refresh)
        .expect("first refresh must persist");
    for job in refresh.jobs() {
        let request = ReviewSummaryRequest::from_work_reference(job.work_ref())
            .expect("summary job reference must parse");
        let output = match request.kind() {
            ReviewKind::Critic => ReviewSummaryOutput::available(
                vec!["Stored synthetic critic praise.".to_owned()],
                Vec::new(),
            )
            .expect("critic output must be valid"),
            ReviewKind::User => ReviewSummaryOutput::Unavailable,
        };
        store
            .persist_review_summary(&ReviewSummary::new(request, output))
            .expect("ready summary must persist");
    }
    store
        .persist_review_refresh(&refresh)
        .expect("exact refresh replay must preserve summaries");
    drop(store);

    let catalogue = Arc::new(Mutex::new(
        SqliteGameCatalogueReadStore::open(&database.path).expect("catalogue must open"),
    ));
    let detail = response_text(gamepulse_web::game_detail_response(catalogue, 303).await).await;
    assert!(detail.contains("Stored synthetic critic praise."));
    assert!(detail.contains("Unavailable: no stored user excerpts."));
    assert!(!detail.contains("Summary pending for the current stored review refresh."));
}

#[tokio::test]
async fn absent_stored_excerpts_render_an_explicit_unavailable_state() {
    let database = TemporaryDatabase::new();
    let mut store = SqliteReviewSummaryStore::open(&database.path).expect("review store must open");
    let source_product_id = SourceProductId::new(202).expect("test identity must be valid");
    let snapshot = gamepulse_application::GameSnapshot::new(
        source_product_id,
        "empty-review-game",
        "Empty Review Game",
        "Synthetic description",
        None,
        None,
        Vec::new(),
        Vec::new(),
    )
    .expect("snapshot must be valid");
    let refresh = GameReviewRefresh::new(
        snapshot,
        ReviewInput::new(source_product_id, ReviewKind::Critic, Vec::new())
            .expect("empty critic input must be valid"),
        ReviewInput::new(source_product_id, ReviewKind::User, Vec::new())
            .expect("empty user input must be valid"),
        ReviewSummaryJobSchedule::new(1).expect("schedule must be valid"),
        timestamp(1),
    )
    .expect("refresh must be valid");
    store
        .persist_review_refresh(&refresh)
        .expect("refresh must persist");
    let fallback = LocalExtractiveReviewSummarizer;
    for job in refresh.jobs() {
        let request = ReviewSummaryRequest::from_work_reference(job.work_ref())
            .expect("summary job reference must parse");
        let input = store
            .load_review_input(&request)
            .expect("input must load")
            .expect("current input must exist");
        let output = gamepulse_application::ReviewSummarizer::summarize(&fallback, &input)
            .expect("fallback must be infallible");
        store
            .persist_review_summary(&ReviewSummary::new(request, output))
            .expect("unavailable summary must persist");
    }
    drop(store);

    let catalogue = Arc::new(Mutex::new(
        SqliteGameCatalogueReadStore::open(&database.path).expect("catalogue must open"),
    ));
    let detail = response_text(gamepulse_web::game_detail_response(catalogue, 202).await).await;
    assert!(detail.contains("Unavailable: no stored critic excerpts."));
    assert!(detail.contains("Unavailable: no stored user excerpts."));
}
