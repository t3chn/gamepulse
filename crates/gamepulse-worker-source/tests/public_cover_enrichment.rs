#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use gamepulse_application::{AsyncReviewSourceIngestionPort, SourceIngestionRequest};
use gamepulse_worker_source::{
    GameDetail, GameIdentity, GameIngestionTransport, MetacriticGameReviewSource,
    OptionalPublicCoverEnricher, PlatformDetail, PlatformUserScore, PublicHtmlCoverEnricher,
    PublicHtmlCoverResponse, PublicHtmlCoverTransport, ReviewKind, ReviewPage, parse_game_detail,
    parse_platform_user_score_for_snapshot, parse_review_page,
};

const DETAIL: &str = include_str!("fixtures/product-detail.json");
const USER_SCORE: &str = include_str!("fixtures/user-score.json");
const CRITIC_REVIEWS: &str = include_str!("fixtures/m011-critic-review-page.json");
const USER_REVIEWS: &str = include_str!("fixtures/m011-user-review-page.json");
const PUBLIC_COVER_VALID: &str = include_str!("fixtures/public-cover/valid.html");
const PUBLIC_COVER_CONTEXTS: &str = include_str!("fixtures/public-cover/contexts.html");
const PUBLIC_COVER_TEMPLATE: &str = include_str!("fixtures/public-cover/template.html");
const PUBLIC_COVER_BODY_ONLY: &str = include_str!("fixtures/public-cover/body-only.html");
const PUBLIC_COVER_DUPLICATE_PROPERTY: &str =
    include_str!("fixtures/public-cover/duplicate-property.html");
const PUBLIC_COVER_DUPLICATE_CONTENT: &str =
    include_str!("fixtures/public-cover/duplicate-content.html");
const PUBLIC_COVER_MALFORMED: &str = include_str!("fixtures/public-cover/malformed.html");
const PUBLIC_COVER_ENTITIES: &str = include_str!("fixtures/public-cover/entities.html");
const PUBLIC_COVER_ZERO: &str = include_str!("fixtures/public-cover/zero.html");
const PUBLIC_COVER_MULTIPLE: &str = include_str!("fixtures/public-cover/multiple.html");
const PUBLIC_COVER_DEPTH_LIMIT: &str = include_str!("fixtures/public-cover/depth-limit.html");
const PUBLIC_COVER_ATTRIBUTE_LIMIT: &str =
    include_str!("fixtures/public-cover/attribute-limit.html");
const PUBLIC_COVER_NODE_LIMIT_TEMPLATE: &str =
    include_str!("fixtures/public-cover/node-limit-template.html");
const PUBLIC_COVER_NODE_FRAGMENT: &str = include_str!("fixtures/public-cover/node-fragment.html");

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

#[derive(Clone)]
struct FixtureHtmlTransport {
    responses: Arc<Mutex<VecDeque<Result<FixtureHtmlResponse, FixtureError>>>>,
    calls: Arc<Mutex<usize>>,
}

impl FixtureHtmlTransport {
    fn new(responses: impl IntoIterator<Item = Result<FixtureHtmlResponse, FixtureError>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into_iter().collect())),
            calls: Arc::new(Mutex::new(0)),
        }
    }

    fn calls(&self) -> usize {
        *self
            .calls
            .lock()
            .expect("fixture calls must not be poisoned")
    }
}

impl PublicHtmlCoverTransport for FixtureHtmlTransport {
    type Error = FixtureError;
    type Response = FixtureHtmlResponse;
    type FetchFuture<'a>
        = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'a>>
    where
        Self: 'a;

    fn fetch_public_game_html(&self, _expected: GameIdentity) -> Self::FetchFuture<'_> {
        let response = self
            .responses
            .lock()
            .expect("fixture responses must not be poisoned")
            .pop_front()
            .unwrap_or(Err(FixtureError::Unavailable));
        *self
            .calls
            .lock()
            .expect("fixture calls must not be poisoned") += 1;
        Box::pin(async move { response })
    }
}

struct FixtureHtmlResponse {
    status: u16,
    content_type: Option<String>,
    content_length: Option<u64>,
    body: Option<Result<String, FixtureError>>,
    body_reads: Arc<AtomicUsize>,
}

