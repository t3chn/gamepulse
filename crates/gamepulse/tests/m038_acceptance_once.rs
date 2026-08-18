#![forbid(unsafe_code)]

#[path = "../src/acceptance.rs"]
mod acceptance;
#[allow(dead_code)]
#[path = "../src/runtime.rs"]
mod runtime;

use std::fs;
use std::future::{Future, pending};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use acceptance::{
    ACCEPTANCE_HELP, AcceptanceCommand, AcceptanceTerminal, database_path_is_fresh,
    parse_entry_command, run_acceptance_once,
};
use gamepulse_application::{
    AcceptanceCycleReadPort, AcceptanceCycleSnapshot, AsyncDailyCrawlSourcePort,
    AsyncReviewSourceIngestionPort, CrawlDiscoveryRequest, DiscoveryCandidate, DiscoveryPage,
    FailureCategoryCounts, GameSnapshot, GameVideoLink, HourlyJobSchedule, JobClaimPacing,
    JobHandler, JobHandlerRegistry, JobTimestamp, ReviewExcerpt, ReviewInput, ReviewKind,
    ReviewSourceIngestion, ReviewSummaryJobSchedule, RuntimeJobType, RuntimeJobTypeFilter,
    SourceIngestionRequest, SourceProductId, WorkerFailureCategory,
};
use gamepulse_storage_sqlite::{
    AcceptanceCycleReadStoreError, SqliteAcceptanceCycleStore, SqliteDailyCrawlStateStore,
    SqliteJobStore, SqliteReviewSummaryStore,
};
use gamepulse_worker_llm::{LocalExtractiveReviewSummarizer, ReviewSummaryHandler};
use gamepulse_worker_source::{
    HourlyDiscoveryHandler, ReviewSourceFailureClassifier, ReviewSourceIngestionHandler,
    SourceIngestionFailureCategory,
};
use runtime::{Runtime, RuntimeClock, RuntimeClockError, RuntimeConfig, SystemRuntimeClock};

static NEXT_DATABASE: AtomicUsize = AtomicUsize::new(0);

const DOCUMENTED_HELP_COMMAND: &str =
    "cargo run --locked --offline -p gamepulse -- acceptance-once --help";
const DOCUMENTED_RUN_TEMPLATE_BODY: &str = concat!(
    "  case \"$acceptance_dir\" in\n",
    "    /tmp/gamepulse-acceptance.*) ;;\n",
    "    *) printf '%s\\n' 'acceptance temporary directory is invalid' >&2; exit 2 ;;\n",
    "  esac\n",
    "  database_path=\"$acceptance_dir/gamepulse.sqlite3\"\n",
    "  cargo run --locked --offline -p gamepulse -- acceptance-once \\\n",
    "    --database \"$database_path\" \\\n",
    "    --target 20 \\\n",
    "    --deadline-seconds 180\n",
    "  command_status=$?\n",
    "  rm -rf -- \"$acceptance_dir\"\n",
    "  exit \"$command_status\"\n",
    ")\n",
);
const M041_CARGO_WRAPPER: &str = r#"#!/bin/sh
printf '%s\n' "$@" > "$GAMEPULSE_M041_ARGUMENT_RECORD"
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--" ]; then
    shift
    break
  fi
  shift
done
database_path=""
database_value_next=false
for argument in "$@"; do
  if [ "$database_value_next" = true ]; then
    database_path="$argument"
    break
  fi
  if [ "$argument" = "--database" ]; then
    database_value_next=true
  fi
done
"$GAMEPULSE_M041_BINARY" "$@"
status=$?
if [ ! -e "$database_path" ] && [ -f "$database_path-wal" ] && [ "$(cat "$database_path-wal")" = "caller-owned-sidecar" ]; then
  printf '%s\n' 'database_not_opened_before_runtime_composition' > "$GAMEPULSE_M041_COMPOSITION_RECORD"
else
  printf '%s\n' 'database_or_sidecar_changed' > "$GAMEPULSE_M041_COMPOSITION_RECORD"
fi
exit "$status"
"#;

struct TemporaryDatabase {
    path: PathBuf,
}

