#![forbid(unsafe_code)]

#[allow(dead_code)]
#[path = "../src/runtime.rs"]
mod runtime;

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gamepulse_application::{
    AsyncDailyCrawlSourcePort, BrowseCursor, BrowseProgress, CrawlDayKey, CrawlDiscoveryRequest,
    DailyCrawlCommit, DailyCrawlState, DailyCrawlStatePort, DiscoveryCandidate, DiscoveryPage,
    HourlyJobSchedule, JobHandler, JobHandlerRegistry, JobHandlerResult, JobRecord, JobStatus,
    JobStore, JobTimestamp, RuntimeJobType, SourceIngestionJobSchedule, TypedJob,
};
use gamepulse_storage_sqlite::{SqliteDailyCrawlStateStore, SqliteJobStore};
use gamepulse_worker_source::{
    HourlyDiscoveryHandler, ListMode, ListingTransport, MetacriticDailyCrawlSource,
};
use runtime::{Runtime, RuntimeClock, RuntimeClockError, RuntimeConfig, RuntimeTaskOutcome};

const LISTING_FIXTURE: &str =
    include_str!("../../gamepulse-worker-source/tests/fixtures/listing-page.json");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FixtureTransportError {
    Unavailable,
}

#[derive(Default)]
struct FixtureTransportState {
    responses: VecDeque<Result<String, FixtureTransportError>>,
    calls: Vec<(ListMode, u32, u32)>,
}

#[derive(Clone, Default)]
struct FixtureListingTransport {
    state: Arc<Mutex<FixtureTransportState>>,
}

impl FixtureListingTransport {
    fn with_responses(
        responses: impl IntoIterator<Item = Result<String, FixtureTransportError>>,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(FixtureTransportState {
                responses: responses.into_iter().collect(),
                calls: Vec::new(),
            })),
        }
    }

    fn calls(&self) -> Vec<(ListMode, u32, u32)> {
        self.state
            .lock()
            .expect("fixture transport state must not be poisoned")
            .calls
            .clone()
    }
}

impl ListingTransport for FixtureListingTransport {
    type Error = FixtureTransportError;
    type FetchFuture<'a>
        = Pin<Box<dyn Future<Output = Result<String, Self::Error>> + Send + 'a>>
    where
        Self: 'a;

    fn fetch_listing(&self, mode: ListMode, offset: u32, limit: u32) -> Self::FetchFuture<'_> {
        let response = {
            let mut state = self
                .state
                .lock()
                .expect("fixture transport state must not be poisoned");
            state.calls.push((mode, offset, limit));
            state
                .responses
                .pop_front()
                .expect("fixture transport needs one response per request")
        };
        Box::pin(async move { response })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MemoryStateError {
    Commit,
}

#[derive(Default)]
struct MemoryDailyCrawlState {
    states: BTreeMap<CrawlDayKey, DailyCrawlState>,
    commits: Vec<DailyCrawlCommit>,
    fail_commit: bool,
}

impl DailyCrawlStatePort for MemoryDailyCrawlState {
    type Error = MemoryStateError;

    fn load(&mut self, day: &CrawlDayKey) -> Result<Option<DailyCrawlState>, MemoryStateError> {
        Ok(self.states.get(day).cloned())
    }

    fn commit(&mut self, commit: DailyCrawlCommit) -> Result<(), MemoryStateError> {
        if self.fail_commit {
            return Err(MemoryStateError::Commit);
        }
        self.states
            .insert(commit.state().day().clone(), commit.state().clone());
        self.commits.push(commit);
        Ok(())
    }
}

#[derive(Default)]
struct DirectDailySourceState {
    responses: VecDeque<Result<DiscoveryPage, FixtureTransportError>>,
    calls: Vec<CrawlDiscoveryRequest>,
}

#[derive(Clone, Default)]
struct DirectDailySource {
    state: Arc<Mutex<DirectDailySourceState>>,
}

impl DirectDailySource {
    fn with_responses(
        responses: impl IntoIterator<Item = Result<DiscoveryPage, FixtureTransportError>>,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(DirectDailySourceState {
                responses: responses.into_iter().collect(),
                calls: Vec::new(),
            })),
        }
    }

    fn calls(&self) -> Vec<CrawlDiscoveryRequest> {
        self.state
            .lock()
            .expect("direct source state must not be poisoned")
            .calls
            .clone()
    }
}

