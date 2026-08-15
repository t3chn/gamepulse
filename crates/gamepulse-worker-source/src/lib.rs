#![forbid(unsafe_code)]

//! Direct-HTTP Metacritic source contract canary.

use std::fmt;
use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{Client, StatusCode, Url, redirect};
use serde::Deserialize;
use serde_json::Value;

use gamepulse_application::{
    DiscoveryCandidate, DiscoveryPage, JobHandler, JobHandlerFailure, JobHandlerFuture,
    JobHandlerResult, RuntimeJobType, TypedJob,
};
use gamepulse_domain::BrowseCursor;

const BACKEND_BASE_URL: &str = "https://backend.metacritic.com/";
const NEW_RELEASES_LIST_LIMIT: u32 = 20;
const NEWEST_BROWSE_LIST_LIMIT: u32 = 24;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_RESPONSE_BYTES: usize = 1_048_576;

/// M006's deliberately bounded source-lane handler.
///
/// It proves typed routing and durable retry behavior without fetching Metacritic
/// or writing product data before the mandatory ingestion use case is adopted.
pub struct HourlyDiscoveryPlaceholderHandler;

impl JobHandler for HourlyDiscoveryPlaceholderHandler {
    fn job_type(&self) -> RuntimeJobType {
        RuntimeJobType::SourceHourlyDiscovery
    }

    fn handle(&self, _job: TypedJob) -> JobHandlerFuture {
        Box::pin(async {
            JobHandlerResult::Failed(JobHandlerFailure::new(
                "source hourly discovery is not implemented in M006",
            ))
        })
    }
}

/// The two mandatory discovery modes established by the current source contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListMode {
    NewReleases,
    NewestBrowse,
}

/// Review paths remain distinct even though their pagination shape is shared.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewKind {
    Critic,
    User,
}

/// A public Metacritic numeric product identity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GameId(pub u64);

/// A source identity binds a stable numeric product ID to its route slug.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameIdentity {
    pub id: GameId,
    pub slug: String,
}

/// A bounded Metascore on its source-native 0-100 scale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Metascore(u8);

impl Metascore {
    pub fn value(self) -> u8 {
        self.0
    }
}

/// A bounded Userscore on its source-native 0-10 scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Userscore(f64);

impl Userscore {
    pub fn value(self) -> f64 {
        self.0
    }
}

/// The explicit continuation supplied by a finder or review response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Continuation {
    pub offset: u32,
    pub limit: u32,
}

/// A compact list item suitable for a later application-owned ingestion port.
#[derive(Clone, Debug, PartialEq)]
pub struct ListedGame {
    pub id: GameId,
    pub slug: String,
    pub title: String,
    pub release_date: Option<String>,
    pub metascore: Option<Metascore>,
    pub userscore: Option<Userscore>,
}

impl ListedGame {
    pub fn identity(&self) -> GameIdentity {
        GameIdentity {
            id: self.id,
            slug: self.slug.clone(),
        }
    }
}

/// A parsed direct-HTTP discovery response.
#[derive(Clone, Debug, PartialEq)]
pub struct ListingPage {
    pub mode: ListMode,
    pub games: Vec<ListedGame>,
    pub total_results: u64,
    pub next: Option<Continuation>,
}

/// Map a parsed source-native listing into the narrow application discovery contract.
///
/// This seam issues no request and deliberately leaves Metacritic DTOs, parser rules, URLs, and
/// continuation validation in this source adapter.
pub fn map_listing_page_for_daily_crawl(
    page: &ListingPage,
) -> Result<DiscoveryPage, DailyCrawlMappingError> {
    let candidates = page
        .games
        .iter()
        .map(|game| {
            DiscoveryCandidate::new(game.id.0, game.slug.clone())
                .map_err(|_| DailyCrawlMappingError::InvalidSourceProductId)
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(DiscoveryPage::new(
        candidates,
        page.next
            .map(|continuation| BrowseCursor::new(u64::from(continuation.offset))),
    ))
}

/// An image descriptor as supplied by the product endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageDescriptor {
    pub bucket_path: String,
    pub bucket_type: String,
    pub filename: String,
    pub kind: String,
}

/// A source-native platform descriptor. Userscores require the separate stats call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformDetail {
    pub id: u64,
    pub slug: String,
    pub release_date: Option<String>,
    pub metascore: Option<Metascore>,
}

