#![forbid(unsafe_code)]

#[allow(dead_code)]
#[path = "../src/runtime.rs"]
mod runtime;

use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use gamepulse_application::{
    HourlyJobSchedule, JobHandler, JobHandlerRegistry, JobRequest, JobStatus, JobStore,
    JobTimestamp, ReviewKind, ReviewSummaryJobSchedule, RuntimeJobType, RuntimeJobTypeFilter,
};
use gamepulse_storage_sqlite::{SqliteJobStore, SqliteReviewSummaryStore};
use gamepulse_worker_source::{
    GameDetail, GameIdentity, GameIngestionTransport, MetacriticGameReviewSource, PlatformDetail,
    PlatformUserScore, PublicHtmlCoverEnricher, PublicHtmlCoverTransport, PublicHtmlResponse,
    ReviewPage, ReviewSourceIngestionHandler, parse_game_detail,
    parse_platform_user_score_for_snapshot, parse_review_page,
};
use runtime::{Runtime, RuntimeClock, RuntimeClockError, RuntimeConfig, RuntimeTaskOutcome};

const DETAIL: &str =
    include_str!("../../gamepulse-worker-source/tests/fixtures/product-detail.json");
const USER_SCORE: &str =
    include_str!("../../gamepulse-worker-source/tests/fixtures/user-score.json");
const CRITIC_REVIEWS: &str =
    include_str!("../../gamepulse-worker-source/tests/fixtures/m011-critic-review-page.json");
const USER_REVIEWS: &str =
    include_str!("../../gamepulse-worker-source/tests/fixtures/m011-user-review-page.json");

static NEXT_DATABASE: AtomicU64 = AtomicU64::new(0);

struct TemporaryDatabase {
    path: PathBuf,
}

impl TemporaryDatabase {
    fn new() -> Self {
        let sequence = NEXT_DATABASE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "gamepulse-m012-public-cover-settlement-{}-{sequence}.sqlite3",
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

#[derive(Clone, Copy)]
struct FixedClock(i64);

impl RuntimeClock for FixedClock {
    fn now(&self) -> Result<JobTimestamp, RuntimeClockError> {
        JobTimestamp::new(self.0).map_err(|_| RuntimeClockError::Overflow)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FixtureError {
    Unavailable,
}

#[derive(Clone, Default)]
struct FixtureGameTransport;

impl GameIngestionTransport for FixtureGameTransport {
    type Error = FixtureError;
    type FetchDetailFuture<'a>
        = Pin<Box<dyn Future<Output = Result<GameDetail, Self::Error>> + Send + 'a>>
    where
        Self: 'a;
    type FetchPlatformUserScoreFuture<'a>
        = Pin<Box<dyn Future<Output = Result<PlatformUserScore, Self::Error>> + Send + 'a>>
    where
        Self: 'a;
    type FetchReviewPageFuture<'a>
        = Pin<Box<dyn Future<Output = Result<ReviewPage, Self::Error>> + Send + 'a>>
    where
        Self: 'a;

    fn fetch_game_detail(&self, expected: GameIdentity) -> Self::FetchDetailFuture<'_> {
        Box::pin(async move {
            parse_game_detail(&expected, DETAIL).map_err(|_| FixtureError::Unavailable)
        })
    }

    fn fetch_platform_user_score(
        &self,
        expected_game: GameIdentity,
        expected_platform: PlatformDetail,
    ) -> Self::FetchPlatformUserScoreFuture<'_> {
        let body = if expected_platform.slug == "pc" {
            USER_SCORE.to_owned()
        } else {
            USER_SCORE
                .replace("/platform/pc/", "/platform/console/")
                .replace("\"score\": 8.4", "\"score\": null")
        };
        Box::pin(async move {
            parse_platform_user_score_for_snapshot(&expected_game, &expected_platform, &body)
                .map_err(|_| FixtureError::Unavailable)
        })
    }

    fn fetch_review_page(
        &self,
        expected_game: GameIdentity,
        kind: ReviewKind,
        offset: u32,
        limit: u32,
    ) -> Self::FetchReviewPageFuture<'_> {
        let body = match kind {
            ReviewKind::Critic => CRITIC_REVIEWS,
            ReviewKind::User => USER_REVIEWS,
        };
        let slug = expected_game.slug;
        Box::pin(async move {
            parse_review_page(kind, &slug, offset, limit, body)
                .map_err(|_| FixtureError::Unavailable)
        })
    }
}

#[derive(Clone, Default)]
struct PendingHtmlTransport {
    calls: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}

struct PendingHtmlFetch {
    drops: Arc<AtomicUsize>,
}

impl Future for PendingHtmlFetch {
    type Output = Result<PublicHtmlResponse, FixtureError>;

