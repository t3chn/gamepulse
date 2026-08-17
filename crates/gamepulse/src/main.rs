#![forbid(unsafe_code)]

mod observability;
mod runtime;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gamepulse_application::{
    HourlyJobSchedule, JobClaimPacing, JobHandler, JobHandlerRegistry, ReviewSummaryJobSchedule,
    RuntimeJobType, RuntimeJobTypeFilter, SourceIngestionJobSchedule,
};
use gamepulse_storage_sqlite::{
    SqliteDailyCrawlStateStore, SqliteGameCatalogueReadStore, SqliteJobStore, SqliteReadinessProbe,
    SqliteReviewSummaryStore,
};
use gamepulse_worker_llm::{LocalExtractiveReviewSummarizer, ReviewSummaryHandler};
use gamepulse_worker_source::{
    HourlyDiscoveryHandler, MetacriticCanaryClient, MetacriticDailyCrawlSource,
    MetacriticGameReviewSource, MetacriticPublicHtmlTransport, PublicHtmlCoverEnricher,
    ReviewSourceIngestionHandler,
};
use observability::{LogFormat, ObservedJobHandler, ObservedPublicCoverEnricher};
use runtime::{Runtime, RuntimeConfig, SystemRuntimeClock};
use tokio::sync::Notify;

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
    daily_crawl_state: Arc<Mutex<SqliteDailyCrawlStateStore>>,
    review_summaries: Arc<Mutex<SqliteReviewSummaryStore>>,
    catalogue: Arc<Mutex<SqliteGameCatalogueReadStore>>,
}

impl RuntimeStorage {
    fn open(path: impl AsRef<Path>) -> Result<Self, Box<dyn std::error::Error>> {
        let path = path.as_ref();
        Ok(Self {
            store: Arc::new(Mutex::new(SqliteJobStore::open(path)?)),
            daily_crawl_state: Arc::new(Mutex::new(SqliteDailyCrawlStateStore::open(path)?)),
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
    if run().await.is_err() {
        std::process::exit(1);
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
    let review_summary_schedule = ReviewSummaryJobSchedule::new(3)?;
    let llm_config = RuntimeConfig::worker_only("gamepulse-llm-runtime", 300, 1)?
        .with_claim_filter(RuntimeJobTypeFilter::llm_lane());
    let llm_handler: Arc<dyn JobHandler> =
        Arc::new(ObservedJobHandler::new(ReviewSummaryHandler::new(
            storage.review_summaries.clone(),
            LocalExtractiveReviewSummarizer,
        )));
    let llm_handlers = Arc::new(JobHandlerRegistry::new([llm_handler])?);
    let wakeup = Arc::new(Notify::new());
    let mut source_runtime = if environment.source_work_enabled {
        let source_client = MetacriticCanaryClient::new()?;
        let public_html_cover = ObservedPublicCoverEnricher::new(PublicHtmlCoverEnricher::new(
            MetacriticPublicHtmlTransport::new()?,
        ));
        let source_port = MetacriticDailyCrawlSource::new(source_client.clone());
        let schedule = HourlyJobSchedule::new(RuntimeJobType::SourceHourlyDiscovery, 3)?;
        let source_ingestion_schedule = SourceIngestionJobSchedule::new(3)?;
        let source_config = RuntimeConfig::new("gamepulse-source-runtime", 300, 2, schedule)?
            .with_claim_filter(RuntimeJobTypeFilter::source_lane())
            .with_claim_pacing(JobClaimPacing::new(
                "source",
                SOURCE_LANE_MINIMUM_CLAIM_INTERVAL_SECONDS,
            )?);
        let source_handler: Arc<dyn JobHandler> =
            Arc::new(ObservedJobHandler::new(HourlyDiscoveryHandler::new(
                storage.daily_crawl_state,
                source_port,
                source_ingestion_schedule,
            )));
        let ingestion_handler: Arc<dyn JobHandler> =
            Arc::new(ObservedJobHandler::new(ReviewSourceIngestionHandler::new(
                storage.review_summaries.clone(),
                MetacriticGameReviewSource::with_public_cover_enricher(
                    source_client,
                    public_html_cover,
                ),
                review_summary_schedule,
            )));
        let source_handlers = Arc::new(JobHandlerRegistry::new([
            source_handler,
            ingestion_handler,
        ])?);
        Some(
            Runtime::new(
                storage.store.clone(),
                Arc::new(SystemRuntimeClock),
                source_config,
                source_handlers,
            )
            .with_wakeup(wakeup.clone()),
        )
    } else {
        observability::source_work_disabled();
        None
    };
    let mut llm_runtime = Runtime::new(
        storage.store,
        Arc::new(SystemRuntimeClock),
        llm_config,
        llm_handlers,
    )
    .with_wakeup(wakeup.clone());
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