/// A genre preserved for the adopted similarity policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Genre {
    pub id: u64,
    pub name: String,
}

/// A Metacritic-hosted video link from the product payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoLink {
    pub url: String,
}

/// The detail subset proven by the direct product endpoint.
#[derive(Clone, Debug, PartialEq)]
pub struct GameDetail {
    pub id: GameId,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub images: Vec<ImageDescriptor>,
    pub platforms: Vec<PlatformDetail>,
    pub developers: Vec<String>,
    pub genres: Vec<Genre>,
    pub video: Option<VideoLink>,
}

impl GameDetail {
    /// Return the current source-defined cover candidate without constructing a CDN URL.
    pub fn cover_image(&self) -> Option<&ImageDescriptor> {
        self.images
            .iter()
            .find(|image| image.kind.eq_ignore_ascii_case("cardImage"))
    }
}

/// The per-platform Userscore response has an independent review count.
#[derive(Clone, Debug, PartialEq)]
pub struct UserScoreSummary {
    pub score: Option<Userscore>,
    pub review_count: u64,
}

/// Review metadata intentionally excludes the source quote text in M002.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewMarker {
    pub id: Option<String>,
    pub score: Option<f64>,
    pub quote_available: bool,
}

/// Structural review input evidence, retained independently for each source kind.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewPage {
    pub kind: ReviewKind,
    pub reviews: Vec<ReviewMarker>,
    pub total_results: u64,
    pub next: Option<Continuation>,
}

/// The narrow direct-HTTP client. It performs no retries and follows no redirects.
#[derive(Clone)]
pub struct MetacriticCanaryClient {
    backend: Url,
    http: Client,
}

