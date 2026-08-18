#![forbid(unsafe_code)]

mod acceptance;
mod covers;
mod observability;
mod runtime;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gamepulse_application::{
    CoverBackfillReport, HourlyJobSchedule, JobClaimPacing, JobHandler, JobHandlerRegistry,
    ReviewSummaryJobSchedule, RuntimeJobType, RuntimeJobTypeFilter, SourceIngestionJobSchedule,
    execute_cover_backfill,
};
use gamepulse_storage_sqlite::{
    SqliteAcceptanceCycleStore, SqliteGameCatalogueReadStore, SqliteGameCoverAssetStore,
    SqliteJobStore, SqliteReadinessProbe, SqliteReviewSummaryStore, SqliteRunProgressStore,
};
use gamepulse_worker_llm::{LocalExtractiveReviewSummarizer, ReviewSummaryHandler};
use gamepulse_worker_source::{
    DurableRunDiscoveryHandler, DurableRunReviewSourceIngestionHandler, MetacriticCanaryClient,
    MetacriticCoverImageClient, MetacriticDailyCrawlSource, MetacriticGameReviewSource,
    MetacriticPublicHtmlTransport, PublicHtmlCoverEnricher,
};
use observability::{LogFormat, ObservedJobHandler, ObservedPublicCoverEnricher};
use runtime::{Runtime, RuntimeConfig, SystemRuntimeClock};
use tokio::sync::Notify;

use acceptance::{
    AcceptanceCommand, AcceptanceReport, AcceptanceTerminal, EntryCommand, database_path_is_fresh,
    parse_entry_command, run_acceptance_once,
};
use covers::{CoverBackfillEntry, parse_cover_backfill};

const DATABASE_PATH_ENV: &str = "GAMEPULSE_DATABASE_PATH";
const HTTP_ADDRESS_ENV: &str = "GAMEPULSE_HTTP_ADDRESS";
const LOG_FORMAT_ENV: &str = "GAMEPULSE_LOG_FORMAT";
const SOURCE_WORK_ENABLED_ENV: &str = "GAMEPULSE_SOURCE_WORK_ENABLED";
const SOURCE_LANE_MINIMUM_CLAIM_INTERVAL_SECONDS: i64 = 2;

struct RuntimeEnvironment {
    database_path: PathBuf,
    http_address: SocketAddr,
    log_format: LogFormat,
    source_work_enabled: bool,
}

struct RuntimeStorage {
    store: Arc<Mutex<SqliteJobStore>>,
    run_progress: Arc<Mutex<SqliteRunProgressStore>>,
    review_summaries: Arc<Mutex<SqliteReviewSummaryStore>>,
    catalogue: Arc<Mutex<SqliteGameCatalogueReadStore>>,
}