    fn poll(self: Pin<&mut Self>, _context: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        Poll::Pending
    }
}

impl Drop for PendingHtmlFetch {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

impl PublicHtmlCoverTransport for PendingHtmlTransport {
    type Error = FixtureError;
    type Response = PublicHtmlResponse;
    type FetchFuture<'a>
        = PendingHtmlFetch
    where
        Self: 'a;

    fn fetch_public_game_html(&self, _expected: GameIdentity) -> Self::FetchFuture<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        PendingHtmlFetch {
            drops: Arc::clone(&self.drops),
        }
    }
}

fn timestamp(value: i64) -> JobTimestamp {
    JobTimestamp::new(value).expect("test timestamp must be valid")
}

fn source_config() -> RuntimeConfig {
    RuntimeConfig::new(
        "m012-source-worker",
        300,
        1,
        HourlyJobSchedule::new(RuntimeJobType::SourceHourlyDiscovery, 1)
            .expect("test schedule must be valid"),
    )
    .expect("test runtime config must be valid")
    .with_claim_filter(RuntimeJobTypeFilter::source_lane())
}

#[tokio::test]
async fn a_pending_optional_cover_is_cancelled_before_the_mandatory_job_lease_and_persists_none() {
    let database = TemporaryDatabase::new();
    let queue = Arc::new(Mutex::new(
        SqliteJobStore::open(&database.path).expect("queue database must open"),
    ));
    let reviews = Arc::new(Mutex::new(
        SqliteReviewSummaryStore::open(&database.path).expect("review database must open"),
    ));
    let html_transport = PendingHtmlTransport::default();
    queue
        .lock()
        .expect("queue must not be poisoned")
        .enqueue(
            JobRequest::new(
                "m012-public-cover-pending",
                RuntimeJobType::SourceGameIngestion.as_str(),
                "metacritic-game:101:example-game",
                1,
                timestamp(10),
            )
            .expect("source job must be valid"),
        )
        .expect("source job must enqueue");
    let source_handler: Arc<dyn JobHandler> = Arc::new(ReviewSourceIngestionHandler::new(
        reviews.clone(),
        MetacriticGameReviewSource::with_public_cover_enricher(
            FixtureGameTransport,
            PublicHtmlCoverEnricher::new(html_transport.clone()),
        ),
        ReviewSummaryJobSchedule::new(1).expect("summary schedule must be valid"),
    ));
    let mut runtime = Runtime::new(
        queue.clone(),
        Arc::new(FixedClock(20)),
        source_config(),
        Arc::new(JobHandlerRegistry::new([source_handler]).expect("source registry must be valid")),
    );

    assert_eq!(
        runtime
            .dispatch_available()
            .expect("source job must claim")
            .claimed,
        1
    );
    assert_eq!(
        runtime
            .join_all()
            .await
            .expect("source task must join")
            .settled,
        [RuntimeTaskOutcome::Succeeded]
    );
    assert_eq!(
        queue
            .lock()
            .expect("queue must not be poisoned")
            .job("m012-public-cover-pending")
            .expect("source job must load")
            .expect("source job must exist")
            .status(),
        JobStatus::Succeeded
    );
    assert_eq!(html_transport.calls.load(Ordering::SeqCst), 1);
    assert_eq!(html_transport.drops.load(Ordering::SeqCst), 1);

    drop(runtime);
    drop(queue);
    drop(reviews);
    let connection = rusqlite::Connection::open(&database.path).expect("database must reopen");
    assert_eq!(
        connection
            .query_row(
                "SELECT public_cover_url FROM games WHERE source_product_id = 101",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .expect("settled snapshot must persist"),
        None
    );
}