impl MetacriticCanaryClient {
    pub fn new() -> Result<Self, SourceError> {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let http = Client::builder()
            .default_headers(headers)
            .redirect(redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(SourceError::Http)?;
        let backend = Url::parse(BACKEND_BASE_URL).map_err(|_| SourceError::InvalidEndpoint)?;

        Ok(Self { backend, http })
    }

    /// Fetch the first daily `New Releases` selection with its verified limit of 20.
    pub async fn fetch_new_releases(&self) -> Result<ListingPage, SourceError> {
        self.fetch_listing(ListMode::NewReleases, 0, NEW_RELEASES_LIST_LIMIT)
            .await
    }

    /// Fetch a later newest-first browse page at a caller-supplied source offset.
    pub async fn fetch_newest_browse_page(&self, offset: u32) -> Result<ListingPage, SourceError> {
        self.fetch_listing(ListMode::NewestBrowse, offset, NEWEST_BROWSE_LIST_LIMIT)
            .await
    }

    /// Fetch a detail payload only when it matches the supplied source identity.
    pub async fn fetch_game_detail(
        &self,
        expected: &GameIdentity,
    ) -> Result<GameDetail, SourceError> {
        validate_slug(&expected.slug)?;
        let url = self.game_detail_url(&expected.slug);
        let body = self.get_text(url).await?;
        parse_game_detail(expected, &body)
    }

    pub async fn fetch_platform_user_score(
        &self,
        slug: &str,
        platform_slug: &str,
    ) -> Result<UserScoreSummary, SourceError> {
        validate_slug(slug)?;
        validate_slug(platform_slug)?;
        let url = self.platform_user_score_url(slug, platform_slug);
        let body = self.get_text(url).await?;
        parse_user_score_summary(slug, platform_slug, &body)
    }

    pub async fn fetch_review_page(
        &self,
        kind: ReviewKind,
        slug: &str,
        offset: u32,
        limit: u32,
    ) -> Result<ReviewPage, SourceError> {
        validate_slug(slug)?;
        if limit == 0 {
            return Err(SourceError::InvalidLimit);
        }

        let url = self.review_page_url(kind, slug, offset, limit);
        let body = self.get_text(url).await?;
        parse_review_page(kind, slug, offset, limit, &body)
    }

    async fn fetch_listing(
        &self,
        mode: ListMode,
        offset: u32,
        limit: u32,
    ) -> Result<ListingPage, SourceError> {
        if limit == 0 {
            return Err(SourceError::InvalidLimit);
        }

        let url = self.listing_url(mode, offset, limit);
        let body = self.get_text(url).await?;
        parse_listing_page(mode, offset, limit, &body)
    }

    fn endpoint(&self, segments: &[&str]) -> Url {
        let mut url = self.backend.clone();
        url.set_path(&format!("/{}", segments.join("/")));
        url
    }

    fn listing_url(&self, mode: ListMode, offset: u32, limit: u32) -> Url {
        let mut url = self.endpoint(&["finder", "metacritic", "web"]);
        let offset_text = offset.to_string();
        let limit_text = limit.to_string();
        let mut query = vec![
            ("sortBy", "-releaseDate"),
            ("mcoTypeId", "13"),
            ("offset", offset_text.as_str()),
            ("limit", limit_text.as_str()),
        ];
        if mode == ListMode::NewReleases {
            query.extend([
                ("componentName", "new-releases-carousel"),
                ("componentDisplayName", "Newly Released"),
                ("componentType", "ProductList"),
                ("metaScoreMin", "1"),
            ]);
        }
        add_query(&mut url, &query);
        url
    }

    fn game_detail_url(&self, slug: &str) -> Url {
        let mut url = self.endpoint(&["games", "metacritic", slug, "web"]);
        add_query(
            &mut url,
            &[
                ("componentName", "product"),
                ("componentDisplayName", "Product"),
                ("componentType", "Product"),
            ],
        );
        url
    }

    fn platform_user_score_url(&self, slug: &str, platform_slug: &str) -> Url {
        let mut url = self.endpoint(&[
            "reviews",
            "metacritic",
            "user",
            "games",
            slug,
            "platform",
            platform_slug,
            "stats",
            "web",
        ]);
        add_query(
            &mut url,
            &[
                ("componentName", "user-score-summary"),
                ("componentDisplayName", "User Score Summary"),
                ("componentType", "MetaScoreSummary"),
            ],
        );
        url
    }

    fn review_page_url(&self, kind: ReviewKind, slug: &str, offset: u32, limit: u32) -> Url {
        let kind_path = match kind {
            ReviewKind::Critic => "critic",
            ReviewKind::User => "user",
        };
        let mut url = self.endpoint(&["reviews", "metacritic", kind_path, "games", slug, "web"]);
        let offset_text = offset.to_string();
        let limit_text = limit.to_string();
        let query = match kind {
            ReviewKind::Critic => vec![
                ("offset", offset_text.as_str()),
                ("limit", limit_text.as_str()),
                ("sort", "date"),
                ("componentName", "latest-critic-reviews"),
                ("componentDisplayName", "Latest Critic Reviews"),
                ("componentType", "ReviewList"),
            ],
            ReviewKind::User => vec![
                ("offset", offset_text.as_str()),
                ("limit", limit_text.as_str()),
                ("orderBy", "score"),
                ("orderType", "desc"),
                ("componentName", "top-user-reviews"),
                ("componentDisplayName", "Top User Reviews"),
                ("componentType", "ReviewList"),
            ],
        };
        add_query(&mut url, &query);
        url
    }

    async fn get_text(&self, url: Url) -> Result<String, SourceError> {
        let mut response = self
            .http
            .get(url.clone())
            .send()
            .await
            .map_err(SourceError::Http)?;
        if response.status() != StatusCode::OK {
            return Err(SourceError::UnexpectedStatus {
                path: url.path().to_owned(),
                status: response.status().as_u16(),
            });
        }
        let is_json = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/json"));
        if !is_json {
            return Err(SourceError::UnexpectedContentType {
                path: url.path().to_owned(),
            });
        }
        let mut body = Vec::with_capacity(response_body_capacity(response.content_length())?);
        while let Some(chunk) = response.chunk().await.map_err(SourceError::Http)? {
            append_response_chunk(&mut body, chunk.as_ref())?;
        }
        decode_response_body(body)
    }
}

/// Parse the public finder response using the originating page context.
pub fn parse_listing_page(
    mode: ListMode,
    offset: u32,
    limit: u32,
    body: &str,
) -> Result<ListingPage, SourceError> {
    validate_limit(limit)?;
    let envelope: RawEnvelope<RawListingData> = decode("listing", body)?;
    let games = envelope
        .data
        .items
        .into_iter()
        .map(parse_listed_game)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ListingPage {
        mode,
        games,
        total_results: envelope.data.total_results,
        next: parse_continuation(
            envelope.links.next,
            ContinuationContext {
                path: "/finder/metacritic/web",
                offset,
                limit,
                total_results: envelope.data.total_results,
            },
        )?,
    })
}