impl TemporaryDatabase {
    fn new(name: &str) -> Self {
        let sequence = NEXT_DATABASE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "gamepulse-m038-acceptance-{name}-{}-{sequence}.sqlite3",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("sqlite3-shm"));
        let _ = fs::remove_file(path.with_extension("sqlite3-wal"));
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

struct TemporaryInvocationHarness {
    root: PathBuf,
    acceptance_dir: PathBuf,
}

impl TemporaryInvocationHarness {
    fn new() -> Self {
        let sequence = NEXT_DATABASE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "gamepulse-m041-invocation-{}-{sequence}",
            std::process::id()
        ));
        let acceptance_dir = PathBuf::from("/tmp").join(format!(
            "gamepulse-acceptance.m041 safe path-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("M041 harness directory must be created");
        fs::create_dir(&acceptance_dir).expect("M041 acceptance directory must be created");
        Self {
            root,
            acceptance_dir,
        }
    }

    fn database_path(&self) -> PathBuf {
        self.acceptance_dir.join("gamepulse.sqlite3")
    }

    fn argument_record(&self) -> PathBuf {
        self.root.join("cargo-arguments")
    }

    fn composition_record(&self) -> PathBuf {
        self.root.join("composition-guard")
    }

    fn write_cargo_wrapper(&self) {
        let bin = self.root.join("bin");
        fs::create_dir(&bin).expect("M041 cargo wrapper directory must be created");
        let wrapper = bin.join("cargo");
        fs::write(&wrapper, M041_CARGO_WRAPPER).expect("M041 cargo wrapper must be written");
        let mut permissions = fs::metadata(&wrapper)
            .expect("M041 cargo wrapper metadata must be readable")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(wrapper, permissions).expect("M041 cargo wrapper must be executable");
    }

    fn shell(&self, script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(script)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env("GAMEPULSE_M041_ACCEPTANCE_DIR", &self.acceptance_dir)
            .env("GAMEPULSE_M041_ARGUMENT_RECORD", self.argument_record())
            .env(
                "GAMEPULSE_M041_COMPOSITION_RECORD",
                self.composition_record(),
            )
            .env("GAMEPULSE_M041_BINARY", env!("CARGO_BIN_EXE_gamepulse"));
        command
    }
}

impl Drop for TemporaryInvocationHarness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::remove_dir_all(&self.acceptance_dir);
    }
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
    Failed,
}

#[derive(Clone)]
struct FixtureDailySource {
    calls: Arc<AtomicUsize>,
    pending: bool,
}