impl RuntimeStorage {
    fn open(path: impl AsRef<Path>) -> Result<Self, Box<dyn std::error::Error>> {
        let path = path.as_ref();
        Ok(Self {
            store: Arc::new(Mutex::new(SqliteJobStore::open(path)?)),
            run_progress: Arc::new(Mutex::new(SqliteRunProgressStore::open(path)?)),
            review_summaries: Arc::new(Mutex::new(SqliteReviewSummaryStore::open(path)?)),
            catalogue: Arc::new(Mutex::new(SqliteGameCatalogueReadStore::open(path)?)),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RuntimeEnvironmentError;

impl std::fmt::Display for RuntimeEnvironmentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("GamePulse environment configuration is invalid")
    }
}

impl std::error::Error for RuntimeEnvironmentError {}

impl RuntimeEnvironment {
    fn load() -> Result<Self, RuntimeEnvironmentError> {
        let database_path = std::env::var(DATABASE_PATH_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or(RuntimeEnvironmentError)?;
        let http_address = std::env::var(HTTP_ADDRESS_ENV)
            .ok()
            .and_then(|value| value.parse::<SocketAddr>().ok())
            .ok_or(RuntimeEnvironmentError)?;
        let log_format_value = std::env::var(LOG_FORMAT_ENV).ok();
        let log_format =
            LogFormat::parse(log_format_value.as_deref()).map_err(|_| RuntimeEnvironmentError)?;
        let source_work_enabled = match std::env::var(SOURCE_WORK_ENABLED_ENV) {
            Ok(value) if value == "true" => true,
            Ok(value) if value == "false" => false,
            Ok(_) | Err(std::env::VarError::NotUnicode(_)) => return Err(RuntimeEnvironmentError),
            Err(std::env::VarError::NotPresent) => true,
        };
        Ok(Self {
            database_path,
            http_address,
            log_format,
            source_work_enabled,
        })
    }
}

#[tokio::main]
async fn main() {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    match parse_cover_backfill(arguments.clone()) {
        Ok(Some(CoverBackfillEntry::Help)) => print!("{}", covers::COVER_BACKFILL_HELP),
        Ok(Some(CoverBackfillEntry::Command(command))) => {
            let exit_code = run_cover_backfill(command).await;
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
        }
        Ok(None) => match parse_entry_command(arguments) {
            Ok(EntryCommand::Serve) => {
                if run().await.is_err() {
                    std::process::exit(1);
                }
            }
            Ok(EntryCommand::AcceptanceHelp) => {
                print!("{}", acceptance::ACCEPTANCE_HELP);
            }
            Ok(EntryCommand::Acceptance(command)) => {
                let report = run_acceptance(command).await;
                println!("{}", report.to_json());
                let exit_code = report.exit_code();
                if exit_code != 0 {
                    std::process::exit(exit_code);
                }
            }
            Err(_) => {
                eprintln!("invalid command");
                std::process::exit(2);
            }
        },
        Err(_) => {
            eprintln!("invalid command");
            std::process::exit(2);
        }
    }
}

async fn run_cover_backfill(command: covers::CoverBackfillCommand) -> i32 {
    let mut store = match SqliteGameCoverAssetStore::open(command.database_path()) {
        Ok(store) => store,
        Err(_) => return 1,
    };
    let client = match MetacriticCoverImageClient::new() {
        Ok(client) => client,
        Err(_) => return 1,
    };
    match execute_cover_backfill(&mut store, &client, command.limit()).await {
        Ok(report) => {
            let (output, exit_code) = render_cover_backfill_report(report);
            println!("{output}");
            exit_code
        }
        Err(_) => 1,
    }
}

/// The binary owns only aggregate JSON framing and the report's application-owned exit policy.
fn render_cover_backfill_report(report: CoverBackfillReport) -> (String, i32) {
    (report.to_json(), report.exit_code())
}

/// The composition root owns concrete SQLite, clock, scheduler, and source-lane wiring.
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let environment = RuntimeEnvironment::load()?;
    observability::initialize_subscriber(environment.log_format)
        .map_err(|_| RuntimeEnvironmentError)?;
    let readiness = Arc::new(SqliteReadinessProbe::new(&environment.database_path));
    let storage = match RuntimeStorage::open(&environment.database_path) {
        Ok(storage) => storage,
        Err(_) => {
            return serve_unready_http(
                environment.http_address,
                readiness,
                environment.source_work_enabled,
            )
            .await;
        }
    };
    let wakeup = Arc::new(Notify::new());
    let mut source_runtime = if environment.source_work_enabled {
        let schedule = HourlyJobSchedule::new(RuntimeJobType::SourceHourlyDiscovery, 3)?;
        let source_config = RuntimeConfig::new("gamepulse-source-runtime", 300, 2, schedule)?
            .with_claim_filter(RuntimeJobTypeFilter::source_lane())
            .with_claim_pacing(JobClaimPacing::new(
                "source",
                SOURCE_LANE_MINIMUM_CLAIM_INTERVAL_SECONDS,
            )?);
        Some(compose_source_runtime(
            &storage,
            source_config,
            wakeup.clone(),
        )?)
    } else {
        observability::source_work_disabled();
        None
    };
    let mut llm_runtime = compose_summary_runtime(&storage, wakeup.clone())?;
    let listener = tokio::net::TcpListener::bind(environment.http_address).await?;
    let web_server = axum::serve(
        listener,
        gamepulse_web::service_router(storage.catalogue, readiness)
            .layer(axum::middleware::from_fn(observability::trace_http_request)),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    });

    let source_runtime_result = async {
        if let Some(source_runtime) = source_runtime.as_mut() {
            source_runtime
                .run_until_shutdown_with_wakeup(wakeup.clone(), async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await
                .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })
        } else {
            let _ = tokio::signal::ctrl_c().await;
            Ok::<(), Box<dyn std::error::Error>>(())
        }
    };
    let llm_runtime_result = async {
        llm_runtime
            .run_until_shutdown_with_wakeup(wakeup.clone(), async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
            .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })
    };
    let web_result = async {
        web_server
            .await
            .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })
    };
    observability::process_started(environment.source_work_enabled);
    let result = tokio::try_join!(source_runtime_result, llm_runtime_result, web_result);
    if result.is_ok() {
        observability::process_stopped();
    }
    result?;
    Ok(())
}