/// Parse the public product response only when it matches the expected identity.
pub fn parse_game_detail(expected: &GameIdentity, body: &str) -> Result<GameDetail, SourceError> {
    validate_slug(&expected.slug)?;
    let envelope: RawEnvelope<RawDetailData> = decode("product", body)?;
    let item = envelope.data.item;
    let images = item
        .images
        .into_iter()
        .map(parse_image)
        .collect::<Result<Vec<_>, _>>()?;
    let platforms = item
        .platforms
        .into_iter()
        .map(parse_platform)
        .collect::<Result<Vec<_>, _>>()?;
    let genres = item
        .genres
        .into_iter()
        .map(|genre| {
            Ok(Genre {
                id: genre.id,
                name: required_string(genre.name, "product.genre.name")?,
            })
        })
        .collect::<Result<Vec<_>, SourceError>>()?;
    let developers = item
        .production
        .companies
        .into_iter()
        .filter(|company| company.type_name.eq_ignore_ascii_case("developer"))
        .map(|company| required_string(company.name, "product.production.companies.name"))
        .collect::<Result<Vec<_>, _>>()?;

    let id = GameId(item.id);
    let slug = required_string(item.slug, "product.slug")?;
    if id != expected.id || slug != expected.slug {
        return Err(SourceError::MismatchedGameIdentity);
    }

    Ok(GameDetail {
        id,
        slug,
        title: required_string(item.title, "product.title")?,
        description: required_string(item.description, "product.description")?,
        images,
        platforms,
        developers,
        genres,
        video: parse_video(item.video)?,
    })
}

/// Parse a platform Userscore response only when its self link matches the request.
pub fn parse_user_score_summary(
    slug: &str,
    platform_slug: &str,
    body: &str,
) -> Result<UserScoreSummary, SourceError> {
    validate_slug(slug)?;
    validate_slug(platform_slug)?;
    let envelope: RawEnvelope<RawUserScoreData> = decode("user score", body)?;
    let path = platform_user_score_path(slug, platform_slug);
    validate_backend_link(envelope.links.self_, &path, "user-score.links.self.href")?;
    Ok(UserScoreSummary {
        score: parse_userscore(envelope.data.item.score, "user-score.score")?,
        review_count: envelope.data.item.review_count,
    })
}

/// Parse a review response while intentionally leaving quote text out of M002 models.
pub fn parse_review_page(
    kind: ReviewKind,
    slug: &str,
    offset: u32,
    limit: u32,
    body: &str,
) -> Result<ReviewPage, SourceError> {
    validate_slug(slug)?;
    validate_limit(limit)?;
    let envelope: RawEnvelope<RawReviewData> = decode("review", body)?;
    let reviews = envelope
        .data
        .items
        .into_iter()
        .map(|review| parse_review_marker(kind, review))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ReviewPage {
        kind,
        reviews,
        total_results: envelope.data.total_results,
        next: parse_continuation(
            envelope.links.next,
            ContinuationContext {
                path: &review_page_path(kind, slug),
                offset,
                limit,
                total_results: envelope.data.total_results,
            },
        )?,
    })
}

