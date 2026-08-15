#![forbid(unsafe_code)]

#[allow(dead_code)]
#[path = "../src/observability.rs"]
mod observability;
#[allow(dead_code)]
#[path = "../src/runtime.rs"]
mod runtime;

use std::fs;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use gamepulse_application::{
    JobFailureResult, JobHandlerFailure, JobHandlerResult, JobRecord, JobStatus, JobTimestamp,
    ReviewRefreshFingerprint, ReviewSummaryRequest, RuntimeJobType, SourceProductId, TypedJob,
};
use gamepulse_domain::ReviewKind;
use serde_json::Value;
use tracing_subscriber::filter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::Layer;
use tracing_subscriber::prelude::*;

const READINESS_ATTEMPTS: usize = 40;
const READINESS_DELAY: Duration = Duration::from_millis(100);
const CHILD_EXIT_DEADLINE: Duration = Duration::from_secs(5);
const CHILD_EXIT_POLL_DELAY: Duration = Duration::from_millis(10);
static NEXT_SMOKE_DIRECTORY: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Default)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

struct SharedWriterGuard(Arc<Mutex<Vec<u8>>>);

impl Write for SharedWriterGuard {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("test log buffer must not be poisoned")
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for SharedWriter {
    type Writer = SharedWriterGuard;

    fn make_writer(&'a self) -> Self::Writer {
        SharedWriterGuard(Arc::clone(&self.0))
    }
}

struct TemporarySmokeDirectory {
    path: std::path::PathBuf,
}

impl TemporarySmokeDirectory {
    fn new() -> Self {
        let sequence = NEXT_SMOKE_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "gamepulse-m014-observability-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("test smoke directory must be created exactly once");
        Self { path }
    }

    fn database_path(&self, format: &str) -> std::path::PathBuf {
        self.path.join(format!("gamepulse-{format}.sqlite3"))
    }

    fn log_path(&self, name: &str) -> std::path::PathBuf {
        self.path.join(name)
    }
}

impl Drop for TemporarySmokeDirectory {
    fn drop(&mut self) {
        for format in ["human", "json", "invalid"] {
            let database = self.database_path(format);
            let _ = fs::remove_file(&database);
            let _ = fs::remove_file(database.with_extension("sqlite3-shm"));
            let _ = fs::remove_file(database.with_extension("sqlite3-wal"));
        }
        for log in ["human.log", "json.log", "invalid.log"] {
            let _ = fs::remove_file(self.log_path(log));
        }
        let _ = fs::remove_dir(&self.path);
    }
}

struct ChildProcess {
    child: Child,
}

impl ChildProcess {
    fn wait_for_exit(&mut self, description: &str) -> std::process::ExitStatus {
        let deadline = Instant::now() + CHILD_EXIT_DEADLINE;

        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return status,
                Ok(None) if Instant::now() < deadline => thread::sleep(CHILD_EXIT_POLL_DELAY),
                Ok(None) => {
                    let kill_result = self.child.kill();
                    let reap_result = self.child.wait();
                    panic!(
                        "{description} did not exit within {CHILD_EXIT_DEADLINE:?}; forced cleanup result: kill={kill_result:?}, reap={reap_result:?}"
                    );
                }
                Err(error) => panic!("{description} child status check failed: {error}"),
            }
        }
    }

    fn graceful_shutdown(&mut self) {
        let signal = Command::new("/bin/kill")
            .arg("-INT")
            .arg(self.child.id().to_string())
            .status()
            .expect("SIGINT helper must start");
        assert!(signal.success(), "SIGINT helper must succeed");
        assert!(
            self.wait_for_exit("binary child after SIGINT").success(),
            "binary must exit cleanly after SIGINT"
        );
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn reserve_loopback_port() -> u16 {
    let listener =
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("test must reserve a loopback port");
    let port = listener
        .local_addr()
        .expect("reserved listener must have a local address")
        .port();
    drop(listener);
    port
}

fn spawn_binary(
    temporary: &TemporarySmokeDirectory,
    format: &str,
    port: u16,
    log_name: &str,
) -> ChildProcess {
    let log = fs::File::create(temporary.log_path(log_name))
        .expect("binary smoke log must be created in the exact temporary directory");
    let stdout = log
        .try_clone()
        .expect("binary smoke log must be cloneable for both process streams");
    let child = Command::new(env!("CARGO_BIN_EXE_gamepulse"))
        .env("GAMEPULSE_DATABASE_PATH", temporary.database_path(format))
        .env("GAMEPULSE_HTTP_ADDRESS", format!("127.0.0.1:{port}"))
        .env("GAMEPULSE_LOG_FORMAT", format)
        .env("GAMEPULSE_SOURCE_WORK_ENABLED", "false")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(log))
        .spawn()
        .expect("source-disabled production binary must start");
    ChildProcess { child }
}

fn http_status(port: u16, target: &str) -> io::Result<u16> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, READINESS_DELAY)?;
    stream.set_read_timeout(Some(READINESS_DELAY))?;
    stream.write_all(
        format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    response
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing HTTP status"))
}

