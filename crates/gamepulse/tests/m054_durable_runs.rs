#![forbid(unsafe_code)]

#[allow(dead_code)]
#[path = "../src/runtime.rs"]
mod runtime;

use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gamepulse_application::{
    AsyncDailyCrawlSourcePort, AsyncReviewSourceIngestionPort, CrawlDiscoveryRequest,
    DiscoveryCandidate, DiscoveryPage, FailureCategoryCounts, GameSnapshot, GameVideoLink,
    HourlyJobSchedule, JobHandlerRegistry, JobTimestamp, ReviewInput, ReviewKind,
    ReviewSourceIngestion, ReviewSummaryJobSchedule, RuntimeJobType, RuntimeJobTypeFilter,
    SourceIngestionJobSchedule, SourceIngestionRequest, WorkerFailureCategory,
};
use gamepulse_storage_sqlite::{SqliteJobStore, SqliteRunProgressStore};
use gamepulse_worker_source::{
    DurableRunDiscoveryHandler, DurableRunReviewSourceIngestionHandler,
    ReviewSourceFailureClassifier, SourceIngestionFailureCategory, SourceRunClock,
    SourceRunClockError,
};
use runtime::{Runtime, RuntimeClock, RuntimeClockError, RuntimeConfig};

static NEXT_DATABASE: AtomicUsize = AtomicUsize::new(0);
use rusqlite::Connection;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FixtureError {
    MissingVideo,
    Transport,
}

impl std::fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("fixture error")
    }
}

impl std::error::Error for FixtureError {}

struct FixtureDiscovery;

impl AsyncDailyCrawlSourcePort for FixtureDiscovery {
    type Error = FixtureError;
    type DiscoverFuture<'a>
        = Pin<Box<dyn Future<Output = Result<DiscoveryPage, FixtureError>> + Send + 'a>>
    where
        Self: 'a;

    fn discover(&self, request: CrawlDiscoveryRequest) -> Self::DiscoverFuture<'_> {
        Box::pin(async move {
            if request != CrawlDiscoveryRequest::NewReleases {
                return Err(FixtureError::MissingVideo);
            }
            Ok(DiscoveryPage::new(
                (1..=21)
                    .map(|id| {
                        DiscoveryCandidate::new(id, format!("fixture-{id}"))
                            .expect("fixture candidate must be valid")
                    })
                    .collect(),
                None,
            ))
        })
    }
}

#[derive(Clone)]
struct ExhaustedDiscovery {
    calls: Arc<Mutex<usize>>,
}

impl AsyncDailyCrawlSourcePort for ExhaustedDiscovery {
    type Error = FixtureError;
    type DiscoverFuture<'a>
        = Pin<Box<dyn Future<Output = Result<DiscoveryPage, FixtureError>> + Send + 'a>>
    where
        Self: 'a;

    fn discover(&self, _request: CrawlDiscoveryRequest) -> Self::DiscoverFuture<'_> {
        let calls = Arc::clone(&self.calls);
        Box::pin(async move {
            *calls
                .lock()
                .expect("fixture discovery calls lock must be available") += 1;
            Ok(DiscoveryPage::new(Vec::new(), None))
        })
    }
}

#[derive(Clone)]
struct FixtureReviews {
    calls: Arc<Mutex<Vec<u64>>>,
    first_error: FixtureError,
}

impl FixtureReviews {
    fn new(calls: Arc<Mutex<Vec<u64>>>, first_error: FixtureError) -> Self {
        Self { calls, first_error }
    }
}

impl AsyncReviewSourceIngestionPort for FixtureReviews {
    type Error = FixtureError;
    type IngestFuture<'a>
        = Pin<Box<dyn Future<Output = Result<ReviewSourceIngestion, FixtureError>> + Send + 'a>>
    where
        Self: 'a;

    fn ingest_reviews(&self, request: SourceIngestionRequest) -> Self::IngestFuture<'_> {
        let calls = Arc::clone(&self.calls);
        Box::pin(async move {
            calls
                .lock()
                .expect("fixture calls lock must be available")
                .push(request.source_product_id().value());
            if request.source_product_id().value() == 1 {
                return Err(self.first_error);
            }
            let source_product_id = request.source_product_id();
            let snapshot = GameSnapshot::new(
                source_product_id,
                request.source_slug(),
                "Fixture game",
                "Fixture description",
                None,
                Some(GameVideoLink::new("fixture-video").expect("fixture video must be valid")),
                Vec::new(),
                Vec::new(),
            )
            .expect("fixture snapshot must be valid");
            ReviewSourceIngestion::new(
                snapshot,
                ReviewInput::new(source_product_id, ReviewKind::Critic, Vec::new())
                    .expect("fixture critic input must be valid"),
                ReviewInput::new(source_product_id, ReviewKind::User, Vec::new())
                    .expect("fixture user input must be valid"),
            )
            .map_err(|_| FixtureError::MissingVideo)
        })
    }
}

#[derive(Clone)]
struct SharedClock {
    next: Arc<AtomicI64>,
}