#[derive(Debug)]
pub enum SourceError {
    Http(reqwest::Error),
    UnexpectedStatus { path: String, status: u16 },
    UnexpectedContentType { path: String },
    ResponseTooLarge,
    InvalidResponseUtf8,
    InvalidEndpoint,
    InvalidSlug,
    InvalidLimit,
    Decode { context: &'static str },
    MissingField { field: &'static str },
    InvalidScore { field: &'static str },
    InvalidContinuation,
    MismatchedGameIdentity,
    MismatchedSelfLink { field: &'static str },
}

/// A source-to-application mapping failure that preserves source ownership of parsing rules.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DailyCrawlMappingError {
    InvalidSourceProductId,
}

impl fmt::Display for DailyCrawlMappingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Metacritic listing has an invalid numeric product identity")
    }
}

impl std::error::Error for DailyCrawlMappingError {}

impl fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(error) => write!(formatter, "Metacritic HTTP request failed: {error}"),
            Self::UnexpectedStatus { path, status } => {
                write!(formatter, "Metacritic returned HTTP {status} for {path}")
            }
            Self::UnexpectedContentType { path } => {
                write!(
                    formatter,
                    "Metacritic returned a non-JSON response for {path}"
                )
            }
            Self::ResponseTooLarge => {
                formatter.write_str("Metacritic response body exceeds the limit")
            }
            Self::InvalidResponseUtf8 => {
                formatter.write_str("Metacritic response body is not valid UTF-8")
            }
            Self::InvalidEndpoint => {
                formatter.write_str("invalid Metacritic endpoint configuration")
            }
            Self::InvalidSlug => formatter.write_str("invalid Metacritic path slug"),
            Self::InvalidLimit => formatter.write_str("Metacritic list limit must be positive"),
            Self::Decode { context } => {
                write!(formatter, "invalid Metacritic {context} JSON shape")
            }
            Self::MissingField { field } => write!(formatter, "missing Metacritic field: {field}"),
            Self::InvalidScore { field } => write!(formatter, "invalid Metacritic score: {field}"),
            Self::InvalidContinuation => {
                formatter.write_str("invalid Metacritic continuation link")
            }
            Self::MismatchedGameIdentity => {
                formatter.write_str("Metacritic detail does not match the requested game identity")
            }
            Self::MismatchedSelfLink { field } => {
                write!(
                    formatter,
                    "Metacritic self link does not match the request: {field}"
                )
            }
        }
    }
}

impl std::error::Error for SourceError {}

fn add_query(url: &mut Url, pairs: &[(&str, &str)]) {
    let mut query = url.query_pairs_mut();
    for (key, value) in pairs {
        query.append_pair(key, value);
    }
}

fn validate_limit(limit: u32) -> Result<(), SourceError> {
    if limit == 0 {
        return Err(SourceError::InvalidLimit);
    }
    Ok(())
}

fn validate_slug(slug: &str) -> Result<(), SourceError> {
    if slug.is_empty()
        || !slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(SourceError::InvalidSlug);
    }
    Ok(())
}

fn decode<T: for<'de> Deserialize<'de>>(
    context: &'static str,
    body: &str,
) -> Result<T, SourceError> {
    serde_json::from_str(body).map_err(|_| SourceError::Decode { context })
}

fn parse_listed_game(raw: RawListingItem) -> Result<ListedGame, SourceError> {
    Ok(ListedGame {
        id: GameId(raw.id),
        slug: required_string(raw.slug, "listing.slug")?,
        title: required_string(raw.title, "listing.title")?,
        release_date: optional_string(raw.release_date),
        metascore: parse_metascore(raw.critic_score_summary, "listing.criticScoreSummary.score")?,
        userscore: parse_userscore(
            raw.user_score.and_then(|score| score.score),
            "listing.userScore.score",
        )?,
    })
}

fn parse_image(raw: RawImage) -> Result<ImageDescriptor, SourceError> {
    Ok(ImageDescriptor {
        bucket_path: required_string(raw.bucket_path, "product.images.bucketPath")?,
        bucket_type: required_string(raw.bucket_type, "product.images.bucketType")?,
        filename: required_string(raw.filename, "product.images.filename")?,
        kind: required_string(raw.type_name, "product.images.typeName")?,
    })
}

