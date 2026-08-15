#![forbid(unsafe_code)]

#[allow(dead_code)]
#[path = "../src/runtime.rs"]
mod runtime;

use std::collections::VecDeque;
use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gamepulse_application::{
    GameSnapshot, GameSnapshotStore, HourlyJobSchedule, JobHandler, JobHandlerRegistry, JobRequest,
    JobStatus, JobStore, JobTimestamp, RuntimeJobType, SourceIngestionJobSchedule,
};
use gamepulse_storage_sqlite::{
    SqliteDailyCrawlStateStore, SqliteGameSnapshotStore, SqliteJobStore,
};
use gamepulse_worker_source::{
    GameIdentity, GameIngestionTransport, HourlyDiscoveryHandler, ListMode, ListingTransport,
    MetacriticDailyCrawlSource, MetacriticGameIngestionSource, PlatformDetail, PlatformUserScore,
    SourceIngestionHandler, parse_game_detail, parse_platform_user_score_for_snapshot,
};
use runtime::{Runtime, RuntimeClock, RuntimeClockError, RuntimeConfig, RuntimeTaskOutcome};
use serde_json::{Value, json};

const LISTING_FIXTURE: &str =
    include_str!("../../gamepulse-worker-source/tests/fixtures/listing-page.json");
const DETAIL_FIXTURE: &str =
    include_str!("../../gamepulse-worker-source/tests/fixtures/product-detail.json");
const USER_SCORE_FIXTURE: &str =
    include_str!("../../gamepulse-worker-source/tests/fixtures/user-score.json");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FixtureError {
    Unavailable,
    InvalidFixture,
}

#[derive(Default)]
struct ListingFixtureState {
    responses: VecDeque<Result<String, FixtureError>>,
    calls: Vec<(ListMode, u32, u32)>,
}

#[derive(Clone, Default)]
struct FixtureListingTransport {
    state: Arc<Mutex<ListingFixtureState>>,
}

impl FixtureListingTransport {
    fn with_responses(responses: impl IntoIterator<Item = Result<String, FixtureError>>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ListingFixtureState {
                responses: responses.into_iter().collect(),
                calls: Vec::new(),
            })),
        }
    }

    fn calls(&self) -> Vec<(ListMode, u32, u32)> {
        self.state
            .lock()
            .expect("fixture listing state must not be poisoned")
            .calls
            .clone()
    }
}

impl ListingTransport for FixtureListingTransport {
    type Error = FixtureError;
    type FetchFuture<'a>
        = Pin<Box<dyn Future<Output = Result<String, Self::Error>> + Send + 'a>>
    where
        Self: 'a;

    fn fetch_listing(&self, mode: ListMode, offset: u32, limit: u32) -> Self::FetchFuture<'_> {
        let response = {
            let mut state = self
                .state
                .lock()
                .expect("fixture listing state must not be poisoned");
            state.calls.push((mode, offset, limit));
            state
                .responses
                .pop_front()
                .expect("fixture listing transport needs one response")
        };
        Box::pin(async move { response })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IngestionFixtureMode {
    Valid,
    SourceFailure,
    MappingFailure,
}

#[derive(Default)]
struct IngestionFixtureState {
    calls: Vec<String>,
}

#[derive(Clone)]
struct FixtureGameIngestionTransport {
    mode: IngestionFixtureMode,
    state: Arc<Mutex<IngestionFixtureState>>,
}

impl FixtureGameIngestionTransport {
    fn new(mode: IngestionFixtureMode) -> Self {
        Self {
            mode,
            state: Arc::new(Mutex::new(IngestionFixtureState::default())),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("fixture ingestion state must not be poisoned")
            .calls
            .clone()
    }
}

impl GameIngestionTransport for FixtureGameIngestionTransport {
    type Error = FixtureError;
    type FetchDetailFuture<'a>
        = Pin<
        Box<
            dyn Future<Output = Result<gamepulse_worker_source::GameDetail, Self::Error>>
                + Send
                + 'a,
        >,
    >
    where
        Self: 'a;
    type FetchPlatformUserScoreFuture<'a>
        = Pin<Box<dyn Future<Output = Result<PlatformUserScore, Self::Error>> + Send + 'a>>
    where
        Self: 'a;