/// Compose the production source lane once so long-running and one-shot entrypoints share the
/// same source adapters, durable stores, typed handlers, and summary schedule.
fn compose_source_runtime(
    storage: &RuntimeStorage,
    config: RuntimeConfig,
    wakeup: Arc<Notify>,
) -> Result<Runtime<SqliteJobStore, SystemRuntimeClock>, Box<dyn std::error::Error>> {
    let review_summary_schedule = ReviewSummaryJobSchedule::new(3)?;
    let source_client = MetacriticCanaryClient::new()?;
    let public_html_cover = ObservedPublicCoverEnricher::new(PublicHtmlCoverEnricher::new(
        MetacriticPublicHtmlTransport::new()?,
    ));
    let source_port = MetacriticDailyCrawlSource::new(source_client.clone());
    let source_ingestion_schedule = SourceIngestionJobSchedule::new(3)?;
    let source_handler: Arc<dyn JobHandler> =
        Arc::new(ObservedJobHandler::new(DurableRunDiscoveryHandler::new(
            storage.run_progress.clone(),
            source_port,
            source_ingestion_schedule,
        )));
    let ingestion_handler: Arc<dyn JobHandler> = Arc::new(ObservedJobHandler::new(
        DurableRunReviewSourceIngestionHandler::new(
            storage.run_progress.clone(),
            MetacriticGameReviewSource::with_public_cover_enricher(
                source_client,
                public_html_cover,
            ),
            review_summary_schedule,
            source_ingestion_schedule,
        ),
    ));
    let handlers = Arc::new(JobHandlerRegistry::new([
        source_handler,
        ingestion_handler,
    ])?);
    Ok(Runtime::new(
        storage.store.clone(),
        Arc::new(SystemRuntimeClock),
        config,
        handlers,
    )
    .with_wakeup(wakeup))
}

/// Compose the production local-review-summary lane once for both entrypoint modes.
fn compose_summary_runtime(
    storage: &RuntimeStorage,
    wakeup: Arc<Notify>,
) -> Result<Runtime<SqliteJobStore, SystemRuntimeClock>, Box<dyn std::error::Error>> {
    let config = RuntimeConfig::worker_only("gamepulse-llm-runtime", 300, 1)?
        .with_claim_filter(RuntimeJobTypeFilter::llm_lane());
    let handler: Arc<dyn JobHandler> =
        Arc::new(ObservedJobHandler::new(ReviewSummaryHandler::new(
            storage.review_summaries.clone(),
            LocalExtractiveReviewSummarizer,
        )));
    let handlers = Arc::new(JobHandlerRegistry::new([handler])?);
    Ok(Runtime::new(
        storage.store.clone(),
        Arc::new(SystemRuntimeClock),
        config,
        handlers,
    )
    .with_wakeup(wakeup))
}

