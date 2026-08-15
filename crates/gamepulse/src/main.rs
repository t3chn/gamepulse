#![forbid(unsafe_code)]

mod runtime;

use std::sync::{Arc, Mutex};

use gamepulse_application::{
    HourlyJobSchedule, JobHandler, JobHandlerRegistry, RuntimeJobType, SourceIngestionJobSchedule,
};
use gamepulse_storage_sqlite::{
    SqliteDailyCrawlStateStore, SqliteGameCatalogueReadStore, SqliteGameSnapshotStore,
    SqliteJobStore,
};
use gamepulse_worker_source::{
    HourlyDiscoveryHandler, MetacriticCanaryClient, MetacriticDailyCrawlSource,
    MetacriticGameIngestionSource, SourceIngestionHandler,
};
use runtime::{Runtime, RuntimeConfig, SystemRuntimeClock};

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
    let game_snapshots = Arc::new(Mutex::new(SqliteGameSnapshotStore::open(&database_path)?));
    let catalogue = Arc::new(Mutex::new(SqliteGameCatalogueReadStore::open(
        &database_path,
    )?));
    let source_client = MetacriticCanaryClient::new()?;
    let source_port = MetacriticDailyCrawlSource::new(source_client.clone());
    let schedule = HourlyJobSchedule::new(RuntimeJobType::SourceHourlyDiscovery, 3)?;
    let source_ingestion_schedule = SourceIngestionJobSchedule::new(3)?;
    let config = RuntimeConfig::new("gamepulse-source-runtime", 300, 2, schedule)?;
    let source_handler: Arc<dyn JobHandler> = Arc::new(HourlyDiscoveryHandler::new(
        daily_crawl_state,
        source_port,
        source_ingestion_schedule,
    ));
    let ingestion_handler: Arc<dyn JobHandler> = Arc::new(SourceIngestionHandler::new(
        game_snapshots,
        MetacriticGameIngestionSource::new(source_client),
    ));
    let handlers = Arc::new(JobHandlerRegistry::new([
        source_handler,
        ingestion_handler,
    ])?);
    let mut runtime = Runtime::new(store, Arc::new(SystemRuntimeClock), config, handlers);
    let http_address =
        std::env::var("GAMEPULSE_HTTP_ADDRESS").unwrap_or_else(|_| "127.0.0.1:3000".to_owned());
    let listener = tokio::net::TcpListener::bind(&http_address).await?;
    let web_server = axum::serve(listener, gamepulse_web::catalogue_router(catalogue))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        });

    let runtime_result = async {
        runtime
            .run_until_shutdown(async {
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
    tokio::try_join!(runtime_result, web_result)?;
    Ok(())
}
