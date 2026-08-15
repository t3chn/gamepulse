#![forbid(unsafe_code)]

mod runtime;

use std::sync::{Arc, Mutex};

use gamepulse_application::{HourlyJobSchedule, JobHandler, JobHandlerRegistry, RuntimeJobType};
use gamepulse_storage_sqlite::SqliteJobStore;
use gamepulse_worker_source::HourlyDiscoveryPlaceholderHandler;
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
    let store = Arc::new(Mutex::new(SqliteJobStore::open(database_path)?));
    let schedule = HourlyJobSchedule::new(RuntimeJobType::SourceHourlyDiscovery, 3)?;
    let config = RuntimeConfig::new("gamepulse-source-runtime", 300, 2, schedule)?;
    let source_handler: Arc<dyn JobHandler> = Arc::new(HourlyDiscoveryPlaceholderHandler);
    let handlers = Arc::new(JobHandlerRegistry::new([source_handler])?);
    let mut runtime = Runtime::new(store, Arc::new(SystemRuntimeClock), config, handlers);

    runtime
        .run_until_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
