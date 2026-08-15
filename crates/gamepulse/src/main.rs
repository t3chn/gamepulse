#![forbid(unsafe_code)]

mod runtime;

use std::sync::{Arc, Mutex};

use gamepulse_application::{
    HourlyJobSchedule, JobHandler, JobHandlerRegistry, ReviewSummaryJobSchedule, RuntimeJobType,
    RuntimeJobTypeFilter, SourceIngestionJobSchedule,
};
use gamepulse_storage_sqlite::{
    SqliteDailyCrawlStateStore, SqliteGameCatalogueReadStore, SqliteJobStore,
    SqliteReviewSummaryStore,
};
use gamepulse_worker_llm::{LocalExtractiveReviewSummarizer, ReviewSummaryHandler};
use gamepulse_worker_source::{
    HourlyDiscoveryHandler, MetacriticCanaryClient, MetacriticDailyCrawlSource,
    MetacriticGameReviewSource, ReviewSourceIngestionHandler,
};
use runtime::{Runtime, RuntimeConfig, SystemRuntimeClock};
use tokio::sync::Notify;

#[tokio::main]
async fn main() {
    if run().await.is_err() {
        std::process::exit(1);
    }
}

/// The composition root owns concrete SQLite, clock, scheduler, and source-lane wiring.
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let database_path =
        std::env::var("GAMEPULSE_DATABASE_PATH").unwrap_or_else(|_| "gamepulse.sqlite3".to_owned());
    let store = Arc::new(Mutex::new(SqliteJobStore::open(&database_path)?));
    let daily_crawl_state = Arc::new(Mutex::new(SqliteDailyCrawlStateStore::open(
        &database_path,
    )?));
    let review_summaries = Arc::new(Mutex::new(SqliteReviewSummaryStore::open(&database_path)?));
    let catalogue = Arc::new(Mutex::new(SqliteGameCatalogueReadStore::open(
        &database_path,
    )?));
    let source_client = MetacriticCanaryClient::new()?;
    let source_port = MetacriticDailyCrawlSource::new(source_client.clone());
    let schedule = HourlyJobSchedule::new(RuntimeJobType::SourceHourlyDiscovery, 3)?;
    let source_ingestion_schedule = SourceIngestionJobSchedule::new(3)?;
    let review_summary_schedule = ReviewSummaryJobSchedule::new(3)?;
    let source_config = RuntimeConfig::new("gamepulse-source-runtime", 300, 2, schedule)?
        .with_claim_filter(RuntimeJobTypeFilter::source_lane());
    let llm_config = RuntimeConfig::worker_only("gamepulse-llm-runtime", 300, 1)?
        .with_claim_filter(RuntimeJobTypeFilter::llm_lane());
    let source_handler: Arc<dyn JobHandler> = Arc::new(HourlyDiscoveryHandler::new(
        daily_crawl_state,
        source_port,
        source_ingestion_schedule,
    ));
    let ingestion_handler: Arc<dyn JobHandler> = Arc::new(ReviewSourceIngestionHandler::new(
        review_summaries.clone(),
        MetacriticGameReviewSource::new(source_client),
        review_summary_schedule,
    ));
    let source_handlers = Arc::new(JobHandlerRegistry::new([
        source_handler,
        ingestion_handler,
    ])?);
    let llm_handler: Arc<dyn JobHandler> = Arc::new(ReviewSummaryHandler::new(
        review_summaries,
        LocalExtractiveReviewSummarizer,
    ));
    let llm_handlers = Arc::new(JobHandlerRegistry::new([llm_handler])?);
    let wakeup = Arc::new(Notify::new());
    let mut source_runtime = Runtime::new(
        store.clone(),
        Arc::new(SystemRuntimeClock),
        source_config,
        source_handlers,
    )
    .with_wakeup(wakeup.clone());
    let mut llm_runtime = Runtime::new(
        store,
        Arc::new(SystemRuntimeClock),
        llm_config,
        llm_handlers,
    )
    .with_wakeup(wakeup.clone());
    let http_address =
        std::env::var("GAMEPULSE_HTTP_ADDRESS").unwrap_or_else(|_| "127.0.0.1:3000".to_owned());
    let listener = tokio::net::TcpListener::bind(&http_address).await?;
    let web_server = axum::serve(listener, gamepulse_web::catalogue_router(catalogue))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        });

    let source_runtime_result = async {
        source_runtime
            .run_until_shutdown_with_wakeup(wakeup.clone(), async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
            .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })
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
    tokio::try_join!(source_runtime_result, llm_runtime_result, web_result)?;
    Ok(())
}