impl FixtureHtmlResponse {
    fn html(status: u16, body: impl Into<String>) -> Self {
        let body = body.into();
        Self {
            status,
            content_type: Some("text/html; charset=utf-8".to_owned()),
            content_length: u64::try_from(body.len()).ok(),
            body: Some(Ok(body)),
            body_reads: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn body_read_failure(status: u16) -> Self {
        Self {
            status,
            content_type: Some("text/html; charset=utf-8".to_owned()),
            content_length: Some(1),
            body: Some(Err(FixtureError::Unavailable)),
            body_reads: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl PublicHtmlCoverResponse for FixtureHtmlResponse {
    type ReadBodyError = FixtureError;
    type ReadBodyFuture<'a>
        = std::future::Ready<Result<String, Self::ReadBodyError>>
    where
        Self: 'a;

    fn status(&self) -> u16 {
        self.status
    }

    fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    fn content_length(&self) -> Option<u64> {
        self.content_length
    }

    fn read_body(&mut self) -> Self::ReadBodyFuture<'_> {
        self.body_reads.fetch_add(1, Ordering::SeqCst);
        std::future::ready(self.body.take().unwrap_or(Err(FixtureError::Unavailable)))
    }
}

#[derive(Clone, Default)]
struct PendingHtmlTransport {
    calls: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}

impl PendingHtmlTransport {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn drops(&self) -> usize {
        self.drops.load(Ordering::SeqCst)
    }
}

struct PendingHtmlFetch {
    drops: Arc<AtomicUsize>,
}

impl Future for PendingHtmlFetch {
    type Output = Result<FixtureHtmlResponse, FixtureError>;

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
    type Response = FixtureHtmlResponse;
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

fn request() -> SourceIngestionRequest {
    SourceIngestionRequest::new(101, "example-game").expect("fixture request must be valid")
}

fn html(status: u16, body: impl Into<String>) -> FixtureHtmlResponse {
    FixtureHtmlResponse::html(status, body)
}

async fn ingest(
    html_transport: FixtureHtmlTransport,
) -> gamepulse_application::ReviewSourceIngestion {
    let source = MetacriticGameReviewSource::with_public_cover_enricher(
        FixtureGameTransport,
        PublicHtmlCoverEnricher::new(html_transport),
    );
    source
        .ingest_reviews(request())
        .await
        .expect("optional cover failure must not fail fixture ingestion")
}

#[tokio::test]
async fn one_valid_og_image_is_attached_once_to_a_complete_mandatory_snapshot() {
    let transport = FixtureHtmlTransport::new([Ok(html(200, PUBLIC_COVER_VALID))]);

    let ingestion = ingest(transport.clone()).await;

    assert_eq!(transport.calls(), 1);
    assert_eq!(ingestion.snapshot().title(), "Example Game");
    assert_eq!(ingestion.critic().kind(), ReviewKind::Critic);
    assert_eq!(ingestion.user().kind(), ReviewKind::User);
    assert_eq!(
        ingestion
            .snapshot()
            .public_cover_url()
            .map(|value| value.as_str()),
        Some("https://www.metacritic.com/images/example.jpg")
    );
}

#[tokio::test]
async fn invalid_optional_html_branches_preserve_the_mandatory_snapshot_without_a_cover_url() {
    let oversized_url = format!(
        "<meta property=\"og:image\" content=\"https://www.metacritic.com/{}\">",
        "a".repeat(2_049)
    );
    let oversized_html = " ".repeat(262_145);
    let cases = [
        html(
            500,
            "<meta property=\"og:image\" content=\"https://www.metacritic.com/a.jpg\">",
        ),
        html(403, "<html>blocked</html>"),
        html(429, "<html>limited</html>"),
        html(200, "<html>Verify you are human</html>"),
        html(200, "<meta property=\"og:image\">"),
        html(
            200,
            "<meta property=\"og:image\" content=\"https://www.metacritic.com/a.jpg\"><meta property=\"og:image\" content=\"https://www.metacritic.com/b.jpg\">",
        ),
        html(200, "<meta property=\"og:image\" content=\"not a URL\">"),
        html(
            200,
            "<meta property=\"og:image\" content=\"http://www.metacritic.com/a.jpg\">",
        ),
        html(
            200,
            "<meta property=\"og:image\" content=\"https://images.example.test/a.jpg\">",
        ),
        html(200, oversized_url),
        html(200, oversized_html),
    ];

    for response in cases {
        let transport = FixtureHtmlTransport::new([Ok(response)]);
        let ingestion = ingest(transport.clone()).await;
        assert_eq!(transport.calls(), 1);
        assert_eq!(ingestion.snapshot().source_product_id().value(), 101);
        assert_eq!(ingestion.snapshot().title(), "Example Game");
        assert!(ingestion.snapshot().public_cover_url().is_none());
    }
}

#[tokio::test]
async fn a_403_response_latches_the_circuit_before_a_body_read() {
    let response = FixtureHtmlResponse::body_read_failure(403);
    let body_reads = Arc::clone(&response.body_reads);
    let transport = FixtureHtmlTransport::new([Ok(response), Ok(html(200, PUBLIC_COVER_VALID))]);
    let source = MetacriticGameReviewSource::with_public_cover_enricher(
        FixtureGameTransport,
        PublicHtmlCoverEnricher::new(transport.clone()),
    );

    let first = source
        .ingest_reviews(request())
        .await
        .expect("first mandatory fixture ingestion must complete");
    let second = source
        .ingest_reviews(request())
        .await
        .expect("circuit must not fail a later mandatory fixture ingestion");

    assert_eq!(transport.calls(), 1);
    assert_eq!(body_reads.load(Ordering::SeqCst), 0);
    assert!(first.snapshot().public_cover_url().is_none());
    assert!(second.snapshot().public_cover_url().is_none());
}

#[tokio::test]
async fn a_429_response_latches_the_circuit_before_a_body_read() {
    let response = FixtureHtmlResponse::body_read_failure(429);
    let body_reads = Arc::clone(&response.body_reads);
    let transport = FixtureHtmlTransport::new([Ok(response), Ok(html(200, PUBLIC_COVER_VALID))]);
    let source = MetacriticGameReviewSource::with_public_cover_enricher(
        FixtureGameTransport,
        PublicHtmlCoverEnricher::new(transport.clone()),
    );

    let first = source
        .ingest_reviews(request())
        .await
        .expect("first mandatory fixture ingestion must complete");
    let second = source
        .ingest_reviews(request())
        .await
        .expect("circuit must not fail a later mandatory fixture ingestion");

    assert_eq!(transport.calls(), 1);
    assert_eq!(body_reads.load(Ordering::SeqCst), 0);
    assert!(first.snapshot().public_cover_url().is_none());
    assert!(second.snapshot().public_cover_url().is_none());
}

#[tokio::test]
async fn a_challenge_body_latches_the_until_restart_circuit() {
    let transport = FixtureHtmlTransport::new([
        Ok(html(200, "<html>Verify you are human</html>")),
        Ok(html(200, PUBLIC_COVER_VALID)),
    ]);
    let source = MetacriticGameReviewSource::with_public_cover_enricher(
        FixtureGameTransport,
        PublicHtmlCoverEnricher::new(transport.clone()),
    );

    let first = source
        .ingest_reviews(request())
        .await
        .expect("challenge must not fail mandatory fixture ingestion");
    let second = source
        .ingest_reviews(request())
        .await
        .expect("circuit must not fail later mandatory fixture ingestion");

    assert_eq!(transport.calls(), 1);
    assert!(first.snapshot().public_cover_url().is_none());
    assert!(second.snapshot().public_cover_url().is_none());
}

#[tokio::test]
async fn a_non_blocking_body_read_failure_preserves_mandatory_ingestion_without_latching() {
    let transport = FixtureHtmlTransport::new([
        Ok(FixtureHtmlResponse::body_read_failure(200)),
        Ok(html(200, PUBLIC_COVER_VALID)),
    ]);
    let source = MetacriticGameReviewSource::with_public_cover_enricher(
        FixtureGameTransport,
        PublicHtmlCoverEnricher::new(transport.clone()),
    );

    let first = source
        .ingest_reviews(request())
        .await
        .expect("body-read failure must not fail mandatory fixture ingestion");
    let second = source
        .ingest_reviews(request())
        .await
        .expect("a later optional response must remain eligible");

    assert_eq!(transport.calls(), 2);
    assert!(first.snapshot().public_cover_url().is_none());
    assert_eq!(
        second
            .snapshot()
            .public_cover_url()
            .map(|value| value.as_str()),
        Some("https://www.metacritic.com/images/example.jpg")
    );
}

#[tokio::test]
async fn one_in_flight_html_attempt_excludes_a_second_attempt_and_releases_on_cancellation() {
    let transport = PendingHtmlTransport::default();
    let enricher = PublicHtmlCoverEnricher::new(transport.clone());
    let identity = GameIdentity {
        id: gamepulse_worker_source::GameId(101),
        slug: "example-game".to_owned(),
    };
    let mut first = Box::pin(enricher.enrich(identity.clone()));

    poll_fn(|context| match first.as_mut().poll(context) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(_) => panic!("fixture cover fetch must remain pending"),
    })
    .await;
    assert_eq!(transport.calls(), 1);
    let mut excluded = Box::pin(enricher.enrich(identity.clone()));
    poll_fn(|context| match excluded.as_mut().poll(context) {
        Poll::Ready(None) => Poll::Ready(()),
        Poll::Ready(Some(_)) => panic!("excluded attempt must not return a cover"),
        Poll::Pending => panic!("in-flight attempt must exclude a second HTML fetch"),
    })
    .await;
    assert_eq!(transport.calls(), 1);

    drop(first);
    assert_eq!(transport.drops(), 1);

    let mut after_cancellation = Box::pin(enricher.enrich(identity));
    poll_fn(|context| match after_cancellation.as_mut().poll(context) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(_) => panic!("released gate must allow one new pending fetch"),
    })
    .await;
    assert_eq!(transport.calls(), 2);
    drop(after_cancellation);
    assert_eq!(transport.drops(), 2);
}

#[test]
fn public_cover_html_fixtures_accept_only_one_effective_head_declaration() {
    assert_cover(
        PUBLIC_COVER_VALID,
        Some("https://www.metacritic.com/images/example.jpg"),
    );
    assert_cover(
        PUBLIC_COVER_CONTEXTS,
        Some("https://www.metacritic.com/images/effective.jpg"),
    );
    assert_cover(
        PUBLIC_COVER_TEMPLATE,
        Some("https://www.metacritic.com/images/effective.jpg"),
    );
    assert_cover(
        PUBLIC_COVER_ENTITIES,
        Some("https://www.metacritic.com/images/effective.jpg?value=one&amp;amp;two"),
    );

    for fixture in [
        PUBLIC_COVER_BODY_ONLY,
        PUBLIC_COVER_DUPLICATE_PROPERTY,
        PUBLIC_COVER_DUPLICATE_CONTENT,
        PUBLIC_COVER_MALFORMED,
        PUBLIC_COVER_ZERO,
        PUBLIC_COVER_MULTIPLE,
    ] {
        assert_cover(fixture, None);
    }
}

#[test]
fn public_cover_html_depth_limit_fixture_fails_closed() {
    assert_cover(PUBLIC_COVER_DEPTH_LIMIT, None);
}

#[test]
fn public_cover_html_attribute_limit_fixture_fails_closed() {
    assert_cover(PUBLIC_COVER_ATTRIBUTE_LIMIT, None);
}

#[test]
fn public_cover_html_node_limit_fixture_fails_closed() {
    let node_limit = PUBLIC_COVER_NODE_LIMIT_TEMPLATE.replace(
        "<!-- repeated-node-fixture -->",
        &PUBLIC_COVER_NODE_FRAGMENT.repeat(1_024),
    );
    assert_cover(&node_limit, None);
}

fn assert_cover(html: &str, expected: Option<&str>) {
    assert_eq!(
        gamepulse_worker_source::parse_public_html_og_image(html)
            .as_ref()
            .map(|value| value.as_str()),
        expected
    );
}