fn parse_platform(raw: RawPlatform) -> Result<PlatformDetail, SourceError> {
    Ok(PlatformDetail {
        id: raw.id,
        slug: required_string(raw.slug, "product.platforms.slug")?,
        release_date: optional_string(raw.release_date),
        metascore: parse_metascore(
            raw.critic_score_summary,
            "product.platforms.criticScoreSummary.score",
        )?,
    })
}

fn parse_video(raw: Option<RawVideo>) -> Result<Option<VideoLink>, SourceError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let url = raw
        .embed_url
        .or(raw.manifest_url)
        .ok_or(SourceError::MissingField {
            field: "product.video.embedUrl",
        })?;
    let parsed = Url::parse(&url).map_err(|_| SourceError::MissingField {
        field: "product.video.embedUrl",
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(SourceError::MissingField {
            field: "product.video.embedUrl",
        });
    }
    Ok(Some(VideoLink { url }))
}

fn parse_review_marker(kind: ReviewKind, raw: RawReview) -> Result<ReviewMarker, SourceError> {
    let max = match kind {
        ReviewKind::Critic => 100.0,
        ReviewKind::User => 10.0,
    };
    let score = parse_optional_score(raw.score, max, false, "review.score")?;
    let quote_available = match raw.quote {
        None | Some(Value::Null) => false,
        Some(Value::String(quote)) => !quote.trim().is_empty(),
        Some(_) => {
            return Err(SourceError::MissingField {
                field: "review.quote",
            });
        }
    };

    Ok(ReviewMarker {
        id: parse_optional_identifier(raw.id, "review.id")?,
        score,
        quote_available,
    })
}

fn parse_optional_identifier(
    value: Option<Value>,
    field: &'static str,
) -> Result<Option<String>, SourceError> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::String(value) if !value.trim().is_empty() => Ok(Some(value)),
        Value::Number(value) => Ok(Some(value.to_string())),
        _ => Err(SourceError::MissingField { field }),
    }
}

fn parse_metascore(
    summary: Option<RawScoreSummary>,
    field: &'static str,
) -> Result<Option<Metascore>, SourceError> {
    let Some(value) = parse_optional_score(
        summary.and_then(|summary| summary.score),
        100.0,
        true,
        field,
    )?
    else {
        return Ok(None);
    };
    Ok(Some(Metascore(value as u8)))
}

fn parse_userscore(
    value: Option<Value>,
    field: &'static str,
) -> Result<Option<Userscore>, SourceError> {
    parse_optional_score(value, 10.0, false, field).map(|score| score.map(Userscore))
}

fn parse_optional_score(
    value: Option<Value>,
    maximum: f64,
    whole_number: bool,
    field: &'static str,
) -> Result<Option<f64>, SourceError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(number) = value.as_f64() else {
        return Err(SourceError::InvalidScore { field });
    };
    if !number.is_finite()
        || !(0.0..=maximum).contains(&number)
        || (whole_number && number.fract() != 0.0)
    {
        return Err(SourceError::InvalidScore { field });
    }
    Ok(Some(number))
}

fn response_body_capacity(content_length: Option<u64>) -> Result<usize, SourceError> {
    let Some(content_length) = content_length else {
        return Ok(0);
    };
    let content_length =
        usize::try_from(content_length).map_err(|_| SourceError::ResponseTooLarge)?;
    if content_length > MAX_RESPONSE_BYTES {
        return Err(SourceError::ResponseTooLarge);
    }
    Ok(content_length)
}

fn append_response_chunk(body: &mut Vec<u8>, chunk: &[u8]) -> Result<(), SourceError> {
    let next_length = body
        .len()
        .checked_add(chunk.len())
        .ok_or(SourceError::ResponseTooLarge)?;
    if next_length > MAX_RESPONSE_BYTES {
        return Err(SourceError::ResponseTooLarge);
    }
    body.extend_from_slice(chunk);
    Ok(())
}

fn decode_response_body(body: Vec<u8>) -> Result<String, SourceError> {
    String::from_utf8(body).map_err(|_| SourceError::InvalidResponseUtf8)
}

struct ContinuationContext<'a> {
    path: &'a str,
    offset: u32,
    limit: u32,
    total_results: u64,
}

