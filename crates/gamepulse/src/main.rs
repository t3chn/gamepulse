#![forbid(unsafe_code)]

mod acceptance;
mod covers;
mod observability;
mod runtime;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gamepulse_application::{
    HourlyJobSchedule, JobClaimPacing, JobHandler, JobHandlerRegistry, ReviewSummaryJobSchedule,
    RuntimeJobType, RuntimeJobTypeFilter, SourceIngestionJobSchedule, execute_cover_backfill,
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
            println!("{}", report.to_json());
            report.exit_code()
        }
        Err(_) => 1,
    }
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