impl SharedClock {
    fn new(start: i64) -> Self {
        Self {
            next: Arc::new(AtomicI64::new(start)),
        }
    }

    fn now_value(&self) -> JobTimestamp {
        JobTimestamp::new(self.next.fetch_add(1, Ordering::SeqCst))
            .expect("fixture clock value must be valid")
    }
}

impl RuntimeClock for SharedClock {
    fn now(&self) -> Result<JobTimestamp, RuntimeClockError> {
        Ok(self.now_value())
    }
}

impl SourceRunClock for SharedClock {
    fn now(&self) -> Result<JobTimestamp, SourceRunClockError> {
        Ok(self.now_value())
    }
}

impl ReviewSourceFailureClassifier for FixtureReviews {
    fn failure_category(&self, _error: &Self::Error) -> SourceIngestionFailureCategory {
        SourceIngestionFailureCategory::OtherMandatoryStage
    }

    fn observation_category(&self, error: &Self::Error) -> WorkerFailureCategory {
        match error {
            FixtureError::MissingVideo => WorkerFailureCategory::MissingRequiredVideo,
            FixtureError::Transport => WorkerFailureCategory::SourceTransportOrContract,
        }
    }
}

struct TemporaryDatabase {
    path: PathBuf,
}

impl TemporaryDatabase {
    fn new() -> Self {
        let sequence = NEXT_DATABASE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "gamepulse-m054-handler-{}-{sequence}.sqlite3",
            std::process::id(),
        ));
        let _ = fs::remove_file(&path);
        Self { path }
    }
}

impl Drop for TemporaryDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(format!("{}-wal", self.path.display()));
        let _ = fs::remove_file(format!("{}-shm", self.path.display()));
    }
}

fn source_runtime(
    path: &std::path::Path,
    clock: SharedClock,
    calls: Arc<Mutex<Vec<u64>>>,
    first_error: FixtureError,
) -> Runtime<SqliteJobStore, SharedClock> {
    let queue = Arc::new(Mutex::new(
        SqliteJobStore::open(path).expect("queue must open"),
    ));
    let run_store = Arc::new(Mutex::new(
        SqliteRunProgressStore::open(path).expect("run store must open"),
    ));
    let source_schedule =
        SourceIngestionJobSchedule::new(1).expect("source schedule must be valid");
    let handlers = Arc::new(
        JobHandlerRegistry::new([
            Arc::new(DurableRunDiscoveryHandler::with_clock(
                run_store.clone(),
                FixtureDiscovery,
                source_schedule,
                clock.clone(),
            )) as Arc<dyn gamepulse_application::JobHandler>,
            Arc::new(DurableRunReviewSourceIngestionHandler::with_clock(
                run_store,
                FixtureReviews::new(calls, first_error),
                ReviewSummaryJobSchedule::new(1).expect("summary schedule must be valid"),
                source_schedule,
                clock.clone(),
            )) as Arc<dyn gamepulse_application::JobHandler>,
        ])
        .expect("source handlers must be unique"),
    );
    Runtime::new(
        queue,
        Arc::new(clock),
        RuntimeConfig::new(
            "m054-source",
            300,
            1,
            HourlyJobSchedule::new(RuntimeJobType::SourceHourlyDiscovery, 1)
                .expect("hourly schedule must be valid"),
        )
        .expect("runtime config must be valid")
        .with_claim_filter(RuntimeJobTypeFilter::source_lane()),
        handlers,
    )
}

fn exhausted_source_runtime(
    path: &std::path::Path,
    clock: SharedClock,
    calls: Arc<Mutex<usize>>,
) -> Runtime<SqliteJobStore, SharedClock> {
    let queue = Arc::new(Mutex::new(
        SqliteJobStore::open(path).expect("queue must open"),
    ));
    let run_store = Arc::new(Mutex::new(
        SqliteRunProgressStore::open(path).expect("run store must open"),
    ));
    let source_schedule =
        SourceIngestionJobSchedule::new(1).expect("source schedule must be valid");
    let handlers = Arc::new(
        JobHandlerRegistry::new([Arc::new(DurableRunDiscoveryHandler::with_clock(
            run_store,
            ExhaustedDiscovery { calls },
            source_schedule,
            clock.clone(),
        )) as Arc<dyn gamepulse_application::JobHandler>])
        .expect("source handler must be unique"),
    );
    Runtime::new(
        queue,
        Arc::new(clock),
        RuntimeConfig::new(
            "m054-exhausted-source",
            300,
            1,
            HourlyJobSchedule::new(RuntimeJobType::SourceHourlyDiscovery, 1)
                .expect("hourly schedule must be valid"),
        )
        .expect("runtime config must be valid")
        .with_claim_filter(RuntimeJobTypeFilter::source_lane()),
        handlers,
    )
}