fn wait_for_liveness(port: u16) {
    for attempt in 0..READINESS_ATTEMPTS {
        if matches!(http_status(port, "/health/live"), Ok(200)) {
            return;
        }
        if attempt + 1 < READINESS_ATTEMPTS {
            thread::sleep(READINESS_DELAY);
        }
    }
    panic!("binary did not become live within the bounded readiness window");
}

fn assert_safe_binary_output(format: &str, rendered: &str) {
    assert!(rendered.contains("source work disabled"));
    assert!(rendered.contains("process started"));
    assert!(rendered.contains("http request completed"));
    assert!(rendered.contains("process stopped"));
    assert!(!rendered.contains("private-title-search"));
    assert!(!rendered.contains("query-secret-value"));
    assert!(!rendered.contains("GAMEPULSE_DATABASE_PATH"));

    match format {
        "human" => assert!(rendered.contains("gamepulse::http")),
        "json" => {
            let events = rendered
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).expect("JSON log line must parse"))
                .collect::<Vec<_>>();
            assert!(events.iter().any(|event| {
                event["target"] == "gamepulse::http"
                    && event["route"] == "/games"
                    && event["status"] == 200
                    && event["request_id"].is_u64()
            }));
        }
        _ => panic!("test supports only documented log formats"),
    }
}

fn run_binary_smoke(temporary: &TemporarySmokeDirectory, format: &str) {
    let port = reserve_loopback_port();
    let log_name = format!("{format}.log");
    let mut child = spawn_binary(temporary, format, port, &log_name);
    wait_for_liveness(port);
    assert_eq!(
        http_status(port, "/health/live").expect("liveness request must return an HTTP status"),
        200
    );
    assert_eq!(
        http_status(port, "/health/ready").expect("readiness request must return an HTTP status"),
        200
    );
    assert_eq!(
        http_status(
            port,
            "/games?title=private-title-search&token=query-secret-value",
        )
        .expect("catalogue request must return an HTTP status"),
        200
    );
    child.graceful_shutdown();
    let rendered = fs::read_to_string(temporary.log_path(&log_name))
        .expect("binary smoke log must be readable after shutdown");
    assert_safe_binary_output(format, &rendered);
}