    fn fetch_game_detail(&self, expected: GameIdentity) -> Self::FetchDetailFuture<'_> {
        self.state
            .lock()
            .expect("fixture ingestion state must not be poisoned")
            .calls
            .push(format!("detail:{}:{}", expected.id.0, expected.slug));
        let result = match self.mode {
            IngestionFixtureMode::SourceFailure => Err(FixtureError::Unavailable),
            IngestionFixtureMode::Valid => parse_game_detail(&expected, DETAIL_FIXTURE)
                .map_err(|_| FixtureError::InvalidFixture),
            IngestionFixtureMode::MappingFailure => parse_game_detail(&expected, DETAIL_FIXTURE)
                .map_err(|_| FixtureError::InvalidFixture)
                .map(|mut detail| {
                    detail.platforms.push(detail.platforms[0].clone());
                    detail
                }),
        };
        Box::pin(async move { result })
    }

    fn fetch_platform_user_score(
        &self,
        expected_game: GameIdentity,
        expected_platform: PlatformDetail,
    ) -> Self::FetchPlatformUserScoreFuture<'_> {
        self.state
            .lock()
            .expect("fixture ingestion state must not be poisoned")
            .calls
            .push(format!(
                "userscore:{}:{}",
                expected_game.id.0, expected_platform.slug
            ));
        let body = match expected_platform.slug.as_str() {
            "pc" => USER_SCORE_FIXTURE.to_owned(),
            "console" => USER_SCORE_FIXTURE
                .replace("/platform/pc/", "/platform/console/")
                .replace("\"score\": 8.4", "\"score\": null"),
            _ => return Box::pin(async { Err(FixtureError::InvalidFixture) }),
        };
        let result =
            parse_platform_user_score_for_snapshot(&expected_game, &expected_platform, &body)
                .map_err(|_| FixtureError::InvalidFixture);
        Box::pin(async move { result })
    }
}

#[derive(Default)]
struct RecordingSnapshotStore {
    snapshots: Vec<GameSnapshot>,
    fail_writes: bool,
}

impl GameSnapshotStore for RecordingSnapshotStore {
    type Error = FixtureError;

    fn upsert_snapshot(&mut self, snapshot: &GameSnapshot) -> Result<(), Self::Error> {
        if self.fail_writes {
            return Err(FixtureError::Unavailable);
        }
        self.snapshots.push(snapshot.clone());
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct FixedClock(i64);

impl RuntimeClock for FixedClock {
    fn now(&self) -> Result<JobTimestamp, RuntimeClockError> {
        JobTimestamp::new(self.0).map_err(|_| RuntimeClockError::Overflow)
    }
}

struct TemporaryDatabase {
    path: PathBuf,
}

impl TemporaryDatabase {
    fn new() -> Self {
        static NEXT_DATABASE: AtomicUsize = AtomicUsize::new(0);
        let sequence = NEXT_DATABASE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "gamepulse-m009-source-ingestion-{}-{sequence}.sqlite",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        Self { path }
    }
}

impl Drop for TemporaryDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(self.path.with_extension("sqlite-shm"));
        let _ = fs::remove_file(self.path.with_extension("sqlite-wal"));
    }
}

fn timestamp(value: i64) -> JobTimestamp {
    JobTimestamp::new(value).expect("test timestamp must be valid")
}

fn source_ingestion_schedule() -> SourceIngestionJobSchedule {
    SourceIngestionJobSchedule::new(1).expect("source ingestion schedule must be valid")
}

fn runtime_config() -> RuntimeConfig {
    RuntimeConfig::new(
        "m009-fixture-worker",
        30,
        1,
        HourlyJobSchedule::new(RuntimeJobType::SourceHourlyDiscovery, 1)
            .expect("hourly schedule must be valid"),
    )
    .expect("runtime configuration must be valid")
}

fn single_candidate_listing() -> String {
    let mut listing: Value = serde_json::from_str(LISTING_FIXTURE).expect("listing fixture JSON");
    listing["data"]["items"] = Value::Array(vec![listing["data"]["items"][0].clone()]);
    listing["data"]["totalResults"] = json!(1);
    listing["links"]["next"] = Value::Null;
    serde_json::to_string(&listing).expect("listing fixture must encode")
}