async fn dispatch_one(runtime: &mut Runtime<SqliteJobStore, SharedClock>) -> FailureCategoryCounts {
    let dispatched = runtime
        .dispatch_available()
        .expect("source dispatch must succeed");
    assert_eq!(dispatched.claimed, 1);
    let settled = runtime.join_all().await.expect("source join must succeed");
    assert_eq!(settled.settled, [runtime::RuntimeTaskOutcome::Succeeded]);
    settled.observed_failures()
}

#[tokio::test]
async fn legacy_missing_video_rejection_can_still_advance_existing_runs() {
    let database = TemporaryDatabase::new();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let clock = SharedClock::new(1);
    let mut runtime = source_runtime(
        &database.path,
        clock.clone(),
        calls.clone(),
        FixtureError::MissingVideo,
    );
    assert_eq!(
        runtime.schedule_hourly().expect("hourly job must schedule"),
        runtime::SchedulerOutcome::Enqueued
    );
    let mut observations = dispatch_one(&mut runtime).await;
    observations.merge(dispatch_one(&mut runtime).await);
    let connection = Connection::open(&database.path).expect("inspection connection must open");
    assert_eq!(observations.missing_required_video(), 1);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM games", [], |row| row.get::<_, i64>(0))
            .expect("game count must load"),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT rejection_category FROM run_items WHERE source_product_id = '1'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("rejection category must load"),
        "missing_required_video"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT state FROM jobs WHERE job_identity = (
                    SELECT job_identity FROM run_items WHERE source_product_id = '1'
                 )",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("rejected item job state must load"),
        "succeeded"
    );
    drop(runtime);

    let mut restarted = source_runtime(
        &database.path,
        clock,
        calls.clone(),
        FixtureError::MissingVideo,
    );
    for _ in 0..20 {
        observations.merge(dispatch_one(&mut restarted).await);
    }
    assert_eq!(observations.missing_required_video(), 1);
    assert_eq!(
        calls
            .lock()
            .expect("fixture calls lock must be available")
            .iter()
            .filter(|&&id| id == 1)
            .count(),
        1,
        "the terminal rejection must not repeat after runtime restart"
    );

    assert_eq!(
        connection
            .query_row("SELECT state FROM runs", [], |row| row.get::<_, String>(0))
            .expect("run state must load"),
        "succeeded"
    );
    assert_eq!(
        connection
            .query_row("SELECT accepted_count FROM runs", [], |row| row
                .get::<_, i64>(0))
            .expect("accepted count must load"),
        20
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM games", [], |row| row.get::<_, i64>(0))
            .expect("game count must load"),
        20
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM run_items WHERE state = 'rejected'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("rejection count must load"),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM run_items WHERE state = 'scheduled'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("pending schedule count must load"),
        0
    );
}

#[tokio::test]
async fn terminal_source_failure_rejects_candidate_and_schedules_the_next_one() {
    let database = TemporaryDatabase::new();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let clock = SharedClock::new(1);
    let mut runtime = source_runtime(&database.path, clock, calls, FixtureError::Transport);
    assert_eq!(
        runtime.schedule_hourly().expect("hourly job must schedule"),
        runtime::SchedulerOutcome::Enqueued
    );
    let _ = dispatch_one(&mut runtime).await;
    let observations = dispatch_one(&mut runtime).await;
    assert_eq!(observations.source_transport_or_contract(), 1);

    let connection = Connection::open(&database.path).expect("inspection connection must open");
    assert_eq!(
        connection
            .query_row(
                "SELECT rejection_category FROM run_items WHERE source_product_id = '1'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("rejection category must load"),
        "source_unavailable"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM run_items WHERE state = 'scheduled' AND source_product_id <> '1'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("next scheduled candidate count must load"),
        1
    );
}

#[tokio::test]
async fn source_exhaustion_persists_failure_and_current_source_job_settles_without_retry() {
    let database = TemporaryDatabase::new();
    let calls = Arc::new(Mutex::new(0));
    let clock = SharedClock::new(1);
    let mut runtime = exhausted_source_runtime(&database.path, clock.clone(), calls.clone());
    assert_eq!(
        runtime.schedule_hourly().expect("hourly job must schedule"),
        runtime::SchedulerOutcome::Enqueued
    );
    let _ = dispatch_one(&mut runtime).await;
    let _ = dispatch_one(&mut runtime).await;
    drop(runtime);

    let connection = Connection::open(&database.path).expect("inspection connection must open");
    assert_eq!(
        connection
            .query_row("SELECT state FROM runs", [], |row| row.get::<_, String>(0))
            .expect("run state must load"),
        "failed_exhausted"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM jobs
                 WHERE job_type = 'source.hourly-discovery' AND state <> 'succeeded'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("nonterminal source job count must load"),
        0
    );
    let mut restarted = exhausted_source_runtime(&database.path, clock, calls.clone());
    assert_eq!(
        restarted
            .dispatch_available()
            .expect("restart dispatch must succeed")
            .claimed,
        0,
        "a terminal exhausted run must not re-enter the source"
    );
    assert_eq!(
        *calls
            .lock()
            .expect("fixture discovery calls lock must be available"),
        2
    );
}