fn parse_continuation(
    raw: Option<RawLink>,
    context: ContinuationContext<'_>,
) -> Result<Option<Continuation>, SourceError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let href = raw.href.ok_or(SourceError::InvalidContinuation)?;
    let url = Url::parse(&href).map_err(|_| SourceError::InvalidContinuation)?;
    if !is_backend_path(&url, context.path) {
        return Err(SourceError::InvalidContinuation);
    }
    let expected_offset = context
        .offset
        .checked_add(context.limit)
        .ok_or(SourceError::InvalidContinuation)?;
    if u64::from(expected_offset) >= context.total_results {
        return Err(SourceError::InvalidContinuation);
    }
    let mut offset_seen = false;
    let mut limit_seen = false;
    let mut offset = None;
    let mut limit = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "offset" => {
                if offset_seen {
                    return Err(SourceError::InvalidContinuation);
                }
                offset_seen = true;
                offset = value.parse::<u32>().ok();
            }
            "limit" => {
                if limit_seen {
                    return Err(SourceError::InvalidContinuation);
                }
                limit_seen = true;
                limit = value.parse::<u32>().ok();
            }
            _ => {}
        }
    }
    match (offset, limit) {
        (Some(offset), Some(limit))
            if offset == expected_offset && limit == context.limit && limit > 0 =>
        {
            Ok(Some(Continuation { offset, limit }))
        }
        _ => Err(SourceError::InvalidContinuation),
    }
}

fn validate_backend_link(
    raw: Option<RawLink>,
    expected_path: &str,
    field: &'static str,
) -> Result<(), SourceError> {
    let href = raw
        .and_then(|link| link.href)
        .ok_or(SourceError::MismatchedSelfLink { field })?;
    let url = Url::parse(&href).map_err(|_| SourceError::MismatchedSelfLink { field })?;
    if !is_backend_path(&url, expected_path) {
        return Err(SourceError::MismatchedSelfLink { field });
    }
    Ok(())
}

fn is_backend_path(url: &Url, expected_path: &str) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("backend.metacritic.com")
        && url.path() == expected_path
}

fn platform_user_score_path(slug: &str, platform_slug: &str) -> String {
    format!("/reviews/metacritic/user/games/{slug}/platform/{platform_slug}/stats/web")
}

fn review_page_path(kind: ReviewKind, slug: &str) -> String {
    let kind = match kind {
        ReviewKind::Critic => "critic",
        ReviewKind::User => "user",
    };
    format!("/reviews/metacritic/{kind}/games/{slug}/web")
}

fn required_string(value: String, field: &'static str) -> Result<String, SourceError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(SourceError::MissingField { field });
    }
    Ok(value.to_owned())
}

fn optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

#[derive(Deserialize)]
struct RawEnvelope<T> {
    data: T,
    #[serde(default)]
    links: RawLinks,
}

#[derive(Default, Deserialize)]
struct RawLinks {
    #[serde(default, rename = "self")]
    self_: Option<RawLink>,
    #[serde(default)]
    next: Option<RawLink>,
}