fn summary_job(kind: ReviewKind) -> TypedJob {
    let fingerprint =
        ReviewRefreshFingerprint::parse("a".repeat(64)).expect("test fingerprint must be valid");
    let request = ReviewSummaryRequest::new(
        SourceProductId::new(1).expect("test source product ID must be valid"),
        kind,
        fingerprint,
    );
    let timestamp = JobTimestamp::new(0).expect("test timestamp must be valid");
    TypedJob::from_record(&JobRecord::restored(
        "test-summary".to_owned(),
        RuntimeJobType::LlmReviewSummary.as_str().to_owned(),
        request.work_reference(),
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
    .expect("test record must be a typed job")
}

#[test]
fn logging_configuration_is_explicit_and_invalid_values_fail_closed() {
    assert_eq!(
        observability::LogFormat::parse(Some("human")),
        Ok(observability::LogFormat::Human)
    );
    assert_eq!(
        observability::LogFormat::parse(Some("json")),
        Ok(observability::LogFormat::Json)
    );
    assert!(observability::LogFormat::parse(None).is_err());
    assert!(observability::LogFormat::parse(Some("json secret=value")).is_err());
}

#[test]
fn route_normalization_redacts_paths_and_request_ids_correlate_events() {
    assert_eq!(observability::normalized_route("/games"), "/games");
    assert_eq!(
        observability::normalized_route("/games/a private title"),
        "/games/{id}"
    );
    assert_eq!(
        observability::normalized_route("/private?token=not-for-logs"),
        "other"
    );

    let first = observability::next_request_id();
    let second = observability::next_request_id();
    assert_ne!(first, second);
    assert_eq!(second, first + 1);
}

#[test]
fn json_http_event_has_safe_correlated_fields_without_raw_request_data() {
    let output = SharedWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_ansi(false)
        .without_time()
        .with_current_span(false)
        .with_span_list(false)
        .with_target(true)
        .with_writer(output.clone())
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    observability::emit_http_completed(observability::HttpRequestLog::new(
        41,
        "GET",
        "/games/{id}",
        200,
        7,
    ));
    drop(guard);

    let payload = output
        .0
        .lock()
        .expect("test log buffer must not be poisoned")
        .clone();
    let event: Value = serde_json::from_slice(&payload).expect("event must be valid JSON");
    assert_eq!(event["request_id"], 41);
    assert_eq!(event["method"], "GET");
    assert_eq!(event["route"], "/games/{id}");
    assert_eq!(event["status"], 200);
    assert_eq!(event["elapsed_ms"], 7);
    let rendered = String::from_utf8(payload).expect("event must be UTF-8");
    assert!(!rendered.contains("private title"));
    assert!(!rendered.contains("token="));
}

#[test]
fn source_cover_and_review_outcome_categories_are_fixed_and_text_free() {
    assert_eq!(
        observability::source_stage_category(RuntimeJobType::SourceHourlyDiscovery),
        "hourly_discovery"
    );
    assert_eq!(
        observability::source_stage_category(RuntimeJobType::SourceGameIngestion),
        "game_ingestion"
    );
    assert_eq!(observability::optional_cover_category(true), "available");
    assert_eq!(observability::optional_cover_category(false), "unavailable");
    assert_eq!(
        observability::handler_outcome_category(&JobHandlerResult::Succeeded),
        "succeeded"
    );
    assert_eq!(
        observability::handler_outcome_category(&JobHandlerResult::Failed(JobHandlerFailure::new(
            "review text must never be logged"
        ))),
        "failed"
    );
    assert_eq!(
        observability::review_kind_category(&summary_job(ReviewKind::Critic)),
        "critic"
    );
    assert_eq!(
        observability::review_kind_category(&summary_job(ReviewKind::User)),
        "user"
    );
    assert_eq!(
        runtime::runtime_outcome_category(runtime::RuntimeTaskOutcome::Succeeded),
        "succeeded"
    );
    assert_eq!(
        runtime::runtime_outcome_category(runtime::RuntimeTaskOutcome::Failed(
            JobFailureResult::ReadyForRetry
        )),
        "retryable_failure"
    );
    assert_eq!(
        runtime::runtime_outcome_category(runtime::RuntimeTaskOutcome::Failed(
            JobFailureResult::Failed
        )),
        "terminal_failure"
    );
}

#[test]
fn exact_target_allowlist_excludes_foreign_warning_payloads_in_human_and_json() {
    let human_output = SharedWriter::default();
    let human_subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .without_time()
            .with_target(true)
            .with_writer(human_output.clone())
            .with_filter(filter::filter_fn(observability::is_gamepulse_event)),
    );
    let human_guard = tracing::subscriber::set_default(human_subscriber);
    tracing::warn!(
        target: "foreign::dependency",
        "foreign error https://unsafe.example/private?secret=foreign-value"
    );
    tracing::info!(target: "gamepulse::http", "owned event remains visible");
    drop(human_guard);

    let human = String::from_utf8(
        human_output
            .0
            .lock()
            .expect("human test log buffer must not be poisoned")
            .clone(),
    )
    .expect("human log must be UTF-8");
    assert!(human.contains("owned event remains visible"));
    assert!(!human.contains("unsafe.example"));
    assert!(!human.contains("foreign-value"));

    let json_output = SharedWriter::default();
    let json_subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .json()
            .flatten_event(true)
            .with_ansi(false)
            .without_time()
            .with_current_span(false)
            .with_span_list(false)
            .with_target(true)
            .with_writer(json_output.clone())
            .with_filter(filter::filter_fn(observability::is_gamepulse_event)),
    );
    let json_guard = tracing::subscriber::set_default(json_subscriber);
    tracing::warn!(
        target: "foreign::dependency",
        "foreign error https://unsafe.example/private?secret=foreign-value"
    );
    tracing::info!(target: "gamepulse::http", "owned event remains visible");
    drop(json_guard);

    let json = String::from_utf8(
        json_output
            .0
            .lock()
            .expect("JSON test log buffer must not be poisoned")
            .clone(),
    )
    .expect("JSON log must be UTF-8");
    assert!(json.contains("owned event remains visible"));
    assert!(!json.contains("unsafe.example"));
    assert!(!json.contains("foreign-value"));
}

#[test]
fn actual_binary_initializer_smoke_is_loopback_only_and_fails_closed() {
    let temporary = TemporarySmokeDirectory::new();
    run_binary_smoke(&temporary, "human");
    run_binary_smoke(&temporary, "json");

    let invalid_log = fs::File::create(temporary.log_path("invalid.log"))
        .expect("invalid-config log must be created in the exact temporary directory");
    let invalid_stdout = invalid_log
        .try_clone()
        .expect("invalid-config log must be cloneable for both process streams");
    let mut invalid_child = ChildProcess {
        child: Command::new(env!("CARGO_BIN_EXE_gamepulse"))
            .env(
                "GAMEPULSE_DATABASE_PATH",
                temporary.database_path("invalid"),
            )
            .env(
                "GAMEPULSE_HTTP_ADDRESS",
                format!("127.0.0.1:{}", reserve_loopback_port()),
            )
            .env("GAMEPULSE_LOG_FORMAT", "invalid-log-config-secret")
            .env("GAMEPULSE_SOURCE_WORK_ENABLED", "false")
            .stdin(Stdio::null())
            .stdout(Stdio::from(invalid_stdout))
            .stderr(Stdio::from(invalid_log))
            .spawn()
            .expect("invalid-config binary child must start"),
    };
    let invalid_status = invalid_child.wait_for_exit("invalid-config binary child");
    assert_eq!(invalid_status.code(), Some(1));
    assert!(
        fs::read(temporary.log_path("invalid.log"))
            .expect("invalid-config log must be readable")
            .is_empty(),
        "invalid logging configuration must not echo its value"
    );
}
