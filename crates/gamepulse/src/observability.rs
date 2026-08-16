#![forbid(unsafe_code)]

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use gamepulse_application::{
    JobHandler, JobHandlerFuture, JobHandlerResult, ReviewSummaryRequest, RuntimeJobType, TypedJob,
};
use gamepulse_worker_source::{
    GameIdentity, OptionalPublicCoverEnricher, classify_source_ingestion_handler_failure,
};
use tracing_subscriber::filter;
use tracing_subscriber::layer::Layer;
use tracing_subscriber::prelude::*;

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
const GAMEPULSE_TRACING_TARGETS: [&str; 6] = [
    "gamepulse::lifecycle",
    "gamepulse::http",
    "gamepulse::scheduler",
    "gamepulse::durable",
    "gamepulse::source",
    "gamepulse::review_summary",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogFormat {
    Human,
    Json,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogConfigurationError;

impl LogFormat {
    pub fn parse(value: Option<&str>) -> Result<Self, LogConfigurationError> {
        match value {
            Some("human") => Ok(Self::Human),
            Some("json") => Ok(Self::Json),
            Some(_) | None => Err(LogConfigurationError),
        }
    }
}

/// Install the only process subscriber before any application adapter is composed.
pub fn initialize_subscriber(format: LogFormat) -> Result<(), LogConfigurationError> {
    let installed = match format {
        LogFormat::Human => tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .without_time()
                    .with_target(true)
                    .with_thread_ids(false)
                    .with_thread_names(false)
                    .with_filter(filter::filter_fn(is_gamepulse_event)),
            )
            .try_init(),
        LogFormat::Json => tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .flatten_event(true)
                    .with_ansi(false)
                    .without_time()
                    .with_current_span(false)
                    .with_span_list(false)
                    .with_target(true)
                    .with_filter(filter::filter_fn(is_gamepulse_event)),
            )
            .try_init(),
    };
    installed.map_err(|_| LogConfigurationError)
}

pub(crate) fn is_gamepulse_event(metadata: &tracing::Metadata<'_>) -> bool {
    metadata.level() <= &tracing::Level::INFO
        && GAMEPULSE_TRACING_TARGETS.contains(&metadata.target())
}

/// Fixed, safe values for one completed HTTP request event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HttpRequestLog {
    request_id: u64,
    method: &'static str,
    route: &'static str,
    status: u16,
    elapsed_ms: u64,
}

impl HttpRequestLog {
    pub(crate) fn new(
        request_id: u64,
        method: &'static str,
        route: &'static str,
        status: u16,
        elapsed_ms: u64,
    ) -> Self {
        Self {
            request_id,
            method,
            route,
            status,
            elapsed_ms,
        }
    }

    pub const fn request_id(self) -> u64 {
        self.request_id
    }

    pub const fn method(self) -> &'static str {
        self.method
    }

    pub const fn route(self) -> &'static str {
        self.route
    }

    pub const fn status(self) -> u16 {
        self.status
    }

    pub const fn elapsed_ms(self) -> u64 {
        self.elapsed_ms
    }
}

/// Binary-owned HTTP tracing middleware. It emits no request data other than normalized fields.
pub async fn trace_http_request(request: Request, next: Next) -> Response {
    let request_id = next_request_id();
    let method = normalized_method(request.method().as_str());
    let route = normalized_route(request.uri().path());
    let started = Instant::now();
    let response = next.run(request).await;
    emit_http_completed(HttpRequestLog::new(
        request_id,
        method,
        route,
        response.status().as_u16(),
        elapsed_millis(started.elapsed()),
    ));
    response
}

pub fn emit_http_completed(event: HttpRequestLog) {
    tracing::info!(
        target: "gamepulse::http",
        request_id = event.request_id(),
        method = event.method(),
        route = event.route(),
        status = event.status(),
        elapsed_ms = event.elapsed_ms(),
        "http request completed"
    );
}

pub fn process_started(source_work_enabled: bool) {
    tracing::info!(
        target: "gamepulse::lifecycle",
        source_work_enabled,
        "process started"
    );
}

pub fn source_work_disabled() {
    tracing::info!(
        target: "gamepulse::lifecycle",
        source_work_enabled = false,
        "source work disabled"
    );
}

pub fn process_stopped() {
    tracing::info!(target: "gamepulse::lifecycle", "process stopped");
}