#[derive(Deserialize)]
struct RawLink {
    href: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawListingData {
    items: Vec<RawListingItem>,
    total_results: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawListingItem {
    id: u64,
    slug: String,
    title: String,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    critic_score_summary: Option<RawScoreSummary>,
    #[serde(default)]
    user_score: Option<RawScore>,
}

#[derive(Deserialize)]
struct RawScoreSummary {
    #[serde(default)]
    score: Option<Value>,
}

#[derive(Deserialize)]
struct RawScore {
    #[serde(default)]
    score: Option<Value>,
}

#[derive(Deserialize)]
struct RawDetailData {
    item: RawDetail,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDetail {
    id: u64,
    slug: String,
    title: String,
    description: String,
    #[serde(default)]
    images: Vec<RawImage>,
    #[serde(default)]
    platforms: Vec<RawPlatform>,
    #[serde(default)]
    production: RawProduction,
    #[serde(default)]
    genres: Vec<RawGenre>,
    #[serde(default)]
    video: Option<RawVideo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawImage {
    bucket_path: String,
    bucket_type: String,
    filename: String,
    type_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPlatform {
    id: u64,
    slug: String,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    critic_score_summary: Option<RawScoreSummary>,
}

#[derive(Default, Deserialize)]
struct RawProduction {
    #[serde(default)]
    companies: Vec<RawCompany>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCompany {
    name: String,
    type_name: String,
}

#[derive(Deserialize)]
struct RawGenre {
    id: u64,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawVideo {
    #[serde(default)]
    embed_url: Option<String>,
    #[serde(default)]
    manifest_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawUserScoreData {
    item: RawUserScoreItem,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawUserScoreItem {
    #[serde(default)]
    score: Option<Value>,
    review_count: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReviewData {
    items: Vec<RawReview>,
    total_results: u64,
}

#[derive(Deserialize)]
struct RawReview {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    score: Option<Value>,
    #[serde(default)]
    quote: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_urls_preserve_each_verified_contract() {
        let client = MetacriticCanaryClient::new().expect("client configuration must be valid");

        assert_request(
            client.listing_url(ListMode::NewReleases, 0, 20),
            "/finder/metacritic/web",
            &[
                ("componentDisplayName", "Newly Released"),
                ("componentName", "new-releases-carousel"),
                ("componentType", "ProductList"),
                ("limit", "20"),
                ("mcoTypeId", "13"),
                ("metaScoreMin", "1"),
                ("offset", "0"),
                ("sortBy", "-releaseDate"),
            ],
        );
        assert_request(
            client.listing_url(ListMode::NewestBrowse, 24, 24),
            "/finder/metacritic/web",
            &[
                ("limit", "24"),
                ("mcoTypeId", "13"),
                ("offset", "24"),
                ("sortBy", "-releaseDate"),
            ],
        );
        assert_request(
            client.game_detail_url("example-game"),
            "/games/metacritic/example-game/web",
            &[
                ("componentDisplayName", "Product"),
                ("componentName", "product"),
                ("componentType", "Product"),
            ],
        );
        assert_request(
            client.platform_user_score_url("example-game", "pc"),
            "/reviews/metacritic/user/games/example-game/platform/pc/stats/web",
            &[
                ("componentDisplayName", "User Score Summary"),
                ("componentName", "user-score-summary"),
                ("componentType", "MetaScoreSummary"),
            ],
        );
        assert_request(
            client.review_page_url(ReviewKind::Critic, "example-game", 0, 3),
            "/reviews/metacritic/critic/games/example-game/web",
            &[
                ("componentDisplayName", "Latest Critic Reviews"),
                ("componentName", "latest-critic-reviews"),
                ("componentType", "ReviewList"),
                ("limit", "3"),
                ("offset", "0"),
                ("sort", "date"),
            ],
        );
        assert_request(
            client.review_page_url(ReviewKind::User, "example-game", 0, 3),
            "/reviews/metacritic/user/games/example-game/web",
            &[
                ("componentDisplayName", "Top User Reviews"),
                ("componentName", "top-user-reviews"),
                ("componentType", "ReviewList"),
                ("limit", "3"),
                ("offset", "0"),
                ("orderBy", "score"),
                ("orderType", "desc"),
            ],
        );
    }

    #[test]
    fn bounded_body_decoder_rejects_oversize_and_invalid_utf8() {
        assert!(matches!(
            response_body_capacity(Some((MAX_RESPONSE_BYTES + 1) as u64)),
            Err(SourceError::ResponseTooLarge)
        ));

        let mut body = Vec::new();
        let oversized_chunk = vec![0_u8; MAX_RESPONSE_BYTES + 1];
        assert!(matches!(
            append_response_chunk(&mut body, &oversized_chunk),
            Err(SourceError::ResponseTooLarge)
        ));
        assert!(matches!(
            decode_response_body(vec![0xff]),
            Err(SourceError::InvalidResponseUtf8)
        ));
    }

    fn assert_request(url: Url, expected_path: &str, expected_pairs: &[(&str, &str)]) {
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("backend.metacritic.com"));
        assert_eq!(url.path(), expected_path);

        let mut actual = url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<Vec<_>>();
        let mut expected = expected_pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<Vec<_>>();
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(actual, expected);
    }
}
