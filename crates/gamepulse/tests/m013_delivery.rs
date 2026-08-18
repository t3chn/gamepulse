#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::to_bytes;
use axum::http::{Request, StatusCode};
use gamepulse_application::ServiceReadinessPort;
use gamepulse_storage_sqlite::{SqliteJobStore, SqliteReadinessProbe};
use tower::ServiceExt;

static NEXT_DATABASE: AtomicU64 = AtomicU64::new(0);

struct TemporaryDatabase {
    path: PathBuf,
}

impl TemporaryDatabase {
    fn new() -> Self {
        let sequence = NEXT_DATABASE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "gamepulse-m013-delivery-{}-{sequence}.sqlite3",
            std::process::id()
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

async fn response_status_and_body(response: axum::response::Response) -> (StatusCode, Vec<u8>) {
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body must collect")
        .to_vec();
    (status, body)
}

#[tokio::test]
async fn liveness_needs_no_store_and_readiness_is_safe_for_missing_or_unmigrated_sqlite() {
    let (status, body) = response_status_and_body(gamepulse_web::liveness_response().await).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());

    let missing_database = TemporaryDatabase::new();
    let missing_probe = SqliteReadinessProbe::new(&missing_database.path);
    assert!(missing_probe.check_readiness().is_err());
    let (status, body) =
        response_status_and_body(gamepulse_web::readiness_response(&missing_probe).await).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.is_empty());

    let unmigrated_database = TemporaryDatabase::new();
    let _connection = rusqlite::Connection::open(&unmigrated_database.path)
        .expect("empty test database must open");
    let unmigrated_probe = SqliteReadinessProbe::new(&unmigrated_database.path);
    assert!(unmigrated_probe.check_readiness().is_err());
    let (status, body) =
        response_status_and_body(gamepulse_web::readiness_response(&unmigrated_probe).await).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.is_empty());
}

#[tokio::test]
async fn readiness_rejects_a_structurally_incomplete_database_claiming_schema_version_five() {
    let incomplete_database = TemporaryDatabase::new();
    let connection = rusqlite::Connection::open(&incomplete_database.path)
        .expect("incomplete test database must open");
    connection
        .pragma_update(None, "user_version", 5_i64)
        .expect("test database must claim schema version five");
    drop(connection);

    let probe = SqliteReadinessProbe::new(&incomplete_database.path);
    assert!(probe.check_readiness().is_err());
    let (status, body) =
        response_status_and_body(gamepulse_web::readiness_response(&probe).await).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.is_empty());
}

#[tokio::test]
async fn readiness_requires_the_current_sqlite_schema_and_returns_no_operational_detail() {
    let database = TemporaryDatabase::new();
    let _store = SqliteJobStore::open(&database.path).expect("test database must migrate");
    let probe = SqliteReadinessProbe::new(&database.path);

    assert!(probe.check_readiness().is_ok());
    let (status, body) =
        response_status_and_body(gamepulse_web::readiness_response(&probe).await).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());
}

#[tokio::test]
async fn unavailable_cover_handler_has_the_same_503_contract_as_other_database_routes() {
    let router =
        gamepulse_web::unavailable_service_router(std::sync::Arc::new(UnavailableReadiness));
    for uri in ["/games", "/games/101", "/games/101/cover"] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(axum::body::Body::empty())
                    .expect("test request must build"),
            )
            .await
            .expect("unavailable router must respond");
        let (status, body) = response_status_and_body(response).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "route {uri}");
        assert!(body.is_empty(), "route {uri}");
    }
}

struct UnavailableReadiness;

impl ServiceReadinessPort for UnavailableReadiness {
    type Error = ();

    fn check_readiness(&self) -> Result<(), Self::Error> {
        Err(())
    }
}
