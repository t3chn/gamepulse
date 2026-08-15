#![forbid(unsafe_code)]

mod runtime;

use std::sync::{Arc, Mutex};

use gamepulse_application::{HourlyJobSchedule, JobHandler, JobHandlerRegistry, RuntimeJobType};
use gamepulse_storage_sqlite::{SqliteDailyCrawlStateStore, SqliteJobStore};
use gamepulse_worker_source::{
    HourlyDiscoveryHandler, MetacriticCanaryClient, MetacriticDailyCrawlSource,
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
    let source_port = MetacriticDailyCrawlSource::new(MetacriticCanaryClient::new()?);
    let schedule = HourlyJobSchedule::new(RuntimeJobType::SourceHourlyDiscovery, 3)?;
    let config = RuntimeConfig::new("gamepulse-source-runtime", 300, 2, schedule)?;
    let source_handler: Arc<dyn JobHandler> =
        Arc::new(HourlyDiscoveryHandler::new(daily_crawl_state, source_port));
    let handlers = Arc::new(JobHandlerRegistry::new([source_handler])?);
    let mut runtime = Runtime::new(store, Arc::new(SystemRuntimeClock), config, handlers);

    runtime
        .run_until_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