async fn settle_ingestion_job(
    handler: Arc<dyn JobHandler>,
    work_ref: &str,
) -> (JobStatus, Vec<gamepulse_application::JobAttempt>) {
    let queue = Arc::new(Mutex::new(
        SqliteJobStore::open_in_memory().expect("queue must open"),
    ));
    queue
        .lock()
        .expect("queue must not be poisoned")
        .enqueue(
            JobRequest::new(
                "m009-failure-case",
                RuntimeJobType::SourceGameIngestion.as_str(),
                work_ref,
                1,
                timestamp(0),
            )
            .expect("job request must be valid"),
        )
        .expect("job must enqueue");
    let handlers = Arc::new(JobHandlerRegistry::new([handler]).expect("registry must be valid"));
    let mut runtime = Runtime::new(
        queue.clone(),
        Arc::new(FixedClock(0)),
        runtime_config(),
        handlers,
    );

    runtime.dispatch_available().expect("job must dispatch");
    assert_eq!(
        runtime.join_all().await.expect("job must settle").settled,
        [RuntimeTaskOutcome::Failed(
            gamepulse_application::JobFailureResult::Failed
        )]
    );
    drop(runtime);

    let mut queue = queue.lock().expect("queue must not be poisoned");
    let record = queue
        .job("m009-failure-case")
        .expect("job lookup must succeed")
        .expect("job must exist");
    let attempts = queue
        .attempts("m009-failure-case")
        .expect("attempt history must load");
    (record.status(), attempts)
}