async fn run_acceptance(command: AcceptanceCommand) -> AcceptanceReport {
    if !database_path_is_fresh(command.database_path()) {
        return AcceptanceReport::new(
            AcceptanceTerminal::ConfigurationFailure,
            command.target(),
            Default::default(),
            0,
        );
    }
    let storage = match RuntimeStorage::open(command.database_path()) {
        Ok(storage) => storage,
        Err(_) => {
            return AcceptanceReport::new(
                AcceptanceTerminal::RuntimeFailure,
                command.target(),
                Default::default(),
                0,
            );
        }
    };
    let mut observation = match SqliteAcceptanceCycleStore::open(command.database_path()) {
        Ok(observation) => observation,
        Err(_) => {
            return AcceptanceReport::new(
                AcceptanceTerminal::RuntimeFailure,
                command.target(),
                Default::default(),
                0,
            );
        }
    };
    let wakeup = Arc::new(Notify::new());
    let schedule = match HourlyJobSchedule::new(RuntimeJobType::SourceHourlyDiscovery, 3) {
        Ok(schedule) => schedule,
        Err(_) => {
            return AcceptanceReport::new(
                AcceptanceTerminal::RuntimeFailure,
                command.target(),
                Default::default(),
                0,
            );
        }
    };
    // The one-shot coordinator never enters the paced timer loop. It drives the same handlers
    // through one enqueue and completion joins only, while retaining the durable source pace.
    let source_pacing =
        match JobClaimPacing::new("source", SOURCE_LANE_MINIMUM_CLAIM_INTERVAL_SECONDS) {
            Ok(pacing) => pacing,
            Err(_) => {
                return AcceptanceReport::new(
                    AcceptanceTerminal::RuntimeFailure,
                    command.target(),
                    Default::default(),
                    0,
                );
            }
        };
    let source_config = match RuntimeConfig::new("gamepulse-acceptance-source", 300, 2, schedule) {
        Ok(config) => config
            .with_claim_filter(RuntimeJobTypeFilter::source_lane())
            .with_claim_pacing(source_pacing),
        Err(_) => {
            return AcceptanceReport::new(
                AcceptanceTerminal::RuntimeFailure,
                command.target(),
                Default::default(),
                0,
            );
        }
    };
    let mut source_runtime = match compose_source_runtime(&storage, source_config, wakeup.clone()) {
        Ok(runtime) => runtime,
        Err(_) => {
            return AcceptanceReport::new(
                AcceptanceTerminal::RuntimeFailure,
                command.target(),
                Default::default(),
                0,
            );
        }
    };
    let mut summary_runtime = match compose_summary_runtime(&storage, wakeup) {
        Ok(runtime) => runtime,
        Err(_) => {
            return AcceptanceReport::new(
                AcceptanceTerminal::RuntimeFailure,
                command.target(),
                Default::default(),
                0,
            );
        }
    };
    run_acceptance_once(
        &mut source_runtime,
        &mut summary_runtime,
        &mut observation,
        &command,
    )
    .await
}