/// Log source and summary handler categories without letting their opaque inputs enter events.
pub struct ObservedJobHandler<H> {
    inner: H,
}

impl<H> ObservedJobHandler<H> {
    pub fn new(inner: H) -> Self {
        Self { inner }
    }
}

impl<H> JobHandler for ObservedJobHandler<H>
where
    H: JobHandler,
{
    fn job_type(&self) -> RuntimeJobType {
        self.inner.job_type()
    }

    fn handle(&self, job: TypedJob) -> JobHandlerFuture {
        let job_type = job.job_type();
        let review_kind = review_kind_category(&job);
        let future = self.inner.handle(job);
        Box::pin(async move {
            let outcome = future.await;
            match job_type {
                RuntimeJobType::SourceHourlyDiscovery | RuntimeJobType::SourceGameIngestion => {
                    tracing::info!(
                        target: "gamepulse::source",
                        source_stage = source_stage_category(job_type),
                        outcome = handler_outcome_category(&outcome),
                        source_failure_category = source_failure_category(job_type, &outcome),
                        "source stage aggregate settled"
                    );
                }
                RuntimeJobType::LlmReviewSummary => {
                    tracing::info!(
                        target: "gamepulse::review_summary",
                        review_kind,
                        outcome = handler_outcome_category(&outcome),
                        "review summary settled"
                    );
                }
            }
            outcome
        })
    }
}

/// Composition-root wrapper for the M012 optional cover branch.
pub struct ObservedPublicCoverEnricher<E> {
    inner: E,
}

impl<E> ObservedPublicCoverEnricher<E> {
    pub fn new(inner: E) -> Self {
        Self { inner }
    }
}

impl<E> OptionalPublicCoverEnricher for ObservedPublicCoverEnricher<E>
where
    E: OptionalPublicCoverEnricher,
{
    type EnrichFuture<'a>
        = Pin<Box<dyn Future<Output = Option<gamepulse_domain::GamePublicCoverUrl>> + Send + 'a>>
    where
        Self: 'a;

    fn enrich(&self, expected: GameIdentity) -> Self::EnrichFuture<'_> {
        let future = self.inner.enrich(expected);
        Box::pin(async move {
            let cover = future.await;
            tracing::info!(
                target: "gamepulse::source",
                optional_cover = optional_cover_category(cover.is_some()),
                "optional cover settled"
            );
            cover
        })
    }
}

pub(crate) fn handler_outcome_category(outcome: &JobHandlerResult) -> &'static str {
    match outcome {
        JobHandlerResult::Succeeded => "succeeded",
        JobHandlerResult::Failed(_) => "failed",
    }
}

pub(crate) fn source_failure_category(
    job_type: RuntimeJobType,
    outcome: &JobHandlerResult,
) -> &'static str {
    match (job_type, outcome) {
        (RuntimeJobType::SourceGameIngestion, JobHandlerResult::Failed(failure)) => {
            classify_source_ingestion_handler_failure(failure.message()).as_str()
        }
        _ => "not_applicable",
    }
}

pub(crate) const fn source_stage_category(job_type: RuntimeJobType) -> &'static str {
    match job_type {
        RuntimeJobType::SourceHourlyDiscovery => "hourly_discovery",
        RuntimeJobType::SourceGameIngestion => "game_ingestion",
        RuntimeJobType::LlmReviewSummary => "not_source",
    }
}

pub(crate) const fn optional_cover_category(available: bool) -> &'static str {
    if available {
        "available"
    } else {
        "unavailable"
    }
}

pub(crate) fn review_kind_category(job: &TypedJob) -> &'static str {
    if job.job_type() != RuntimeJobType::LlmReviewSummary {
        return "not_applicable";
    }
    ReviewSummaryRequest::from_work_reference(job.work_ref())
        .map(|request| request.kind().as_str())
        .unwrap_or("invalid")
}

pub(crate) fn next_request_id() -> u64 {
    NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

fn normalized_method(value: &str) -> &'static str {
    match value {
        "GET" => "GET",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        _ => "OTHER",
    }
}

pub(crate) fn normalized_route(value: &str) -> &'static str {
    match value {
        "/health/live" => "/health/live",
        "/health/ready" => "/health/ready",
        "/games" => "/games",
        value if value.starts_with("/games/") => "/games/{id}",
        _ => "other",
    }
}

fn elapsed_millis(elapsed: Duration) -> u64 {
    elapsed.as_millis().try_into().unwrap_or(u64::MAX)
}