#[tokio::test]
async fn fixture_discovery_enqueues_then_ingests_one_game_and_survives_sqlite_reopen() {
    let database = TemporaryDatabase::new();
    let queue = Arc::new(Mutex::new(
        SqliteJobStore::open(&database.path).expect("queue database must open"),
    ));
    let daily_state = Arc::new(Mutex::new(
        SqliteDailyCrawlStateStore::open(&database.path).expect("daily state database must open"),
    ));
    let snapshots = Arc::new(Mutex::new(
        SqliteGameSnapshotStore::open(&database.path).expect("snapshot database must open"),
    ));
    let listing_transport =
        FixtureListingTransport::with_responses([Ok(single_candidate_listing())]);
    let ingestion_transport = FixtureGameIngestionTransport::new(IngestionFixtureMode::Valid);
    let discovery_handler: Arc<dyn JobHandler> = Arc::new(HourlyDiscoveryHandler::new(
        daily_state.clone(),
        MetacriticDailyCrawlSource::new(listing_transport.clone()),
        source_ingestion_schedule(),
    ));
    let ingestion_handler: Arc<dyn JobHandler> = Arc::new(SourceIngestionHandler::new(
        snapshots.clone(),
        MetacriticGameIngestionSource::new(ingestion_transport.clone()),
    ));
    let handlers = Arc::new(
        JobHandlerRegistry::new([discovery_handler, ingestion_handler])
            .expect("registry must be valid"),
    );
    let schedule = HourlyJobSchedule::new(RuntimeJobType::SourceHourlyDiscovery, 1)
        .expect("hourly schedule must be valid");
    let config = RuntimeConfig::new("m009-fixture-worker", 30, 1, schedule)
        .expect("runtime configuration must be valid");
    let mut runtime = Runtime::new(queue.clone(), Arc::new(FixedClock(0)), config, handlers);

    runtime.schedule_hourly().expect("hourly job must enqueue");
    runtime
        .dispatch_available()
        .expect("hourly job must dispatch");
    assert_eq!(
        runtime
            .join_all()
            .await
            .expect("hourly job must settle")
            .settled,
        [RuntimeTaskOutcome::Succeeded]
    );
    runtime
        .dispatch_available()
        .expect("derived ingestion job must dispatch");
    assert_eq!(
        runtime
            .join_all()
            .await
            .expect("ingestion job must settle")
            .settled,
        [RuntimeTaskOutcome::Succeeded]
    );
    drop(runtime);
    drop(queue);
    drop(daily_state);
    drop(snapshots);

    assert_eq!(listing_transport.calls(), [(ListMode::NewReleases, 0, 20)]);
    assert_eq!(
        ingestion_transport.calls(),
        [
            "detail:101:example-game",
            "userscore:101:pc",
            "userscore:101:console",
        ]
    );

    let reopened = rusqlite::Connection::open(&database.path).expect("database must reopen");
    let game = reopened
        .query_row(
            "SELECT source_slug, title FROM games WHERE source_product_id = 101",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("snapshot game must survive reopen");
    let platform_count = reopened
        .query_row(
            "SELECT COUNT(*) FROM game_platform_scores WHERE game_source_product_id = 101",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("platform scores must survive reopen");
    let console_userscore = reopened
        .query_row(
            "SELECT userscore FROM game_platform_scores
             WHERE game_source_product_id = 101 AND source_platform_id = 8",
            [],
            |row| row.get::<_, Option<f64>>(0),
        )
        .expect("explicit missing Userscore must survive reopen");
    assert_eq!(game, ("example-game".to_owned(), "Example Game".to_owned()));
    assert_eq!(platform_count, 2);
    assert_eq!(console_userscore, None);

    let mut reopened_queue = SqliteJobStore::open(&database.path).expect("queue must reopen");
    assert_eq!(
        reopened_queue
            .job("source.game-ingestion:1970-01-01:101")
            .expect("ingestion job must load")
            .expect("ingestion job must exist")
            .status(),
        JobStatus::Succeeded
    );
}

#[tokio::test]
async fn malformed_source_mapping_and_store_failures_settle_without_a_snapshot() {
    let malformed_transport = FixtureGameIngestionTransport::new(IngestionFixtureMode::Valid);
    let malformed_store = Arc::new(Mutex::new(RecordingSnapshotStore::default()));
    let malformed_handler: Arc<dyn JobHandler> = Arc::new(SourceIngestionHandler::new(
        malformed_store.clone(),
        MetacriticGameIngestionSource::new(malformed_transport.clone()),
    ));
    let (malformed_status, malformed_attempts) =
        settle_ingestion_job(malformed_handler, "metacritic-game:001:example-game").await;
    assert_eq!(malformed_status, JobStatus::Failed);
    assert_eq!(malformed_attempts.len(), 1);
    assert!(malformed_transport.calls().is_empty());
    assert!(
        malformed_store
            .lock()
            .expect("snapshot store must not be poisoned")
            .snapshots
            .is_empty()
    );

    let source_transport = FixtureGameIngestionTransport::new(IngestionFixtureMode::SourceFailure);
    let source_store = Arc::new(Mutex::new(RecordingSnapshotStore::default()));
    let source_handler: Arc<dyn JobHandler> = Arc::new(SourceIngestionHandler::new(
        source_store.clone(),
        MetacriticGameIngestionSource::new(source_transport.clone()),
    ));
    let (source_status, source_attempts) =
        settle_ingestion_job(source_handler, "metacritic-game:101:example-game").await;
    assert_eq!(source_status, JobStatus::Failed);
    assert_eq!(source_attempts.len(), 1);
    assert_eq!(source_transport.calls(), ["detail:101:example-game"]);
    assert!(
        source_store
            .lock()
            .expect("snapshot store must not be poisoned")
            .snapshots
            .is_empty()
    );

    let mapping_transport =
        FixtureGameIngestionTransport::new(IngestionFixtureMode::MappingFailure);
    let mapping_store = Arc::new(Mutex::new(RecordingSnapshotStore::default()));
    let mapping_handler: Arc<dyn JobHandler> = Arc::new(SourceIngestionHandler::new(
        mapping_store.clone(),
        MetacriticGameIngestionSource::new(mapping_transport.clone()),
    ));
    let (mapping_status, mapping_attempts) =
        settle_ingestion_job(mapping_handler, "metacritic-game:101:example-game").await;
    assert_eq!(mapping_status, JobStatus::Failed);
    assert_eq!(mapping_attempts.len(), 1);
    assert_eq!(
        mapping_transport.calls(),
        [
            "detail:101:example-game",
            "userscore:101:pc",
            "userscore:101:console",
            "userscore:101:pc",
        ]
    );
    assert!(
        mapping_store
            .lock()
            .expect("snapshot store must not be poisoned")
            .snapshots
            .is_empty()
    );

    let store_transport = FixtureGameIngestionTransport::new(IngestionFixtureMode::Valid);
    let store = Arc::new(Mutex::new(RecordingSnapshotStore {
        snapshots: Vec::new(),
        fail_writes: true,
    }));
    let store_handler: Arc<dyn JobHandler> = Arc::new(SourceIngestionHandler::new(
        store.clone(),
        MetacriticGameIngestionSource::new(store_transport.clone()),
    ));
    let (store_status, store_attempts) =
        settle_ingestion_job(store_handler, "metacritic-game:101:example-game").await;
    assert_eq!(store_status, JobStatus::Failed);
    assert_eq!(store_attempts.len(), 1);
    assert_eq!(store_transport.calls().len(), 3);
    assert!(
        store
            .lock()
            .expect("snapshot store must not be poisoned")
            .snapshots
            .is_empty()
    );
}