impl AsyncDailyCrawlSourcePort for DirectDailySource {
    type Error = FixtureTransportError;
    type DiscoverFuture<'a>
        = Pin<Box<dyn Future<Output = Result<DiscoveryPage, Self::Error>> + Send + 'a>>
    where
        Self: 'a;

    fn discover(&self, request: CrawlDiscoveryRequest) -> Self::DiscoverFuture<'_> {
        let response = {
            let mut state = self
                .state
                .lock()
                .expect("direct source state must not be poisoned");
            state.calls.push(request);
            state
                .responses
                .pop_front()
                .expect("direct source needs one response per request")
        };
        Box::pin(async move { response })
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
        let suffix = NEXT_DATABASE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "gamepulse-m007-source-handler-{}-{suffix}.sqlite",
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

fn typed_hourly_job(work_reference: &str) -> TypedJob {
    let timestamp = JobTimestamp::new(0).expect("test timestamp must be valid");
    TypedJob::from_record(&JobRecord::restored(
        "m007-test-job".to_owned(),
        RuntimeJobType::SourceHourlyDiscovery.as_str().to_owned(),
        work_reference.to_owned(),
        1,
        0,
        JobStatus::Ready,
        timestamp,
        timestamp,
        None,
        None,
        None,
        None,
    ))
    .expect("test record must be a typed source job")
}

fn fixture_browse_page() -> String {
    LISTING_FIXTURE
        .replace("\"totalResults\": 42", "\"totalResults\": 72")
        .replace("offset=20&limit=20", "offset=24&limit=24")
}

fn fixture_exhausted_browse_page() -> String {
    fixture_browse_page().replace("\"next\": {", "\"later\": {")
}

fn source_ingestion_schedule() -> SourceIngestionJobSchedule {
    SourceIngestionJobSchedule::new(1).expect("source ingestion schedule must be valid")
}

fn direct_page(ids: std::ops::RangeInclusive<u64>, next: Option<u64>) -> DiscoveryPage {
    DiscoveryPage::new(
        ids.map(|id| {
            DiscoveryCandidate::new(id, format!("game-{id}"))
                .expect("direct fixture candidate must be valid")
        })
        .collect(),
        next.map(BrowseCursor::new),
    )
}

fn result_is_failure(result: JobHandlerResult) -> bool {
    matches!(result, JobHandlerResult::Failed(_))
}

#[tokio::test]
async fn accepted_hourly_slot_commits_fixture_selection_settles_job_and_survives_reopen() {
    let database = TemporaryDatabase::new();
    let queue = Arc::new(Mutex::new(
        SqliteJobStore::open(&database.path).expect("queue database must open"),
    ));
    let daily_state = Arc::new(Mutex::new(
        SqliteDailyCrawlStateStore::open(&database.path).expect("daily state database must open"),
    ));
    let transport = FixtureListingTransport::with_responses([Ok(LISTING_FIXTURE.to_owned())]);
    let source = MetacriticDailyCrawlSource::new(transport.clone());
    let handler: Arc<dyn JobHandler> = Arc::new(HourlyDiscoveryHandler::new(
        daily_state.clone(),
        source,
        source_ingestion_schedule(),
    ));
    let schedule = HourlyJobSchedule::new(RuntimeJobType::SourceHourlyDiscovery, 1)
        .expect("schedule must be valid");
    let config = RuntimeConfig::new("m007-test-worker", 30, 1, schedule)
        .expect("runtime configuration must be valid");
    let handlers = Arc::new(JobHandlerRegistry::new([handler]).expect("registry must be valid"));
    let mut runtime = Runtime::new(queue.clone(), Arc::new(FixedClock(0)), config, handlers);

    runtime.schedule_hourly().expect("hourly job must enqueue");
    runtime.dispatch_available().expect("job must dispatch");
    assert_eq!(
        runtime.join_all().await.expect("job must settle").settled,
        [RuntimeTaskOutcome::Succeeded]
    );
    drop(runtime);

    let mut queue_guard = queue.lock().expect("queue must not be poisoned");
    assert_eq!(
        queue_guard
            .job("hourly:source.hourly-discovery:0")
            .expect("job lookup must succeed")
            .expect("scheduled job must exist")
            .status(),
        JobStatus::Succeeded
    );
    drop(queue_guard);
    drop(queue);
    drop(daily_state);

    let day = CrawlDayKey::new("1970-01-01").expect("UTC day must be valid");
    let mut reopened = SqliteDailyCrawlStateStore::open(&database.path)
        .expect("daily state must reopen after accepted commit");
    let state = reopened
        .load(&day)
        .expect("reopened state must load")
        .expect("accepted day must exist");
    assert_eq!(
        state
            .selected_or_processed()
            .iter()
            .map(|identity| identity.value())
            .collect::<Vec<_>>(),
        [101, 102]
    );
    assert!(state.new_releases_completed());
    assert_eq!(state.browse_progress(), BrowseProgress::Initial);
    assert_eq!(transport.calls(), [(ListMode::NewReleases, 0, 20)]);
}

#[tokio::test]
async fn same_utc_day_browses_and_a_new_utc_day_restarts_new_releases() {
    let state = Arc::new(Mutex::new(MemoryDailyCrawlState::default()));
    let transport = FixtureListingTransport::with_responses([
        Ok(LISTING_FIXTURE.to_owned()),
        Ok(fixture_exhausted_browse_page()),
        Ok(LISTING_FIXTURE.to_owned()),
    ]);
    let handler = HourlyDiscoveryHandler::new(
        state.clone(),
        MetacriticDailyCrawlSource::new(transport.clone()),
        source_ingestion_schedule(),
    );

    assert!(matches!(
        handler.handle(typed_hourly_job("hour-slot:0")).await,
        JobHandlerResult::Succeeded
    ));
    assert!(matches!(
        handler.handle(typed_hourly_job("hour-slot:1")).await,
        JobHandlerResult::Succeeded
    ));
    assert!(matches!(
        handler.handle(typed_hourly_job("hour-slot:24")).await,
        JobHandlerResult::Succeeded
    ));

    assert_eq!(
        transport.calls(),
        [
            (ListMode::NewReleases, 0, 20),
            (ListMode::NewestBrowse, 0, 24),
            (ListMode::NewReleases, 0, 20),
        ]
    );
    let state = state.lock().expect("memory state must not be poisoned");
    assert_eq!(state.states.len(), 2);
}

#[tokio::test]
async fn replayed_twenty_four_item_browse_page_continues_to_one_atomic_twenty_item_hourly_commit() {
    let day = CrawlDayKey::new("1970-01-01").expect("test day must be valid");
    let processed = (1..=20)
        .map(|id| gamepulse_application::SourceProductId::new(id).expect("test ID must be valid"));
    let state = Arc::new(Mutex::new(MemoryDailyCrawlState {
        states: BTreeMap::from([(
            day.clone(),
            DailyCrawlState::restored(day.clone(), processed, true, BrowseProgress::Initial),
        )]),
        ..Default::default()
    }));
    let source = DirectDailySource::with_responses([
        Ok(direct_page(1..=24, Some(24))),
        Ok(direct_page(25..=40, Some(48))),
    ]);
    let handler =
        HourlyDiscoveryHandler::new(state.clone(), source.clone(), source_ingestion_schedule());

    assert!(matches!(
        handler.handle(typed_hourly_job("hour-slot:0")).await,
        JobHandlerResult::Succeeded
    ));

    assert_eq!(
        source.calls(),
        [
            CrawlDiscoveryRequest::NewestBrowse { cursor: None },
            CrawlDiscoveryRequest::NewestBrowse {
                cursor: Some(BrowseCursor::new(24)),
            },
        ]
    );
    let state = state.lock().expect("memory state must not be poisoned");
    assert_eq!(state.commits.len(), 1);
    assert_eq!(state.commits[0].selected().len(), 20);
    assert_eq!(state.commits[0].jobs().len(), 20);
    assert_eq!(
        state.commits[0]
            .selected()
            .iter()
            .map(|candidate| candidate.source_product_id().value())
            .collect::<Vec<_>>(),
        (21..=40).collect::<Vec<_>>()
    );
    assert_eq!(
        state
            .states
            .get(&day)
            .expect("updated state must persist")
            .browse_progress(),
        BrowseProgress::Continue(BrowseCursor::new(48))
    );
}

#[tokio::test]
async fn malformed_or_overflowing_work_references_fail_without_source_or_state_changes() {
    let state = Arc::new(Mutex::new(MemoryDailyCrawlState::default()));
    let transport = FixtureListingTransport::default();
    let handler = HourlyDiscoveryHandler::new(
        state.clone(),
        MetacriticDailyCrawlSource::new(transport.clone()),
        source_ingestion_schedule(),
    );

    assert!(result_is_failure(
        handler
            .handle(typed_hourly_job("hour-slot:not-a-number"))
            .await
    ));
    assert!(result_is_failure(
        handler
            .handle(typed_hourly_job("hour-slot:2562047788015216"))
            .await
    ));
    assert!(transport.calls().is_empty());
    assert!(
        state
            .lock()
            .expect("memory state must not be poisoned")
            .states
            .is_empty()
    );
}

#[tokio::test]
async fn source_or_commit_failure_returns_handler_failure_without_publishing_state() {
    let source_failure_state = Arc::new(Mutex::new(MemoryDailyCrawlState::default()));
    let source_failure_transport =
        FixtureListingTransport::with_responses([Err(FixtureTransportError::Unavailable)]);
    let source_failure_handler = HourlyDiscoveryHandler::new(
        source_failure_state.clone(),
        MetacriticDailyCrawlSource::new(source_failure_transport.clone()),
        source_ingestion_schedule(),
    );
    assert!(result_is_failure(
        source_failure_handler
            .handle(typed_hourly_job("hour-slot:0"))
            .await
    ));
    assert_eq!(
        source_failure_transport.calls(),
        [(ListMode::NewReleases, 0, 20)]
    );
    assert!(
        source_failure_state
            .lock()
            .expect("memory state must not be poisoned")
            .states
            .is_empty()
    );

    let commit_failure_state = Arc::new(Mutex::new(MemoryDailyCrawlState {
        fail_commit: true,
        ..Default::default()
    }));
    let commit_failure_transport =
        FixtureListingTransport::with_responses([Ok(LISTING_FIXTURE.to_owned())]);
    let commit_failure_handler = HourlyDiscoveryHandler::new(
        commit_failure_state.clone(),
        MetacriticDailyCrawlSource::new(commit_failure_transport.clone()),
        source_ingestion_schedule(),
    );
    assert!(result_is_failure(
        commit_failure_handler
            .handle(typed_hourly_job("hour-slot:0"))
            .await
    ));
    assert_eq!(
        commit_failure_transport.calls(),
        [(ListMode::NewReleases, 0, 20)]
    );
    assert!(
        commit_failure_state
            .lock()
            .expect("memory state must not be poisoned")
            .states
            .is_empty()
    );
}