async fn serve_unready_http(
    http_address: SocketAddr,
    readiness: Arc<SqliteReadinessProbe>,
    source_work_enabled: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind(http_address).await?;
    let server = axum::serve(
        listener,
        gamepulse_web::unavailable_service_router(readiness)
            .layer(axum::middleware::from_fn(observability::trace_http_request)),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    });
    observability::process_started(source_work_enabled);
    let result = server
        .await
        .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) });
    if result.is_ok() {
        observability::process_stopped();
    }
    result
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::future::{Future, Ready, ready};
    use std::path::PathBuf;
    use std::process;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};

    use gamepulse_application::{
        GameCoverDescriptor, GameSnapshot, MAX_COVER_BACKFILL_CANDIDATES, SourceProductId,
        StoredCoverImage, execute_cover_backfill, upsert_game_snapshot,
    };
    use gamepulse_storage_sqlite::{SqliteGameCoverAssetStore, SqliteGameSnapshotStore};
    use gamepulse_worker_source::{
        CoverImageHttpResponse, CoverImageHttpTransport, MetacriticCoverImageClient,
    };
    use rusqlite::Connection;

    use super::render_cover_backfill_report;

    const PNG_SIGNATURE: &[u8] = &[137, 80, 78, 71, 13, 10, 26, 10];

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum FixtureTransportError {
        BodyLimit,
    }

    struct FixtureCoverResponse {
        status: u16,
        content_type: Option<String>,
        content_length: Option<u64>,
        body: Option<Result<Vec<u8>, FixtureTransportError>>,
    }

    impl FixtureCoverResponse {
        fn new(
            status: u16,
            content_type: Option<&str>,
            content_length: Option<u64>,
            body: Vec<u8>,
        ) -> Self {
            Self {
                status,
                content_type: content_type.map(str::to_owned),
                content_length,
                body: Some(Ok(body)),
            }
        }
    }

    impl CoverImageHttpResponse for FixtureCoverResponse {
        type ReadBodyError = FixtureTransportError;
        type ReadBodyFuture<'a>
            = Ready<Result<Vec<u8>, Self::ReadBodyError>>
        where
            Self: 'a;

        fn status(&self) -> u16 {
            self.status
        }

        fn content_type(&self) -> Option<&str> {
            self.content_type.as_deref()
        }

        fn content_length(&self) -> Option<u64> {
            self.content_length
        }

        fn read_body(&mut self) -> Self::ReadBodyFuture<'_> {
            ready(
                self.body
                    .take()
                    .expect("fixture response body must be read at most once"),
            )
        }
    }

    struct FixtureCoverTransport {
        responses: Mutex<VecDeque<Result<FixtureCoverResponse, FixtureTransportError>>>,
        calls: Arc<Mutex<usize>>,
    }

    impl CoverImageHttpTransport for FixtureCoverTransport {
        type Error = FixtureTransportError;
        type Response = FixtureCoverResponse;
        type FetchFuture<'a>
            = Ready<Result<Self::Response, Self::Error>>
        where
            Self: 'a;

        fn fetch_cover(
            &self,
            _url: &gamepulse_application::GamePublicCoverUrl,
        ) -> Self::FetchFuture<'_> {
            *self
                .calls
                .lock()
                .expect("fixture transport calls must not poison") += 1;
            ready(
                self.responses
                    .lock()
                    .expect("fixture transport responses must not poison")
                    .pop_front()
                    .expect("fixture transport needs one response per fetch"),
            )
        }

        fn body_limit_error(&self) -> Self::Error {
            FixtureTransportError::BodyLimit
        }
    }

    struct TemporaryDatabase {
        path: PathBuf,
    }

    impl TemporaryDatabase {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "gamepulse-cover-backfill-binary-{}.sqlite3",
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

    fn snapshot(source_product_id: u64, descriptor: Option<GameCoverDescriptor>) -> GameSnapshot {
        GameSnapshot::new(
            SourceProductId::new(source_product_id).expect("fixture identity must be valid"),
            format!("private-slug-{source_product_id}"),
            "Private title",
            "Private fixture description",
            descriptor,
            None,
            Vec::new(),
            Vec::new(),
        )
        .expect("fixture snapshot must be valid")
    }

    fn descriptor(filename: &str) -> GameCoverDescriptor {
        GameCoverDescriptor::new(
            format!("/provider/7/2/{filename}"),
            "catalog",
            filename,
            "cardImage",
        )
        .expect("fixture descriptor must be valid")
    }

    fn resolver_rejected_descriptor() -> GameCoverDescriptor {
        GameCoverDescriptor::new(
            "/provider/7/../private-rejected.png",
            "catalog",
            "private-rejected.png",
            "cardImage",
        )
        .expect("fixture descriptor must remain structurally valid")
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = std::pin::pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    #[test]
    fn mixed_cover_backfill_fixture_uses_the_real_adapter_classifier_and_binary_report() {
        let database = TemporaryDatabase::new();
        let mut snapshots =
            SqliteGameSnapshotStore::open(&database.path).expect("snapshot store must open");
        upsert_game_snapshot(&mut snapshots, &snapshot(7_001, None))
            .expect("missing descriptor snapshot must persist");
        upsert_game_snapshot(
            &mut snapshots,
            &snapshot(7_002, Some(resolver_rejected_descriptor())),
        )
        .expect("resolver-rejected descriptor snapshot must persist");
        for source_product_id in 7_003..=7_012 {
            upsert_game_snapshot(
                &mut snapshots,
                &snapshot(
                    source_product_id,
                    Some(descriptor(&format!(
                        "private-cover-{source_product_id}.png"
                    ))),
                ),
            )
            .expect("fetchable descriptor snapshot must persist");
        }
        drop(snapshots);

        let transport = FixtureCoverTransport {
            responses: Mutex::new(VecDeque::from([
                Ok(FixtureCoverResponse::new(
                    200,
                    Some("image/png"),
                    Some(PNG_SIGNATURE.len() as u64),
                    PNG_SIGNATURE.to_vec(),
                )),
                Ok(FixtureCoverResponse::new(100, None, None, Vec::new())),
                Ok(FixtureCoverResponse::new(204, None, None, Vec::new())),
                Ok(FixtureCoverResponse::new(302, None, None, Vec::new())),
                Ok(FixtureCoverResponse::new(404, None, None, Vec::new())),
                Ok(FixtureCoverResponse::new(503, None, None, Vec::new())),
                Ok(FixtureCoverResponse::new(200, None, None, Vec::new())),
                Ok(FixtureCoverResponse::new(
                    200,
                    Some("image/png"),
                    Some(27),
                    b"private-response-payload".to_vec(),
                )),
                Ok(FixtureCoverResponse::new(
                    200,
                    Some("image/png"),
                    Some(0),
                    Vec::new(),
                )),
                Ok(FixtureCoverResponse::new(
                    200,
                    Some("image/png"),
                    Some((StoredCoverImage::MAX_BYTES + 1) as u64),
                    Vec::new(),
                )),
            ])),
            calls: Arc::new(Mutex::new(0)),
        };
        let calls = Arc::clone(&transport.calls);
        let client = MetacriticCoverImageClient::with_transport(transport);
        let mut assets =
            SqliteGameCoverAssetStore::open(&database.path).expect("asset store must open");

        let report = block_on(execute_cover_backfill(&mut assets, &client, 20))
            .expect("mixed local cover backfill must complete");
        assert_eq!(report.attempted(), 12);
        assert!(report.attempted() <= MAX_COVER_BACKFILL_CANDIDATES);
        assert_eq!(report.stored(), 1);
        assert_eq!(report.unavailable(), 10);
        assert_eq!(report.unavailable(), report.unavailable_reasons().total());
        assert_eq!(report.failed(), 1);
        assert!(report.made_progress());
        assert_eq!(*calls.lock().expect("fixture call count must load"), 10);
        drop(assets);

        let connection = Connection::open(&database.path).expect("verification database must open");
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM game_cover_assets", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("stored asset count must load"),
            1
        );

        let (output, exit_code) = render_cover_backfill_report(report);
        assert_eq!(exit_code, 1);
        assert_eq!(
            output,
            concat!(
                "{\"schema_version\":\"gamepulse.cover_backfill.v3\",",
                "\"attempted\":12,\"stored\":1,\"unavailable\":10,",
                "\"unavailable_reasons\":{",
                "\"descriptor_rejected\":2,",
                "\"unexpected_http_status\":{",
                "\"informational\":1,\"successful_other\":1,\"redirection\":1,",
                "\"client_error\":1,\"server_error\":1,\"other\":0},",
                "\"unsupported_content_type\":1,\"signature_mismatch\":1,\"invalid_body\":1},",
                "\"stale\":0,\"already_current\":0,\"failed\":1,\"made_progress\":true}"
            )
        );
        for prohibited in [
            "7001",
            "7002",
            "Private title",
            "private-slug",
            "private-cover",
            "/provider/",
            "https://",
            "metacritic.com",
            "filename",
            "host",
            "header",
            "private-response-payload",
            "/tmp/",
        ] {
            assert!(!output.contains(prohibited));
        }
    }
}