impl FixtureDailySource {
    fn complete() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            pending: false,
        }
    }

    fn pending() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            pending: true,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AsyncDailyCrawlSourcePort for FixtureDailySource {
    type Error = FixtureError;
    type DiscoverFuture<'a>
        = Pin<Box<dyn Future<Output = Result<DiscoveryPage, Self::Error>> + Send + 'a>>
    where
        Self: 'a;

    fn discover(&self, request: CrawlDiscoveryRequest) -> Self::DiscoverFuture<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.pending {
            return Box::pin(async { pending::<Result<DiscoveryPage, FixtureError>>().await });
        }
        Box::pin(async move {
            if request != CrawlDiscoveryRequest::NewReleases {
                return Err(FixtureError::Failed);
            }
            Ok(DiscoveryPage::new(
                (1..=20)
                    .map(|product_id| {
                        DiscoveryCandidate::new(product_id, format!("fixture-{product_id}"))
                            .expect("fixture candidate must be valid")
                    })
                    .collect(),
                None,
            ))
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FixtureReviewMode {
    AlwaysValid,
    FirstCallFails,
}

#[derive(Clone)]
struct FixtureReviewSource {
    mode: FixtureReviewMode,
    calls: Arc<AtomicUsize>,
}

impl FixtureReviewSource {
    fn new(mode: FixtureReviewMode) -> Self {
        Self {
            mode,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AsyncReviewSourceIngestionPort for FixtureReviewSource {
    type Error = FixtureError;
    type IngestFuture<'a>
        = Pin<Box<dyn Future<Output = Result<ReviewSourceIngestion, Self::Error>> + Send + 'a>>
    where
        Self: 'a;

    fn ingest_reviews(&self, request: SourceIngestionRequest) -> Self::IngestFuture<'_> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let should_fail = self.mode == FixtureReviewMode::FirstCallFails && call == 0;
        Box::pin(async move {
            if should_fail {
                return Err(FixtureError::Failed);
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
            let critic = review_input(source_product_id, ReviewKind::Critic, "Critics liked this.");
            let user = review_input(source_product_id, ReviewKind::User, "Users liked this.");
            ReviewSourceIngestion::new(snapshot, critic, user).map_err(|_| FixtureError::Failed)
        })
    }
}

impl ReviewSourceFailureClassifier for FixtureReviewSource {
    fn failure_category(&self, _error: &Self::Error) -> SourceIngestionFailureCategory {
        SourceIngestionFailureCategory::OtherMandatoryStage
    }
}

fn review_input(
    source_product_id: SourceProductId,
    kind: ReviewKind,
    excerpt: &str,
) -> ReviewInput {
    ReviewInput::new(
        source_product_id,
        kind,
        vec![ReviewExcerpt::new(excerpt).expect("fixture excerpt must be valid")],
    )
    .expect("fixture review input must be valid")
}

fn source_config() -> RuntimeConfig {
    RuntimeConfig::new(
        "m038-source",
        300,
        2,
        HourlyJobSchedule::new(RuntimeJobType::SourceHourlyDiscovery, 3)
            .expect("fixture source schedule must be valid"),
    )
    .expect("fixture source runtime config must be valid")
    .with_claim_filter(RuntimeJobTypeFilter::source_lane())
}

fn paced_source_config() -> RuntimeConfig {
    source_config().with_claim_pacing(
        JobClaimPacing::new("source", 3).expect("fixture source pace must be valid"),
    )
}

fn summary_config() -> RuntimeConfig {
    RuntimeConfig::worker_only("m038-summary", 300, 1)
        .expect("fixture summary runtime config must be valid")
        .with_claim_filter(RuntimeJobTypeFilter::llm_lane())
}

fn runtimes(
    database: &TemporaryDatabase,
    discovery: FixtureDailySource,
    reviews: FixtureReviewSource,
) -> (
    Runtime<SqliteJobStore, FixedClock>,
    Runtime<SqliteJobStore, FixedClock>,
) {
    runtimes_with_clock(
        database,
        discovery,
        reviews,
        source_config(),
        Arc::new(FixedClock(10)),
    )
}

fn paced_runtimes(
    database: &TemporaryDatabase,
    discovery: FixtureDailySource,
    reviews: FixtureReviewSource,
) -> (
    Runtime<SqliteJobStore, SystemRuntimeClock>,
    Runtime<SqliteJobStore, SystemRuntimeClock>,
) {
    runtimes_with_clock(
        database,
        discovery,
        reviews,
        paced_source_config(),
        Arc::new(SystemRuntimeClock),
    )
}

fn runtimes_with_clock<C>(
    database: &TemporaryDatabase,
    discovery: FixtureDailySource,
    reviews: FixtureReviewSource,
    source_runtime_config: RuntimeConfig,
    clock: Arc<C>,
) -> (Runtime<SqliteJobStore, C>, Runtime<SqliteJobStore, C>)
where
    C: RuntimeClock,
{
    let queue = Arc::new(Mutex::new(
        SqliteJobStore::open(&database.path).expect("fixture queue must open"),
    ));
    let daily_state = Arc::new(Mutex::new(
        SqliteDailyCrawlStateStore::open(&database.path).expect("fixture daily state must open"),
    ));
    let review_store = Arc::new(Mutex::new(
        SqliteReviewSummaryStore::open(&database.path).expect("fixture review store must open"),
    ));
    let discovery_handler: Arc<dyn JobHandler> = Arc::new(HourlyDiscoveryHandler::new(
        daily_state,
        discovery,
        gamepulse_application::SourceIngestionJobSchedule::new(3)
            .expect("fixture ingestion schedule must be valid"),
    ));
    let ingestion_handler: Arc<dyn JobHandler> = Arc::new(ReviewSourceIngestionHandler::new(
        review_store.clone(),
        reviews,
        ReviewSummaryJobSchedule::new(3).expect("fixture summary schedule must be valid"),
    ));
    let summary_handler: Arc<dyn JobHandler> = Arc::new(ReviewSummaryHandler::new(
        review_store,
        LocalExtractiveReviewSummarizer,
    ));
    let source_runtime = Runtime::new(
        queue.clone(),
        clock.clone(),
        source_runtime_config,
        Arc::new(
            JobHandlerRegistry::new([discovery_handler, ingestion_handler])
                .expect("fixture source handlers must be valid"),
        ),
    );
    let summary_runtime = Runtime::new(
        queue,
        clock,
        summary_config(),
        Arc::new(
            JobHandlerRegistry::new([summary_handler])
                .expect("fixture summary handlers must be valid"),
        ),
    );
    (source_runtime, summary_runtime)
}

struct ShortVideoObserver {
    inner: SqliteAcceptanceCycleStore,
}

struct FinalAggregateReadFailureObserver {
    inner: SqliteAcceptanceCycleStore,
    complete_snapshot_seen: bool,
}

impl FinalAggregateReadFailureObserver {
    fn open(path: &Path) -> Self {
        Self {
            inner: SqliteAcceptanceCycleStore::open(path)
                .expect("fixture acceptance reader must open"),
            complete_snapshot_seen: false,
        }
    }
}

impl AcceptanceCycleReadPort for FinalAggregateReadFailureObserver {
    type Error = AcceptanceCycleReadStoreError;

    fn acceptance_cycle_snapshot(
        &mut self,
    ) -> Result<AcceptanceCycleSnapshot, AcceptanceCycleReadStoreError> {
        let snapshot = self.inner.acceptance_cycle_snapshot()?;
        if self.complete_snapshot_seen {
            return Err(AcceptanceCycleReadStoreError);
        }
        if acceptance_cycle_is_complete(snapshot) {
            self.complete_snapshot_seen = true;
        }
        Ok(snapshot)
    }
}

fn acceptance_cycle_is_complete(snapshot: AcceptanceCycleSnapshot) -> bool {
    snapshot.selected() == 20
        && snapshot.source_ingestion().total() == 20
        && snapshot.source_ingestion().succeeded() == 20
        && snapshot.summaries().total() == 40
        && snapshot.summaries().succeeded() == 40
        && snapshot.persisted() == 20
        && snapshot.complete_video() == 20
        && snapshot.summaries_ready() == 20
        && snapshot.summaries_pending_or_missing() == 0
}

impl ShortVideoObserver {
    fn open(path: &std::path::Path) -> Self {
        Self {
            inner: SqliteAcceptanceCycleStore::open(path)
                .expect("fixture acceptance reader must open"),
        }
    }
}

impl AcceptanceCycleReadPort for ShortVideoObserver {
    type Error = AcceptanceCycleReadStoreError;

    fn acceptance_cycle_snapshot(
        &mut self,
    ) -> Result<AcceptanceCycleSnapshot, AcceptanceCycleReadStoreError> {
        let snapshot = self.inner.acceptance_cycle_snapshot()?;
        Ok(AcceptanceCycleSnapshot::new(
            snapshot.selected(),
            snapshot.source_ingestion(),
            snapshot.summaries(),
            snapshot.persisted(),
            snapshot.complete_video().saturating_sub(1),
            snapshot.summaries_ready(),
            snapshot.summaries_pending_or_missing(),
            snapshot.failures(),
        ))
    }
}

fn command(path: PathBuf, deadline_seconds: u64) -> AcceptanceCommand {
    AcceptanceCommand::new(path, 20, deadline_seconds).expect("fixture command must be valid")
}

#[test]
fn observed_failure_counts_start_zero_increment_reset_and_hold_no_private_values() {
    let mut counts = FailureCategoryCounts::zero();
    assert_eq!(counts, FailureCategoryCounts::default());
    for category in [
        WorkerFailureCategory::MissingRequiredVideo,
        WorkerFailureCategory::SourceTransportOrContract,
        WorkerFailureCategory::PersistenceOrQueue,
        WorkerFailureCategory::OtherMandatory,
    ] {
        counts.increment(category);
    }
    assert_eq!(counts.missing_required_video(), 1);
    assert_eq!(counts.source_transport_or_contract(), 1);
    assert_eq!(counts.persistence_or_queue(), 1);
    assert_eq!(counts.other_mandatory(), 1);
    assert_eq!(
        WorkerFailureCategory::MissingRequiredVideo.as_str(),
        "missing_required_video"
    );
    assert!(!format!("{counts:?}").contains("fixture"));
    assert!(!format!("{counts:?}").contains("example-game"));
    counts.reset();
    assert_eq!(counts, FailureCategoryCounts::zero());
}

#[tokio::test]
async fn acceptance_runs_one_cycle_and_drains_only_its_mandatory_summary_jobs() {
    let database = TemporaryDatabase::new("complete");
    let discovery = FixtureDailySource::complete();
    let reviews = FixtureReviewSource::new(FixtureReviewMode::AlwaysValid);
    let (mut source_runtime, mut summary_runtime) =
        runtimes(&database, discovery.clone(), reviews.clone());
    let mut observation = SqliteAcceptanceCycleStore::open(&database.path)
        .expect("fixture acceptance reader must open");

    let report = run_acceptance_once(
        &mut source_runtime,
        &mut summary_runtime,
        &mut observation,
        &command(database.path.clone(), 5),
    )
    .await;

    assert_eq!(report.terminal(), AcceptanceTerminal::Complete);
    assert_eq!(discovery.calls(), 1);
    assert_eq!(reviews.calls(), 20);
    assert_eq!(report.snapshot().selected(), 20);
    assert_eq!(report.snapshot().source_ingestion().attempted(), 20);
    assert_eq!(report.snapshot().persisted(), 20);
    assert_eq!(report.snapshot().complete_video(), 20);
    assert_eq!(report.snapshot().summaries_ready(), 20);
    assert_eq!(report.snapshot().summaries().total(), 40);
    assert_eq!(report.snapshot().summaries().succeeded(), 40);
}

#[tokio::test]
async fn acceptance_stops_after_the_first_retryable_mandatory_failure_without_retrying() {
    let database = TemporaryDatabase::new("failure");
    let discovery = FixtureDailySource::complete();
    let reviews = FixtureReviewSource::new(FixtureReviewMode::FirstCallFails);
    let (mut source_runtime, mut summary_runtime) =
        runtimes(&database, discovery.clone(), reviews.clone());
    let mut observation = SqliteAcceptanceCycleStore::open(&database.path)
        .expect("fixture acceptance reader must open");

    let report = run_acceptance_once(
        &mut source_runtime,
        &mut summary_runtime,
        &mut observation,
        &command(database.path.clone(), 5),
    )
    .await;

    assert_eq!(report.terminal(), AcceptanceTerminal::MandatoryJobFailure);
    assert_eq!(discovery.calls(), 1);
    assert_eq!(reviews.calls(), 2);
    assert_eq!(report.snapshot().source_ingestion().attempted(), 2);
    assert_eq!(report.snapshot().summaries().attempted(), 0);
    assert_eq!(
        report.snapshot().failures().source_other_mandatory_stage(),
        1
    );
}

#[tokio::test]
async fn acceptance_waits_for_persisted_source_pacing_before_settling_a_failure() {
    let database = TemporaryDatabase::new("paced-failure");
    let discovery = FixtureDailySource::complete();
    let reviews = FixtureReviewSource::new(FixtureReviewMode::FirstCallFails);
    let (mut source_runtime, mut summary_runtime) =
        paced_runtimes(&database, discovery.clone(), reviews.clone());
    let mut observation = SqliteAcceptanceCycleStore::open(&database.path)
        .expect("fixture acceptance reader must open");

    let report = run_acceptance_once(
        &mut source_runtime,
        &mut summary_runtime,
        &mut observation,
        &command(database.path.clone(), 10),
    )
    .await;

    assert_eq!(report.terminal(), AcceptanceTerminal::MandatoryJobFailure);
    assert_eq!(discovery.calls(), 1);
    assert_eq!(reviews.calls(), 1);
    assert_eq!(report.snapshot().source_ingestion().attempted(), 1);
    assert_eq!(report.snapshot().summaries().attempted(), 0);
}

#[tokio::test]
async fn initial_schedule_failure_counts_as_process_local_persistence_or_queue() {
    let database = TemporaryDatabase::new("initial-schedule-failure");
    let queue = Arc::new(Mutex::new(
        SqliteJobStore::open(&database.path).expect("fixture queue must open"),
    ));
    let mut source_runtime = Runtime::new(
        queue.clone(),
        Arc::new(FixedClock(-1)),
        source_config(),
        Arc::new(JobHandlerRegistry::default()),
    );
    let mut summary_runtime = Runtime::new(
        queue,
        Arc::new(FixedClock(-1)),
        summary_config(),
        Arc::new(JobHandlerRegistry::default()),
    );
    let mut observation = SqliteAcceptanceCycleStore::open(&database.path)
        .expect("fixture acceptance reader must open");

    let report = run_acceptance_once(
        &mut source_runtime,
        &mut summary_runtime,
        &mut observation,
        &command(database.path.clone(), 5),
    )
    .await;

    assert_eq!(report.terminal(), AcceptanceTerminal::RuntimeFailure);
    assert_eq!(report.observed_failures().persistence_or_queue(), 1);
    assert_eq!(report.observed_failures().missing_required_video(), 0);
    assert_eq!(report.snapshot(), AcceptanceCycleSnapshot::default());
}

#[tokio::test]
async fn acceptance_hard_deadline_aborts_the_pending_first_cycle() {
    let database = TemporaryDatabase::new("deadline");
    let discovery = FixtureDailySource::pending();
    let reviews = FixtureReviewSource::new(FixtureReviewMode::AlwaysValid);
    let (mut source_runtime, mut summary_runtime) = runtimes(&database, discovery.clone(), reviews);
    let mut observation = SqliteAcceptanceCycleStore::open(&database.path)
        .expect("fixture acceptance reader must open");

    let report = run_acceptance_once(
        &mut source_runtime,
        &mut summary_runtime,
        &mut observation,
        &command(database.path.clone(), 1),
    )
    .await;

    assert_eq!(report.terminal(), AcceptanceTerminal::Deadline);
    assert_eq!(discovery.calls(), 1);
    assert_eq!(report.snapshot().selected(), 0);
}

#[tokio::test]
async fn acceptance_rejects_a_short_complete_video_result() {
    let database = TemporaryDatabase::new("target");
    let discovery = FixtureDailySource::complete();
    let reviews = FixtureReviewSource::new(FixtureReviewMode::AlwaysValid);
    let (mut source_runtime, mut summary_runtime) = runtimes(&database, discovery, reviews);
    let mut observation = ShortVideoObserver::open(&database.path);

    let report = run_acceptance_once(
        &mut source_runtime,
        &mut summary_runtime,
        &mut observation,
        &command(database.path.clone(), 5),
    )
    .await;

    assert_eq!(report.terminal(), AcceptanceTerminal::TargetFailure);
    assert_eq!(report.snapshot().complete_video(), 19);
}

#[tokio::test]
async fn acceptance_final_aggregate_read_failure_is_runtime_failure() {
    let database = TemporaryDatabase::new("final-read-failure");
    let discovery = FixtureDailySource::complete();
    let reviews = FixtureReviewSource::new(FixtureReviewMode::AlwaysValid);
    let (mut source_runtime, mut summary_runtime) = runtimes(&database, discovery, reviews);
    let mut observation = FinalAggregateReadFailureObserver::open(&database.path);

    let report = run_acceptance_once(
        &mut source_runtime,
        &mut summary_runtime,
        &mut observation,
        &command(database.path.clone(), 5),
    )
    .await;

    assert_eq!(report.terminal(), AcceptanceTerminal::RuntimeFailure);
    assert_eq!(report.exit_code(), 1);
    assert_eq!(report.snapshot(), AcceptanceCycleSnapshot::default());
    assert!(
        report
            .to_json()
            .contains("\"terminal_outcome\":\"runtime_failure\"")
    );
}

#[test]
fn acceptance_command_defaults_to_twenty_requires_a_fresh_absolute_database_and_reports_no_private_values()
 {
    let command = parse_entry_command([
        std::ffi::OsString::from("acceptance-once"),
        std::ffi::OsString::from("--database"),
        std::ffi::OsString::from("/tmp/gamepulse-m038-fresh.sqlite3"),
        std::ffi::OsString::from("--target"),
        std::ffi::OsString::from("20"),
        std::ffi::OsString::from("--deadline-seconds"),
        std::ffi::OsString::from("180"),
    ])
    .expect("command must parse");
    let acceptance::EntryCommand::Acceptance(command) = command else {
        panic!("acceptance command must select one-shot mode");
    };
    assert_eq!(command.target(), 20);
    assert_eq!(
        command.database_path(),
        std::path::Path::new("/tmp/gamepulse-m038-fresh.sqlite3")
    );
    assert!(AcceptanceCommand::new(PathBuf::from("relative.sqlite3"), 20, 60).is_err());
    assert!(AcceptanceCommand::new(PathBuf::from("/tmp/fixture.sqlite3"), 19, 60).is_err());

    let database = TemporaryDatabase::new("fresh");
    assert!(database_path_is_fresh(&database.path));
    fs::write(&database.path, b"not an acceptance database")
        .expect("existing fixture file must write");
    assert!(!database_path_is_fresh(&database.path));
    assert_eq!(
        fs::read(&database.path).expect("caller-owned database must remain readable"),
        b"not an acceptance database"
    );
    fs::remove_file(&database.path).expect("fixture database must remove");
    for sidecar_suffix in ["-journal", "-shm", "-wal"] {
        let sidecar = PathBuf::from(format!("{}{}", database.path.display(), sidecar_suffix));
        fs::write(&sidecar, b"caller-owned sidecar").expect("fixture sidecar must write");
        assert!(!database_path_is_fresh(&database.path));
        assert_eq!(
            fs::read(&sidecar).expect("caller-owned sidecar must remain readable"),
            b"caller-owned sidecar"
        );
        fs::remove_file(sidecar).expect("fixture sidecar must remove");
    }

    let report = acceptance::AcceptanceReport::new(
        AcceptanceTerminal::MandatoryJobFailure,
        20,
        AcceptanceCycleSnapshot::default(),
        7,
    );
    let report_json = report.to_json();
    assert!(report_json.contains("\"schema_version\":\"gamepulse.acceptance.v1\""));
    assert!(!report_json.contains("Fixture game"));
    assert!(!report_json.contains("fixture-1"));
    assert!(!report_json.contains("/tmp/"));
    assert!(!report_json.contains("fixture-video"));
    assert!(!report_json.contains("Critics liked this."));
    assert!(!report_json.contains("payload"));
    assert!(!report_json.contains("credential"));
    assert_eq!(
        acceptance::AcceptanceReport::new(
            AcceptanceTerminal::Complete,
            20,
            AcceptanceCycleSnapshot::default(),
            0,
        )
        .exit_code(),
        0
    );
    assert_eq!(report.exit_code(), 3);
    assert_eq!(
        acceptance::AcceptanceReport::new(
            AcceptanceTerminal::RuntimeFailure,
            20,
            AcceptanceCycleSnapshot::default(),
            0,
        )
        .exit_code(),
        1
    );
    assert_eq!(
        acceptance::AcceptanceReport::new(
            AcceptanceTerminal::ConfigurationFailure,
            20,
            AcceptanceCycleSnapshot::default(),
            0,
        )
        .exit_code(),
        3
    );
}

#[test]
fn acceptance_help_exits_successfully_before_runtime_composition() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_gamepulse"))
        .arg("acceptance-once")
        .arg("--help")
        .output()
        .expect("fixture binary must start for help");

    assert!(output.status.success());
    assert_eq!(output.stdout, ACCEPTANCE_HELP.as_bytes());
    assert!(output.stderr.is_empty());
    assert!(ACCEPTANCE_HELP.contains("--database <ABSOLUTE_DATABASE_PATH>"));
    assert!(ACCEPTANCE_HELP.contains("--target 20"));
    assert!(ACCEPTANCE_HELP.contains("--deadline-seconds <POSITIVE_SECONDS>"));
}

#[test]
fn documented_offline_shell_template_forwards_a_quoted_database_path_without_composition() {
    let readme = include_str!("../../../README.md");
    let documented_template = format!(
        "(\n  acceptance_dir=\"$(mktemp -d /tmp/gamepulse-acceptance.XXXXXX)\" || exit 1\n{DOCUMENTED_RUN_TEMPLATE_BODY}"
    );
    assert!(
        readme.contains(DOCUMENTED_HELP_COMMAND),
        "README must preserve the canonical offline help command"
    );
    assert!(
        readme.contains(&documented_template),
        "README and process test must share the canonical command shape"
    );

    let harness = TemporaryInvocationHarness::new();
    harness.write_cargo_wrapper();
    let help = harness
        .shell(DOCUMENTED_HELP_COMMAND)
        .output()
        .expect("documented help shell command must start");
    assert!(help.status.success());
    assert_eq!(help.stdout, ACCEPTANCE_HELP.as_bytes());
    assert!(help.stderr.is_empty());

    let database_path = harness.database_path();
    assert!(database_path.is_absolute());
    assert!(
        !database_path.exists(),
        "test database file must start fresh and non-empty as a path value"
    );
    fs::write(
        PathBuf::from(format!("{}-wal", database_path.display())),
        b"caller-owned-sidecar",
    )
    .expect("caller-owned sidecar must guard the no-composition process test");

    for command in [
        "cargo run --locked --offline -p gamepulse -- acceptance-once --deadline-seconds 180"
            .to_string(),
        "cargo run --locked --offline -p gamepulse -- acceptance-once --database '' --target 20 --deadline-seconds 180"
            .to_string(),
    ] {
        let output = harness
            .shell(&command)
            .output()
            .expect("invalid documented-shape shell command must start");
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, b"invalid command\n");
        assert!(
            !database_path.exists(),
            "invalid arguments must stop before SQLite opens"
        );
    }

    let test_template = format!(
        "(\n  acceptance_dir=\"$GAMEPULSE_M041_ACCEPTANCE_DIR\"\n{DOCUMENTED_RUN_TEMPLATE_BODY}"
    );
    let output = harness
        .shell(&test_template)
        .output()
        .expect("documented acceptance shell template must start");
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stderr.is_empty());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("configuration report must be JSON");
    assert_eq!(
        report
            .get("terminal_outcome")
            .and_then(serde_json::Value::as_str),
        Some("configuration_failure")
    );
    assert_eq!(
        report.get("target").and_then(serde_json::Value::as_u64),
        Some(20)
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains(&database_path.display().to_string()),
        "aggregate report must not disclose the caller path"
    );
    assert_eq!(
        fs::read_to_string(harness.argument_record())
            .expect("shell-to-cargo argument record must be readable"),
        format!(
            "run\n--locked\n--offline\n-p\ngamepulse\n--\nacceptance-once\n--database\n{}\n--target\n20\n--deadline-seconds\n180\n",
            database_path.display()
        )
    );
    assert_eq!(
        fs::read_to_string(harness.composition_record())
            .expect("pre-composition guard record must be readable"),
        "database_not_opened_before_runtime_composition\n"
    );
    assert!(
        !harness.acceptance_dir.exists(),
        "the documented caller-owned cleanup must remove only its temporary directory"
    );
}

#[test]
fn malformed_or_missing_acceptance_arguments_exit_two_without_opening_sqlite() {
    let database = TemporaryDatabase::new("invalid-arguments");
    let database_argument = database.path.as_os_str().to_os_string();
    let invalid_argument_sets = vec![
        vec![
            std::ffi::OsString::from("--deadline-seconds"),
            std::ffi::OsString::from("1"),
        ],
        vec![
            std::ffi::OsString::from("--database"),
            database_argument.clone(),
        ],
        vec![
            std::ffi::OsString::from("--database"),
            database_argument.clone(),
            std::ffi::OsString::from("--deadline-seconds"),
            std::ffi::OsString::from("0"),
        ],
        vec![
            std::ffi::OsString::from("--database"),
            database_argument.clone(),
            std::ffi::OsString::from("--deadline-seconds"),
            std::ffi::OsString::from("1"),
            std::ffi::OsString::from("--target"),
            std::ffi::OsString::from("twenty"),
        ],
        vec![
            std::ffi::OsString::from("--database"),
            database_argument,
            std::ffi::OsString::from("--deadline-seconds"),
            std::ffi::OsString::from("1"),
            std::ffi::OsString::from("--target"),
        ],
    ];

    for arguments in invalid_argument_sets {
        let mut process = std::process::Command::new(env!("CARGO_BIN_EXE_gamepulse"));
        process.arg("acceptance-once").args(arguments);

        let output = process
            .output()
            .expect("fixture binary must start for invalid arguments");
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, b"invalid command\n");
        assert!(database_path_is_fresh(&database.path));
    }
}
