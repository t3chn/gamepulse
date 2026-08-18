#![forbid(unsafe_code)]

//! Application use cases and ports for GamePulse.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub use gamepulse_domain::{
    APP_NAME, BrowseCursor, BrowseProgress, CrawlDayKey, CrawlDayKeyError, CrawlDiscoveryRequest,
    DAILY_CRAWL_SELECTION_LIMIT, DailyCrawlAction, DailyCrawlState, DailyCrawlTransition,
    GameCoverDescriptor, GameDeveloper, GamePlatformScore, GamePublicCoverUrl, GameSnapshot,
    GameSnapshotValidationError, GameVideoLink, Metascore, MetascoreError,
    REVIEW_EXCERPT_MAX_BYTES, REVIEW_INPUT_LIMIT, ReviewExcerpt, ReviewExcerptError, ReviewKind,
    ReviewPolarity, SourceProductId, SourceProductIdError, Userscore, UserscoreError,
};
use gamepulse_domain::{prepare_daily_crawl, select_daily_crawl_up_to};

/// One hourly browse selection can advance through at most this many source pages before failing
/// closed. It bounds source work while still allowing a replayed page to be completed with later
/// newest-first pages in the same atomic commit.
const MAX_BROWSE_PAGES_PER_HOURLY_SELECTION: usize = 8;

/// Application-owned durable boundary for replacing one complete game snapshot.
///
/// Implementations must key the game by `source_product_id`, update its mutable source slug, and
/// make the game row plus all platform-score and developer rows visible together or not at all.
pub trait GameSnapshotStore {
    type Error;

    fn upsert_snapshot(&mut self, snapshot: &GameSnapshot) -> Result<(), Self::Error>;
}

/// The allowlisted media types GamePulse may retain and serve for a local game cover.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoverImageContentType {
    Jpeg,
    Png,
    Webp,
}

impl CoverImageContentType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "image/jpeg" => Some(Self::Jpeg),
            "image/png" => Some(Self::Png),
            "image/webp" => Some(Self::Webp),
            _ => None,
        }
    }
}

/// A size-bounded image already validated by the source adapter for durable local delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredCoverImage {
    content_type: CoverImageContentType,
    bytes: Vec<u8>,
}

impl StoredCoverImage {
    pub const MAX_BYTES: usize = 2 * 1024 * 1024;

    pub fn new(content_type: CoverImageContentType, bytes: Vec<u8>) -> Option<Self> {
        (!bytes.is_empty() && bytes.len() <= Self::MAX_BYTES).then_some(Self {
            content_type,
            bytes,
        })
    }

    pub const fn content_type(&self) -> CoverImageContentType {
        self.content_type
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// A collision-free, versioned representation of the exact source descriptor that selected a
/// local cover asset. It is intentionally derived in application code so every durable adapter
/// compares the same stable identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoverDescriptorFingerprint(String);

impl CoverDescriptorFingerprint {
    pub fn from_descriptor(descriptor: &GameCoverDescriptor) -> Self {
        let mut value = String::from("v1");
        for component in [
            descriptor.bucket_path(),
            descriptor.bucket_type(),
            descriptor.filename(),
            descriptor.kind(),
        ] {
            let _ = write!(value, ":{}:", component.len());
            for byte in component.bytes() {
                let _ = write!(value, "{byte:02x}");
            }
        }
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One persisted source descriptor selected for the bounded local-cover refresh workflow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoverBackfillCandidate {
    source_product_id: SourceProductId,
    descriptor: GameCoverDescriptor,
    descriptor_fingerprint: CoverDescriptorFingerprint,
}

impl CoverBackfillCandidate {
    pub fn new(source_product_id: SourceProductId, descriptor: GameCoverDescriptor) -> Self {
        let descriptor_fingerprint = CoverDescriptorFingerprint::from_descriptor(&descriptor);
        Self {
            source_product_id,
            descriptor,
            descriptor_fingerprint,
        }
    }

    pub const fn source_product_id(&self) -> SourceProductId {
        self.source_product_id
    }

    pub fn descriptor(&self) -> &GameCoverDescriptor {
        &self.descriptor
    }

    pub fn descriptor_fingerprint(&self) -> &CoverDescriptorFingerprint {
        &self.descriptor_fingerprint
    }
}

/// The only conditional persistence outcomes allowed by the local-cover workflow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoverBackfillPersistOutcome {
    Stored,
    AlreadyCurrent,
    Stale,
}

/// Application-owned durable boundary for selecting stale local assets and conditionally storing
/// one fetched cover only while its descriptor remains current.
pub trait CoverBackfillStorePort {
    type Error;

    fn cover_backfill_candidates(
        &mut self,
        limit: usize,
    ) -> Result<Vec<CoverBackfillCandidate>, Self::Error>;

    fn store_cover_if_current(
        &mut self,
        candidate: &CoverBackfillCandidate,
        cover: &StoredCoverImage,
    ) -> Result<CoverBackfillPersistOutcome, Self::Error>;
}

/// Application-owned asynchronous source boundary for one already-selected cover descriptor.
pub trait AsyncCoverImageSourcePort: Send + Sync {
    type Error;
    type FetchFuture<'a>: Future<Output = Result<CoverBackfillFetchOutcome, Self::Error>>
        + Send
        + 'a
    where
        Self: 'a;

    fn fetch_cover(&self, candidate: &CoverBackfillCandidate) -> Self::FetchFuture<'_>;
}

/// One opt-in invocation cannot issue more than this many source requests.
pub const MAX_COVER_BACKFILL_CANDIDATES: usize = 20;

/// Safe status-class grouping for a cover response that was not exactly HTTP 200.
///
/// The report deliberately retains no raw status code or response text. A non-200 2xx response is
/// still an unavailable outcome because the bounded image protocol requires exactly HTTP 200.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoverBackfillHttpStatusClass {
    Informational,
    SuccessfulOther,
    Redirection,
    ClientError,
    ServerError,
    Other,
}

impl CoverBackfillHttpStatusClass {
    pub const fn from_status(status: u16) -> Self {
        match status {
            100..=199 => Self::Informational,
            200..=299 => Self::SuccessfulOther,
            300..=399 => Self::Redirection,
            400..=499 => Self::ClientError,
            500..=599 => Self::ServerError,
            _ => Self::Other,
        }
    }
}

/// Closed, aggregate-safe reasons why a selected cover did not yield a durable local asset.
///
/// Source adapters may map their local observations into this enum, but the coordinator owns the
/// resulting counters. No variant contains a descriptor, URL, header value, body, or identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoverBackfillUnavailableReason {
    DescriptorRejected,
    UnexpectedHttpStatus(CoverBackfillHttpStatusClass),
    UnsupportedContentType,
    SignatureMismatch,
    InvalidBody,
}

/// The only non-error result a cover source adapter may return for a selected candidate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoverBackfillFetchOutcome {
    Stored(StoredCoverImage),
    Unavailable(CoverBackfillUnavailableReason),
}

/// Stable aggregate-only diagnostics for all unavailable cover outcomes in one invocation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CoverBackfillUnavailableReasons {
    descriptor_rejected: usize,
    http_informational: usize,
    http_successful_other: usize,
    http_redirection: usize,
    http_client_error: usize,
    http_server_error: usize,
    http_other: usize,
    unsupported_content_type: usize,
    signature_mismatch: usize,
    invalid_body: usize,
}

impl CoverBackfillUnavailableReasons {
    pub const fn descriptor_rejected(self) -> usize {
        self.descriptor_rejected
    }

    pub const fn http_informational(self) -> usize {
        self.http_informational
    }

    pub const fn http_successful_other(self) -> usize {
        self.http_successful_other
    }

    pub const fn http_redirection(self) -> usize {
        self.http_redirection
    }

    pub const fn http_client_error(self) -> usize {
        self.http_client_error
    }

    pub const fn http_server_error(self) -> usize {
        self.http_server_error
    }

    pub const fn http_other(self) -> usize {
        self.http_other
    }

    pub const fn unsupported_content_type(self) -> usize {
        self.unsupported_content_type
    }

    pub const fn signature_mismatch(self) -> usize {
        self.signature_mismatch
    }

    pub const fn invalid_body(self) -> usize {
        self.invalid_body
    }

    /// The top-level unavailable count is derived from these counters, so the public invariant
    /// cannot drift as new branches are recorded.
    pub const fn total(self) -> usize {
        self.descriptor_rejected
            .saturating_add(self.http_informational)
            .saturating_add(self.http_successful_other)
            .saturating_add(self.http_redirection)
            .saturating_add(self.http_client_error)
            .saturating_add(self.http_server_error)
            .saturating_add(self.http_other)
            .saturating_add(self.unsupported_content_type)
            .saturating_add(self.signature_mismatch)
            .saturating_add(self.invalid_body)
    }

    fn record(&mut self, reason: CoverBackfillUnavailableReason) {
        match reason {
            CoverBackfillUnavailableReason::DescriptorRejected => {
                self.descriptor_rejected = self.descriptor_rejected.saturating_add(1)
            }
            CoverBackfillUnavailableReason::UnexpectedHttpStatus(
                CoverBackfillHttpStatusClass::Informational,
            ) => self.http_informational = self.http_informational.saturating_add(1),
            CoverBackfillUnavailableReason::UnexpectedHttpStatus(
                CoverBackfillHttpStatusClass::SuccessfulOther,
            ) => self.http_successful_other = self.http_successful_other.saturating_add(1),
            CoverBackfillUnavailableReason::UnexpectedHttpStatus(
                CoverBackfillHttpStatusClass::Redirection,
            ) => self.http_redirection = self.http_redirection.saturating_add(1),
            CoverBackfillUnavailableReason::UnexpectedHttpStatus(
                CoverBackfillHttpStatusClass::ClientError,
            ) => self.http_client_error = self.http_client_error.saturating_add(1),
            CoverBackfillUnavailableReason::UnexpectedHttpStatus(
                CoverBackfillHttpStatusClass::ServerError,
            ) => self.http_server_error = self.http_server_error.saturating_add(1),
            CoverBackfillUnavailableReason::UnexpectedHttpStatus(
                CoverBackfillHttpStatusClass::Other,
            ) => self.http_other = self.http_other.saturating_add(1),
            CoverBackfillUnavailableReason::UnsupportedContentType => {
                self.unsupported_content_type = self.unsupported_content_type.saturating_add(1)
            }
            CoverBackfillUnavailableReason::SignatureMismatch => {
                self.signature_mismatch = self.signature_mismatch.saturating_add(1)
            }
            CoverBackfillUnavailableReason::InvalidBody => {
                self.invalid_body = self.invalid_body.saturating_add(1)
            }
        }
    }
}

/// Aggregate-only result of one bounded local-cover refresh invocation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CoverBackfillReport {
    attempted: usize,
    stored: usize,
    unavailable_reasons: CoverBackfillUnavailableReasons,
    stale: usize,
    already_current: usize,
    failed: usize,
}

impl CoverBackfillReport {
    pub const fn attempted(self) -> usize {
        self.attempted
    }

    pub const fn stored(self) -> usize {
        self.stored
    }

    pub const fn unavailable(self) -> usize {
        self.unavailable_reasons.total()
    }

    pub const fn unavailable_reasons(self) -> CoverBackfillUnavailableReasons {
        self.unavailable_reasons
    }

    pub const fn stale(self) -> usize {
        self.stale
    }

    pub const fn already_current(self) -> usize {
        self.already_current
    }

    pub const fn failed(self) -> usize {
        self.failed
    }

    /// A repeat is safe only while the preceding invocation made durable progress.
    pub const fn made_progress(self) -> bool {
        self.stored > 0
    }

    pub const fn exit_code(self) -> i32 {
        if self.failed == 0 { 0 } else { 1 }
    }

    pub fn to_json(self) -> String {
        format!(
            concat!(
                "{{\"schema_version\":\"gamepulse.cover_backfill.v3\",",
                "\"attempted\":{},\"stored\":{},\"unavailable\":{},",
                "\"unavailable_reasons\":{{",
                "\"descriptor_rejected\":{},",
                "\"unexpected_http_status\":{{",
                "\"informational\":{},\"successful_other\":{},\"redirection\":{},",
                "\"client_error\":{},\"server_error\":{},\"other\":{}}},",
                "\"unsupported_content_type\":{},\"signature_mismatch\":{},\"invalid_body\":{}}},",
                "\"stale\":{},\"already_current\":{},\"failed\":{},\"made_progress\":{}}}"
            ),
            self.attempted,
            self.stored,
            self.unavailable(),
            self.unavailable_reasons.descriptor_rejected,
            self.unavailable_reasons.http_informational,
            self.unavailable_reasons.http_successful_other,
            self.unavailable_reasons.http_redirection,
            self.unavailable_reasons.http_client_error,
            self.unavailable_reasons.http_server_error,
            self.unavailable_reasons.http_other,
            self.unavailable_reasons.unsupported_content_type,
            self.unavailable_reasons.signature_mismatch,
            self.unavailable_reasons.invalid_body,
            self.stale,
            self.already_current,
            self.failed,
            self.made_progress(),
        )
    }
}

/// Only an invalid caller limit or initial candidate-selection failure prevents a report.
#[derive(Debug)]
pub enum CoverBackfillExecutionError<StoreError> {
    InvalidLimit,
    Selection(StoreError),
}

/// Fetch and conditionally persist a bounded set of missing or stale local cover assets.
///
/// The coordinator deliberately holds no durable store lock while a source future is pending.
/// Repeated calls are deterministic by candidate order; a descriptor refresh between selection and
/// persistence is reported as stale rather than overwriting the newer source state.
pub async fn execute_cover_backfill<S, P>(
    store: &mut S,
    source: &P,
    limit: usize,
) -> Result<CoverBackfillReport, CoverBackfillExecutionError<S::Error>>
where
    S: CoverBackfillStorePort,
    P: AsyncCoverImageSourcePort,
{
    if !(1..=MAX_COVER_BACKFILL_CANDIDATES).contains(&limit) {
        return Err(CoverBackfillExecutionError::InvalidLimit);
    }
    let candidates = store
        .cover_backfill_candidates(limit)
        .map_err(CoverBackfillExecutionError::Selection)?;
    let mut report = CoverBackfillReport::default();
    for candidate in candidates {
        report.attempted = report.attempted.saturating_add(1);
        match source.fetch_cover(&candidate).await {
            Ok(CoverBackfillFetchOutcome::Stored(cover)) => {
                match store.store_cover_if_current(&candidate, &cover) {
                    Ok(CoverBackfillPersistOutcome::Stored) => {
                        report.stored = report.stored.saturating_add(1)
                    }
                    Ok(CoverBackfillPersistOutcome::AlreadyCurrent) => {
                        report.already_current = report.already_current.saturating_add(1)
                    }
                    Ok(CoverBackfillPersistOutcome::Stale) => {
                        report.stale = report.stale.saturating_add(1)
                    }
                    Err(_) => report.failed = report.failed.saturating_add(1),
                }
            }
            Ok(CoverBackfillFetchOutcome::Unavailable(reason)) => {
                report.unavailable_reasons.record(reason)
            }
            Err(_) => report.failed = report.failed.saturating_add(1),
        }
    }
    Ok(report)
}

/// The compact fields rendered on a catalogue card.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogueGameCard {
    source_product_id: SourceProductId,
    title: String,
    has_local_cover: bool,
    highest_metascore: Option<u8>,
    platforms: Vec<String>,
    developers: Vec<String>,
}

impl CatalogueGameCard {
    pub fn new(
        source_product_id: SourceProductId,
        title: impl Into<String>,
        has_local_cover: bool,
        highest_metascore: Option<u8>,
        platforms: Vec<String>,
        developers: Vec<String>,
    ) -> Self {
        Self {
            source_product_id,
            title: title.into(),
            has_local_cover,
            highest_metascore,
            platforms,
            developers,
        }
    }

    pub const fn source_product_id(&self) -> SourceProductId {
        self.source_product_id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub const fn has_local_cover(&self) -> bool {
        self.has_local_cover
    }

    pub const fn highest_metascore(&self) -> Option<u8> {
        self.highest_metascore
    }

    pub fn platforms(&self) -> &[String] {
        &self.platforms
    }

    pub fn developers(&self) -> &[String] {
        &self.developers
    }
}

/// One persisted platform score displayed on a catalogue detail page.
#[derive(Clone, Debug, PartialEq)]
pub struct CataloguePlatformScore {
    source_platform_id: u64,
    source_slug: String,
    metascore: Option<u8>,
    userscore: Option<f64>,
}

impl CataloguePlatformScore {
    pub fn new(
        source_platform_id: u64,
        source_slug: impl Into<String>,
        metascore: Option<u8>,
        userscore: Option<f64>,
    ) -> Self {
        Self {
            source_platform_id,
            source_slug: source_slug.into(),
            metascore,
            userscore,
        }
    }

    pub const fn source_platform_id(&self) -> u64 {
        self.source_platform_id
    }

    pub fn source_slug(&self) -> &str {
        &self.source_slug
    }

    pub const fn metascore(&self) -> Option<u8> {
        self.metascore
    }

    pub const fn userscore(&self) -> Option<f64> {
        self.userscore
    }
}

/// The stored cover descriptor rendered as source metadata, never as a fabricated URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogueCoverDescriptor {
    bucket_path: String,
    bucket_type: String,
    filename: String,
    kind: String,
}

impl CatalogueCoverDescriptor {
    pub fn new(
        bucket_path: impl Into<String>,
        bucket_type: impl Into<String>,
        filename: impl Into<String>,
        kind: impl Into<String>,
    ) -> Self {
        Self {
            bucket_path: bucket_path.into(),
            bucket_type: bucket_type.into(),
            filename: filename.into(),
            kind: kind.into(),
        }
    }

    pub fn bucket_path(&self) -> &str {
        &self.bucket_path
    }

    pub fn bucket_type(&self) -> &str {
        &self.bucket_type
    }

    pub fn filename(&self) -> &str {
        &self.filename
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }
}

/// One stored candidate selected from the SQLite-only similarity fallback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimilarCatalogueGame {
    source_product_id: SourceProductId,
    title: String,
}

/// A persisted review-summary state exposed on the stored-game detail page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogueReviewSummary {
    Pending,
    Unavailable,
    Available {
        likes: Vec<String>,
        dislikes: Vec<String>,
    },
}

impl SimilarCatalogueGame {
    pub fn new(source_product_id: SourceProductId, title: impl Into<String>) -> Self {
        Self {
            source_product_id,
            title: title.into(),
        }
    }

    pub const fn source_product_id(&self) -> SourceProductId {
        self.source_product_id
    }

    pub fn title(&self) -> &str {
        &self.title
    }
}

/// The complete stored representation needed by the server-rendered game page.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogueGameDetail {
    source_product_id: SourceProductId,
    source_slug: String,
    title: String,
    description: String,
    cover: Option<CatalogueCoverDescriptor>,
    has_local_cover: bool,
    video_url: Option<String>,
    platform_scores: Vec<CataloguePlatformScore>,
    developers: Vec<String>,
    similar_games: Vec<SimilarCatalogueGame>,
    critic_summary: Option<CatalogueReviewSummary>,
    user_summary: Option<CatalogueReviewSummary>,
}

impl CatalogueGameDetail {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_product_id: SourceProductId,
        source_slug: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
        cover: Option<CatalogueCoverDescriptor>,
        has_local_cover: bool,
        video_url: Option<String>,
        platform_scores: Vec<CataloguePlatformScore>,
        developers: Vec<String>,
        similar_games: Vec<SimilarCatalogueGame>,
        critic_summary: Option<CatalogueReviewSummary>,
        user_summary: Option<CatalogueReviewSummary>,
    ) -> Self {
        Self {
            source_product_id,
            source_slug: source_slug.into(),
            title: title.into(),
            description: description.into(),
            cover,
            has_local_cover,
            video_url,
            platform_scores,
            developers,
            similar_games,
            critic_summary,
            user_summary,
        }
    }

    pub const fn source_product_id(&self) -> SourceProductId {
        self.source_product_id
    }

    pub fn source_slug(&self) -> &str {
        &self.source_slug
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn cover(&self) -> Option<&CatalogueCoverDescriptor> {
        self.cover.as_ref()
    }

    pub const fn has_local_cover(&self) -> bool {
        self.has_local_cover
    }

    pub fn video_url(&self) -> Option<&str> {
        self.video_url.as_deref()
    }

    pub fn platform_scores(&self) -> &[CataloguePlatformScore] {
        &self.platform_scores
    }

    pub fn developers(&self) -> &[String] {
        &self.developers
    }

    pub fn similar_games(&self) -> &[SimilarCatalogueGame] {
        &self.similar_games
    }

    pub fn critic_summary(&self) -> Option<&CatalogueReviewSummary> {
        self.critic_summary.as_ref()
    }

    pub fn user_summary(&self) -> Option<&CatalogueReviewSummary> {
        self.user_summary.as_ref()
    }
}

/// The title and platform filters for the always-rating-sorted catalogue.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CatalogueQuery {
    title_search: Option<String>,
    platform_slug: Option<String>,
}

impl CatalogueQuery {
    pub fn new(title_search: Option<String>, platform_slug: Option<String>) -> Self {
        Self {
            title_search: normalize_catalogue_filter(title_search),
            platform_slug: normalize_catalogue_filter(platform_slug),
        }
    }

    pub fn title_search(&self) -> Option<&str> {
        self.title_search.as_deref()
    }

    pub fn platform_slug(&self) -> Option<&str> {
        self.platform_slug.as_deref()
    }
}

fn normalize_catalogue_filter(value: Option<String>) -> Option<String> {
    value.and_then(|value| (!value.trim().is_empty()).then_some(value))
}

/// The persisted platform values available as catalogue filters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CataloguePlatformFilter {
    source_slug: String,
}

impl CataloguePlatformFilter {
    pub fn new(source_slug: impl Into<String>) -> Self {
        Self {
            source_slug: source_slug.into(),
        }
    }

    pub fn source_slug(&self) -> &str {
        &self.source_slug
    }
}

/// The complete data required to render one catalogue response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CataloguePage {
    games: Vec<CatalogueGameCard>,
    platform_filters: Vec<CataloguePlatformFilter>,
}

impl CataloguePage {
    pub fn new(
        games: Vec<CatalogueGameCard>,
        platform_filters: Vec<CataloguePlatformFilter>,
    ) -> Self {
        Self {
            games,
            platform_filters,
        }
    }

    pub fn games(&self) -> &[CatalogueGameCard] {
        &self.games
    }

    pub fn platform_filters(&self) -> &[CataloguePlatformFilter] {
        &self.platform_filters
    }
}

/// Application-owned read boundary over durable game snapshots.
pub trait GameCatalogueReadPort {
    type Error;

    fn list_catalogue(&mut self, query: &CatalogueQuery) -> Result<CataloguePage, Self::Error>;

    fn game_detail(
        &mut self,
        source_product_id: SourceProductId,
    ) -> Result<Option<CatalogueGameDetail>, Self::Error>;

    fn cover_image(
        &mut self,
        source_product_id: SourceProductId,
    ) -> Result<Option<StoredCoverImage>, Self::Error>;
}

/// Application-owned readiness boundary for the configured durable store.
///
/// Implementations must not schedule work, invoke a source, or expose
/// operational details through their error type.
pub trait ServiceReadinessPort: Send + Sync {
    type Error;

    fn check_readiness(&self) -> Result<(), Self::Error>;
}

/// Read one deterministic catalogue page through the application-owned port.
pub fn load_catalogue<P>(port: &mut P, query: &CatalogueQuery) -> Result<CataloguePage, P::Error>
where
    P: GameCatalogueReadPort,
{
    port.list_catalogue(query)
}

/// Read one persisted game detail through the application-owned port.
pub fn load_catalogue_game<P>(
    port: &mut P,
    source_product_id: SourceProductId,
) -> Result<Option<CatalogueGameDetail>, P::Error>
where
    P: GameCatalogueReadPort,
{
    port.game_detail(source_product_id)
}

/// Read one local cover asset through the application-owned catalogue boundary.
pub fn load_catalogue_cover<P>(
    port: &mut P,
    source_product_id: SourceProductId,
) -> Result<Option<StoredCoverImage>, P::Error>
where
    P: GameCatalogueReadPort,
{
    port.cover_image(source_product_id)
}

/// Persist one previously validated inner snapshot through the application-owned boundary.
pub fn upsert_game_snapshot<S>(store: &mut S, snapshot: &GameSnapshot) -> Result<(), S::Error>
where
    S: GameSnapshotStore,
{
    store.upsert_snapshot(snapshot)
}

/// One kind-separated, bounded source input together with the durable content hash used for
/// summary freshness. The hash is over the exact retained excerpt bytes, retained polarity, and
/// source kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewInput {
    source_product_id: SourceProductId,
    kind: ReviewKind,
    excerpts: Vec<ReviewExcerpt>,
    content_hash: ReviewContentHash,
}

impl ReviewInput {
    pub fn new(
        source_product_id: SourceProductId,
        kind: ReviewKind,
        excerpts: Vec<ReviewExcerpt>,
    ) -> Result<Self, ReviewInputError> {
        if excerpts.len() > REVIEW_INPUT_LIMIT {
            return Err(ReviewInputError::TooManyExcerpts);
        }
        let content_hash = ReviewContentHash::for_input(kind, &excerpts);
        Ok(Self {
            source_product_id,
            kind,
            excerpts,
            content_hash,
        })
    }

    pub const fn source_product_id(&self) -> SourceProductId {
        self.source_product_id
    }

    pub const fn kind(&self) -> ReviewKind {
        self.kind
    }

    pub fn excerpts(&self) -> &[ReviewExcerpt] {
        &self.excerpts
    }

    pub fn content_hash(&self) -> &ReviewContentHash {
        &self.content_hash
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewInputError {
    TooManyExcerpts,
}

impl fmt::Display for ReviewInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyExcerpts => {
                formatter.write_str("review input exceeds the first-page bound")
            }
        }
    }
}

impl std::error::Error for ReviewInputError {}

/// A lowercase SHA-256 content digest stored independently for each review kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewContentHash(String);

impl ReviewContentHash {
    fn for_input(kind: ReviewKind, excerpts: &[ReviewExcerpt]) -> Self {
        if excerpts.iter().all(|excerpt| excerpt.polarity().is_none()) {
            return Self::legacy_for_input(kind, excerpts);
        }

        let mut bytes = Vec::new();
        append_hash_field(&mut bytes, b"gamepulse-review-input:v2");
        append_hash_field(&mut bytes, kind.as_str().as_bytes());
        for excerpt in excerpts {
            append_hash_field(&mut bytes, excerpt.as_str().as_bytes());
            append_hash_field(
                &mut bytes,
                excerpt
                    .polarity()
                    .map(ReviewPolarity::as_str)
                    .unwrap_or("unknown")
                    .as_bytes(),
            );
        }
        Self(sha256_hex(&bytes))
    }

    fn legacy_for_input(kind: ReviewKind, excerpts: &[ReviewExcerpt]) -> Self {
        let mut bytes = Vec::new();
        append_hash_field(&mut bytes, kind.as_str().as_bytes());
        for excerpt in excerpts {
            append_hash_field(&mut bytes, excerpt.as_str().as_bytes());
        }
        Self(sha256_hex(&bytes))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The SHA-256 digest of the two kind-specific input hashes. It is the freshness fence for both
/// durable summary jobs and summary output writes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewRefreshFingerprint(String);

impl ReviewRefreshFingerprint {
    fn for_inputs(critic: &ReviewInput, user: &ReviewInput) -> Self {
        let mut bytes = Vec::new();
        append_hash_field(&mut bytes, b"critic");
        append_hash_field(&mut bytes, critic.content_hash().as_str().as_bytes());
        append_hash_field(&mut bytes, b"user");
        append_hash_field(&mut bytes, user.content_hash().as_str().as_bytes());
        Self(sha256_hex(&bytes))
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, ReviewRefreshFingerprintError> {
        let value = value.into();
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ReviewRefreshFingerprintError::Malformed);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewRefreshFingerprintError {
    Malformed,
}

impl fmt::Display for ReviewRefreshFingerprintError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("review refresh fingerprint must be a SHA-256 hex digest")
    }
}

impl std::error::Error for ReviewRefreshFingerprintError {}

/// The complete atomic persistence unit produced by a source refresh.
#[derive(Clone, Debug, PartialEq)]
pub struct GameReviewRefresh {
    snapshot: GameSnapshot,
    critic: ReviewInput,
    user: ReviewInput,
    fingerprint: ReviewRefreshFingerprint,
    jobs: Vec<JobRequest>,
}

impl GameReviewRefresh {
    pub fn new(
        snapshot: GameSnapshot,
        critic: ReviewInput,
        user: ReviewInput,
        schedule: ReviewSummaryJobSchedule,
        created_at: JobTimestamp,
    ) -> Result<Self, GameReviewRefreshError> {
        if critic.kind() != ReviewKind::Critic || user.kind() != ReviewKind::User {
            return Err(GameReviewRefreshError::ReviewKindsMustBeSeparate);
        }
        let source_product_id = snapshot.source_product_id();
        if critic.source_product_id() != source_product_id
            || user.source_product_id() != source_product_id
        {
            return Err(GameReviewRefreshError::ReviewInputGameMismatch);
        }
        let fingerprint = ReviewRefreshFingerprint::for_inputs(&critic, &user);
        let jobs = schedule
            .requests_for(source_product_id, &fingerprint, created_at)
            .map_err(GameReviewRefreshError::JobSchedule)?;
        Ok(Self {
            snapshot,
            critic,
            user,
            fingerprint,
            jobs,
        })
    }

    pub fn snapshot(&self) -> &GameSnapshot {
        &self.snapshot
    }

    pub fn input(&self, kind: ReviewKind) -> &ReviewInput {
        match kind {
            ReviewKind::Critic => &self.critic,
            ReviewKind::User => &self.user,
        }
    }

    pub fn fingerprint(&self) -> &ReviewRefreshFingerprint {
        &self.fingerprint
    }

    pub fn jobs(&self) -> &[JobRequest] {
        &self.jobs
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GameReviewRefreshError {
    ReviewKindsMustBeSeparate,
    ReviewInputGameMismatch,
    JobSchedule(ReviewSummaryJobScheduleError),
}

impl fmt::Display for GameReviewRefreshError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReviewKindsMustBeSeparate => {
                formatter.write_str("critic and user review inputs must stay separate")
            }
            Self::ReviewInputGameMismatch => {
                formatter.write_str("review input identity must match its game snapshot")
            }
            Self::JobSchedule(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for GameReviewRefreshError {}

/// Application-owned durable boundary for the all-or-nothing M011 refresh commit.
pub trait GameReviewRefreshStore {
    type Error;

    fn persist_review_refresh(&mut self, refresh: &GameReviewRefresh) -> Result<(), Self::Error>;
}

/// Persist a complete source snapshot, both review inputs, and both summary jobs atomically.
pub fn persist_game_review_refresh<S>(
    store: &mut S,
    refresh: &GameReviewRefresh,
) -> Result<(), S::Error>
where
    S: GameReviewRefreshStore,
{
    store.persist_review_refresh(refresh)
}

/// Canonical work request for one fingerprint-fenced review summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewSummaryRequest {
    source_product_id: SourceProductId,
    kind: ReviewKind,
    fingerprint: ReviewRefreshFingerprint,
}

impl ReviewSummaryRequest {
    pub fn new(
        source_product_id: SourceProductId,
        kind: ReviewKind,
        fingerprint: ReviewRefreshFingerprint,
    ) -> Self {
        Self {
            source_product_id,
            kind,
            fingerprint,
        }
    }

    pub fn from_work_reference(value: &str) -> Result<Self, ReviewSummaryRequestError> {
        let encoded = value
            .strip_prefix(REVIEW_SUMMARY_WORK_REFERENCE_PREFIX)
            .ok_or(ReviewSummaryRequestError::MalformedWorkReference)?;
        let mut parts = encoded.split(':');
        let product_id = parts
            .next()
            .ok_or(ReviewSummaryRequestError::MalformedWorkReference)?;
        let kind = parts
            .next()
            .and_then(ReviewKind::parse)
            .ok_or(ReviewSummaryRequestError::MalformedWorkReference)?;
        let fingerprint = parts
            .next()
            .ok_or(ReviewSummaryRequestError::MalformedWorkReference)?;
        if parts.next().is_some()
            || product_id.is_empty()
            || !product_id.bytes().all(|byte| byte.is_ascii_digit())
            || (product_id.len() > 1 && product_id.starts_with('0'))
        {
            return Err(ReviewSummaryRequestError::MalformedWorkReference);
        }
        let source_product_id = product_id
            .parse::<u64>()
            .ok()
            .and_then(|value| SourceProductId::new(value).ok())
            .ok_or(ReviewSummaryRequestError::MalformedWorkReference)?;
        let fingerprint = ReviewRefreshFingerprint::parse(fingerprint)
            .map_err(|_| ReviewSummaryRequestError::MalformedWorkReference)?;
        Ok(Self::new(source_product_id, kind, fingerprint))
    }

    pub const fn source_product_id(&self) -> SourceProductId {
        self.source_product_id
    }

    pub const fn kind(&self) -> ReviewKind {
        self.kind
    }

    pub fn fingerprint(&self) -> &ReviewRefreshFingerprint {
        &self.fingerprint
    }

    pub fn work_reference(&self) -> String {
        format!(
            "{REVIEW_SUMMARY_WORK_REFERENCE_PREFIX}{}:{}:{}",
            self.source_product_id.value(),
            self.kind.as_str(),
            self.fingerprint.as_str()
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewSummaryRequestError {
    MalformedWorkReference,
}

impl fmt::Display for ReviewSummaryRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("malformed review summary work reference")
    }
}

impl std::error::Error for ReviewSummaryRequestError {}

/// One persisted summary result. Unavailable is deliberate output for an empty input, never a
/// fabricated positive or negative statement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewSummaryOutput {
    Unavailable,
    Available {
        likes: Vec<String>,
        dislikes: Vec<String>,
    },
}

impl ReviewSummaryOutput {
    pub fn available(
        likes: Vec<String>,
        dislikes: Vec<String>,
    ) -> Result<Self, ReviewSummaryOutputError> {
        if likes.len() > 3 || dislikes.len() > 3 {
            return Err(ReviewSummaryOutputError::TooManyItems);
        }
        for item in likes.iter().chain(dislikes.iter()) {
            if item.trim().is_empty() || item.len() > REVIEW_EXCERPT_MAX_BYTES {
                return Err(ReviewSummaryOutputError::InvalidItem);
            }
        }
        Ok(Self::Available { likes, dislikes })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewSummaryOutputError {
    TooManyItems,
    InvalidItem,
}

impl fmt::Display for ReviewSummaryOutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyItems => formatter.write_str("review summary has too many items"),
            Self::InvalidItem => formatter.write_str("review summary item is invalid"),
        }
    }
}

impl std::error::Error for ReviewSummaryOutputError {}

/// A provider-agnostic summary request and its exact freshness fence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewSummary {
    request: ReviewSummaryRequest,
    output: ReviewSummaryOutput,
}

impl ReviewSummary {
    pub fn new(request: ReviewSummaryRequest, output: ReviewSummaryOutput) -> Self {
        Self { request, output }
    }

    pub fn request(&self) -> &ReviewSummaryRequest {
        &self.request
    }

    pub fn output(&self) -> &ReviewSummaryOutput {
        &self.output
    }
}

/// The summarizer boundary is deliberately free of provider names, configuration, and SDK types.
pub trait ReviewSummarizer: Send + Sync {
    type Error;

    fn summarize(&self, input: &ReviewInput) -> Result<ReviewSummaryOutput, Self::Error>;
}

/// Durable read/write boundary for the local summary worker. The adapter must only apply a write
/// when the request fingerprint is still current for that game and kind.
pub trait ReviewSummaryStore {
    type Error;

    fn load_review_input(
        &mut self,
        request: &ReviewSummaryRequest,
    ) -> Result<Option<ReviewInput>, Self::Error>;

    fn persist_review_summary(
        &mut self,
        summary: &ReviewSummary,
    ) -> Result<FencedSummaryWrite, Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FencedSummaryWrite {
    Applied,
    Stale,
}

/// The canonical opaque work-reference prefix for one local review-summary job.
pub const REVIEW_SUMMARY_WORK_REFERENCE_PREFIX: &str = "review-summary:";

/// Application-owned policy for exactly one summary job per review kind and refresh fingerprint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReviewSummaryJobSchedule {
    max_attempts: u32,
}

impl ReviewSummaryJobSchedule {
    pub fn new(max_attempts: u32) -> Result<Self, JobInputError> {
        if max_attempts == 0 {
            return Err(JobInputError::ZeroMaxAttempts);
        }
        Ok(Self { max_attempts })
    }

    pub fn requests_for(
        self,
        source_product_id: SourceProductId,
        fingerprint: &ReviewRefreshFingerprint,
        created_at: JobTimestamp,
    ) -> Result<Vec<JobRequest>, ReviewSummaryJobScheduleError> {
        ReviewKind::ALL
            .into_iter()
            .map(|kind| {
                let request =
                    ReviewSummaryRequest::new(source_product_id, kind, fingerprint.clone());
                JobRequest::new(
                    format!(
                        "{}:{}:{}:{}",
                        RuntimeJobType::LlmReviewSummary.as_str(),
                        source_product_id.value(),
                        kind.as_str(),
                        fingerprint.as_str()
                    ),
                    RuntimeJobType::LlmReviewSummary.as_str(),
                    request.work_reference(),
                    self.max_attempts,
                    created_at,
                )
                .map_err(ReviewSummaryJobScheduleError::JobRequest)
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewSummaryJobScheduleError {
    JobRequest(JobInputError),
}

impl fmt::Display for ReviewSummaryJobScheduleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::JobRequest(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ReviewSummaryJobScheduleError {}

fn append_hash_field(target: &mut Vec<u8>, value: &[u8]) {
    target.extend_from_slice(&(value.len() as u64).to_be_bytes());
    target.extend_from_slice(value);
}

fn sha256_hex(input: &[u8]) -> String {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut state = INITIAL;
    for chunk in padded.chunks_exact(64) {
        let mut words = [0_u32; 64];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes(
                chunk[index * 4..index * 4 + 4]
                    .try_into()
                    .expect("chunk width"),
            );
        }
        for index in 16..64 {
            let small0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let small1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(small0)
                .wrapping_add(words[index - 7])
                .wrapping_add(small1);
        }
        let mut working = state;
        for index in 0..64 {
            let choice = (working[4] & working[5]) ^ ((!working[4]) & working[6]);
            let major =
                (working[0] & working[1]) ^ (working[0] & working[2]) ^ (working[1] & working[2]);
            let big1 = working[4].rotate_right(6)
                ^ working[4].rotate_right(11)
                ^ working[4].rotate_right(25);
            let big0 = working[0].rotate_right(2)
                ^ working[0].rotate_right(13)
                ^ working[0].rotate_right(22);
            let temporary1 = working[7]
                .wrapping_add(big1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let temporary2 = big0.wrapping_add(major);
            working = [
                temporary1.wrapping_add(temporary2),
                working[0],
                working[1],
                working[2],
                working[3].wrapping_add(temporary1),
                working[4],
                working[5],
                working[6],
            ];
        }
        for (value, result) in state.iter_mut().zip(working) {
            *value = value.wrapping_add(result);
        }
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

/// A validated, source-agnostic input for one durable game-ingestion job.
///
/// The numeric product identity and source slug are carried together because the identity is
/// stable while the slug remains mutable routing data. Source adapters apply any source-specific
/// slug grammar before issuing a request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceIngestionRequest {
    source_product_id: SourceProductId,
    source_slug: String,
}

impl SourceIngestionRequest {
    pub fn new(
        source_product_id: u64,
        source_slug: impl Into<String>,
    ) -> Result<Self, SourceIngestionRequestError> {
        let source_slug = source_slug.into();
        if source_slug.is_empty() || source_slug.contains(':') {
            return Err(SourceIngestionRequestError::InvalidSourceSlug);
        }
        Ok(Self {
            source_product_id: SourceProductId::new(source_product_id)
                .map_err(|_| SourceIngestionRequestError::InvalidSourceProductId)?,
            source_slug,
        })
    }

    /// Decode the canonical work reference emitted by `SourceIngestionJobSchedule`.
    pub fn from_work_reference(work_reference: &str) -> Result<Self, SourceIngestionRequestError> {
        let encoded = work_reference
            .strip_prefix(SOURCE_INGESTION_WORK_REFERENCE_PREFIX)
            .ok_or(SourceIngestionRequestError::MalformedWorkReference)?;
        let (source_product_id, source_slug) = encoded
            .split_once(':')
            .ok_or(SourceIngestionRequestError::MalformedWorkReference)?;
        if source_product_id.is_empty()
            || !source_product_id.bytes().all(|byte| byte.is_ascii_digit())
            || (source_product_id.len() > 1 && source_product_id.starts_with('0'))
        {
            return Err(SourceIngestionRequestError::MalformedWorkReference);
        }
        let source_product_id = source_product_id
            .parse::<u64>()
            .map_err(|_| SourceIngestionRequestError::MalformedWorkReference)?;
        Self::new(source_product_id, source_slug)
    }

    pub fn source_product_id(&self) -> SourceProductId {
        self.source_product_id
    }

    pub fn source_slug(&self) -> &str {
        &self.source_slug
    }

    pub fn work_reference(&self) -> String {
        format!(
            "{SOURCE_INGESTION_WORK_REFERENCE_PREFIX}{}:{}",
            self.source_product_id.value(),
            self.source_slug
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceIngestionRequestError {
    InvalidSourceProductId,
    InvalidSourceSlug,
    MalformedWorkReference,
}

impl fmt::Display for SourceIngestionRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSourceProductId => {
                formatter.write_str("source ingestion product identity must be a positive number")
            }
            Self::InvalidSourceSlug => {
                formatter.write_str("source ingestion slug must be non-empty and colon-free")
            }
            Self::MalformedWorkReference => {
                formatter.write_str("malformed source ingestion work reference")
            }
        }
    }
}

impl std::error::Error for SourceIngestionRequestError {}

/// Application-owned async source boundary for one fully mapped game snapshot.
///
/// The outer source adapter owns source-native requests, parsing, identity validation, and
/// mapping. This port intentionally exposes only validated inner values to the use case.
pub trait AsyncSourceIngestionPort: Send + Sync {
    type Error;
    type IngestFuture<'a>: Future<Output = Result<GameSnapshot, Self::Error>> + Send + 'a
    where
        Self: 'a;

    fn ingest(&self, request: SourceIngestionRequest) -> Self::IngestFuture<'_>;
}

/// Invoke one source ingestion and atomically persist its resulting snapshot through the
/// application-owned snapshot boundary.
///
/// The caller supplies a short persistence closure so an outer worker can acquire a concrete
/// SQLite mutex only after the source future resolves.
pub async fn execute_async_source_ingestion<P, C, StoreError>(
    source_port: &P,
    persist_snapshot: C,
    request: SourceIngestionRequest,
) -> Result<(), SourceIngestionError<P::Error, StoreError>>
where
    P: AsyncSourceIngestionPort,
    C: FnOnce(&GameSnapshot) -> Result<(), StoreError>,
{
    let snapshot = source_port
        .ingest(request)
        .await
        .map_err(SourceIngestionError::Source)?;
    persist_snapshot(&snapshot).map_err(SourceIngestionError::Store)
}

/// The opaque application outcome categories for source ingestion.
#[derive(Debug)]
pub enum SourceIngestionError<SourceError, StoreError> {
    Source(SourceError),
    Store(StoreError),
}

/// Fully mapped source output for M011: one snapshot plus independently typed critic and user
/// inputs. Outer source adapters may not return anonymous or combined review collections.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewSourceIngestion {
    snapshot: GameSnapshot,
    critic: ReviewInput,
    user: ReviewInput,
}

impl ReviewSourceIngestion {
    pub fn new(
        snapshot: GameSnapshot,
        critic: ReviewInput,
        user: ReviewInput,
    ) -> Result<Self, GameReviewRefreshError> {
        if critic.kind() != ReviewKind::Critic || user.kind() != ReviewKind::User {
            return Err(GameReviewRefreshError::ReviewKindsMustBeSeparate);
        }
        if critic.source_product_id() != snapshot.source_product_id()
            || user.source_product_id() != snapshot.source_product_id()
        {
            return Err(GameReviewRefreshError::ReviewInputGameMismatch);
        }
        Ok(Self {
            snapshot,
            critic,
            user,
        })
    }

    pub fn snapshot(&self) -> &GameSnapshot {
        &self.snapshot
    }

    pub fn critic(&self) -> &ReviewInput {
        &self.critic
    }

    pub fn user(&self) -> &ReviewInput {
        &self.user
    }

    pub fn into_parts(self) -> (GameSnapshot, ReviewInput, ReviewInput) {
        (self.snapshot, self.critic, self.user)
    }
}

/// Application-owned async source boundary for a snapshot and its two separated review inputs.
pub trait AsyncReviewSourceIngestionPort: Send + Sync {
    type Error;
    type IngestFuture<'a>: Future<Output = Result<ReviewSourceIngestion, Self::Error>> + Send + 'a
    where
        Self: 'a;

    fn ingest_reviews(&self, request: SourceIngestionRequest) -> Self::IngestFuture<'_>;
}

/// Await source work before acquiring the concrete durable store, then atomically publish the
/// snapshot, review inputs, and derived jobs through the application-owned refresh boundary.
pub async fn execute_async_review_source_ingestion<P, C, StoreError>(
    source_port: &P,
    persist_refresh: C,
    request: SourceIngestionRequest,
    schedule: ReviewSummaryJobSchedule,
    created_at: JobTimestamp,
) -> Result<(), ReviewSourceIngestionError<P::Error, StoreError>>
where
    P: AsyncReviewSourceIngestionPort,
    C: FnOnce(&GameReviewRefresh) -> Result<(), StoreError>,
{
    let ingestion = source_port
        .ingest_reviews(request)
        .await
        .map_err(ReviewSourceIngestionError::Source)?;
    let (snapshot, critic, user) = ingestion.into_parts();
    let refresh = GameReviewRefresh::new(snapshot, critic, user, schedule, created_at)
        .map_err(ReviewSourceIngestionError::InvalidRefresh)?;
    persist_refresh(&refresh).map_err(ReviewSourceIngestionError::Store)
}

#[derive(Debug)]
pub enum ReviewSourceIngestionError<SourceError, StoreError> {
    Source(SourceError),
    InvalidRefresh(GameReviewRefreshError),
    Store(StoreError),
}

/// A compact, source-adapter-mapped candidate. The slug is opaque to this policy;
/// daily uniqueness always uses `source_product_id`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryCandidate {
    source_product_id: SourceProductId,
    source_slug: String,
}

impl DiscoveryCandidate {
    pub fn new(
        source_product_id: u64,
        source_slug: impl Into<String>,
    ) -> Result<Self, DiscoveryCandidateError> {
        let source_slug = source_slug.into();
        if source_slug.is_empty() {
            return Err(DiscoveryCandidateError::EmptySourceSlug);
        }

        Ok(Self {
            source_product_id: SourceProductId::new(source_product_id)
                .map_err(DiscoveryCandidateError::InvalidSourceProductId)?,
            source_slug,
        })
    }

    pub fn source_product_id(&self) -> SourceProductId {
        self.source_product_id
    }

    pub fn source_slug(&self) -> &str {
        &self.source_slug
    }
}

/// A source result reduced to the fields the daily selection policy needs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryPage {
    candidates: Vec<DiscoveryCandidate>,
    next_browse_cursor: Option<BrowseCursor>,
}

impl DiscoveryPage {
    pub fn new(
        candidates: Vec<DiscoveryCandidate>,
        next_browse_cursor: Option<BrowseCursor>,
    ) -> Self {
        Self {
            candidates,
            next_browse_cursor,
        }
    }

    pub fn candidates(&self) -> &[DiscoveryCandidate] {
        &self.candidates
    }

    pub fn next_browse_cursor(&self) -> Option<BrowseCursor> {
        self.next_browse_cursor
    }
}

/// The source-worker boundary. Implementations map source-native DTOs into `DiscoveryPage`.
pub trait DailyCrawlSourcePort {
    type Error;

    fn discover(&mut self, request: CrawlDiscoveryRequest) -> Result<DiscoveryPage, Self::Error>;
}

/// The asynchronous source-worker boundary for an hourly discovery attempt.
///
/// The application owns request selection and durable commit policy. Outer source adapters own
/// request execution, source-native parsing, and mapping into `DiscoveryPage`; this future is
/// intentionally expressed with `std::future::Future` so the application remains independent of
/// a particular async runtime.
pub trait AsyncDailyCrawlSourcePort: Send + Sync {
    type Error;
    type DiscoverFuture<'a>: Future<Output = Result<DiscoveryPage, Self::Error>> + Send + 'a
    where
        Self: 'a;

    fn discover(&self, request: CrawlDiscoveryRequest) -> Self::DiscoverFuture<'_>;
}

/// The current source page expected by a durable mandatory run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableRunDiscovery {
    run_id: String,
    request: CrawlDiscoveryRequest,
    version: u64,
}

impl DurableRunDiscovery {
    pub fn new(
        run_id: impl Into<String>,
        request: CrawlDiscoveryRequest,
        version: u64,
    ) -> Result<Self, DurableRunDiscoveryError> {
        let run_id = run_id.into();
        if run_id.is_empty() || run_id.len() > 128 || run_id.contains(':') {
            return Err(DurableRunDiscoveryError::InvalidRunId);
        }
        Ok(Self {
            run_id,
            request,
            version,
        })
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub const fn request(&self) -> CrawlDiscoveryRequest {
        self.request
    }

    pub const fn version(&self) -> u64 {
        self.version
    }

    pub fn progress_work_reference(&self) -> String {
        format!(
            "{RUN_PROGRESS_WORK_REFERENCE_PREFIX}{}:{}",
            self.run_id, self.version
        )
    }

    pub fn from_progress_work_reference(value: &str) -> Result<Self, DurableRunDiscoveryError> {
        let encoded = value
            .strip_prefix(RUN_PROGRESS_WORK_REFERENCE_PREFIX)
            .ok_or(DurableRunDiscoveryError::MalformedWorkReference)?;
        let (run_id, version) = encoded
            .split_once(':')
            .ok_or(DurableRunDiscoveryError::MalformedWorkReference)?;
        if version.is_empty()
            || !version.bytes().all(|byte| byte.is_ascii_digit())
            || (version.len() > 1 && version.starts_with('0'))
        {
            return Err(DurableRunDiscoveryError::MalformedWorkReference);
        }
        let version = version
            .parse::<u64>()
            .map_err(|_| DurableRunDiscoveryError::MalformedWorkReference)?;
        // The exact request is recovered from durable run state, not from the opaque work ref.
        Self::new(run_id, CrawlDiscoveryRequest::NewReleases, version)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurableRunDiscoveryError {
    InvalidRunId,
    MalformedWorkReference,
}

impl fmt::Display for DurableRunDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRunId => {
                formatter.write_str("run identifier must be bounded and colon-free")
            }
            Self::MalformedWorkReference => {
                formatter.write_str("malformed durable run progress work reference")
            }
        }
    }
}

impl std::error::Error for DurableRunDiscoveryError {}

/// Safe terminal outcomes for durable run progression.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurableRunProgressOutcome {
    Progressed,
    AlreadyTerminal,
    DeadlineExceeded,
    SourceExhausted,
}

/// Application-owned port for the mandatory exact-target run lifecycle.
///
/// The adapter must atomically bind a run item state transition, derived source job scheduling,
/// and (for a completed item) the game/review refresh. New durable control metadata may retain
/// only stable identity, routing slug, lifecycle, fencing/version data, and fixed rejection
/// categories; raw source material and errors are forbidden.
#[allow(clippy::too_many_arguments)]
pub trait DurableRunProgressStore {
    type Error;

    fn begin_or_resume(
        &mut self,
        day: &CrawlDayKey,
        target: usize,
        created_at: JobTimestamp,
        deadline_at: JobTimestamp,
        job_identity: &str,
        claim_fence: JobClaimFence,
        now: JobTimestamp,
    ) -> Result<Option<DurableRunDiscovery>, Self::Error>;

    fn load_progress_discovery(
        &mut self,
        run_id: &str,
        version: u64,
        job_identity: &str,
        claim_fence: JobClaimFence,
        now: JobTimestamp,
    ) -> Result<Option<DurableRunDiscovery>, Self::Error>;

    fn record_discovery_page(
        &mut self,
        discovery: &DurableRunDiscovery,
        page: &DiscoveryPage,
        schedule: SourceIngestionJobSchedule,
        created_at: JobTimestamp,
        job_identity: &str,
        claim_fence: JobClaimFence,
        now: JobTimestamp,
    ) -> Result<DurableRunProgressOutcome, Self::Error>;

    fn persist_completed_item(
        &mut self,
        request: &RunSourceIngestionRequest,
        job_identity: &str,
        refresh: &GameReviewRefresh,
        schedule: SourceIngestionJobSchedule,
        created_at: JobTimestamp,
        claim_fence: JobClaimFence,
        now: JobTimestamp,
    ) -> Result<DurableRunProgressOutcome, Self::Error>;

    fn reject_missing_required_video(
        &mut self,
        request: &RunSourceIngestionRequest,
        job_identity: &str,
        schedule: SourceIngestionJobSchedule,
        created_at: JobTimestamp,
        claim_fence: JobClaimFence,
        now: JobTimestamp,
    ) -> Result<DurableRunProgressOutcome, Self::Error>;

    fn reject_source_unavailable(
        &mut self,
        request: &RunSourceIngestionRequest,
        job_identity: &str,
        schedule: SourceIngestionJobSchedule,
        created_at: JobTimestamp,
        claim_fence: JobClaimFence,
        now: JobTimestamp,
    ) -> Result<DurableRunProgressOutcome, Self::Error>;
}

/// The sole persistence boundary for this milestone.
///
/// `commit` must make the next daily state and its selected candidates visible together, or make
/// neither visible. M003 provides no concrete durable implementation.
pub trait DailyCrawlStatePort {
    type Error;

    fn load(&mut self, day: &CrawlDayKey) -> Result<Option<DailyCrawlState>, Self::Error>;

    fn commit(&mut self, commit: DailyCrawlCommit) -> Result<(), Self::Error>;
}

/// The application-owned data atomically committed after a successful selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DailyCrawlCommit {
    expected_previous_state: Option<DailyCrawlState>,
    state: DailyCrawlState,
    selected: Vec<DiscoveryCandidate>,
    jobs: Vec<JobRequest>,
}

impl DailyCrawlCommit {
    /// Construct a validated application-owned commit for a persistence adapter.
    pub fn new(
        expected_previous_state: Option<DailyCrawlState>,
        state: DailyCrawlState,
        selected: Vec<DiscoveryCandidate>,
    ) -> Result<Self, DailyCrawlCommitError> {
        let mut selected_ids = BTreeSet::new();
        for candidate in &selected {
            let source_product_id = candidate.source_product_id();
            if !state.selected_or_processed().contains(&source_product_id) {
                return Err(DailyCrawlCommitError::SelectedCandidateAbsentFromState);
            }
            if !selected_ids.insert(source_product_id) {
                return Err(DailyCrawlCommitError::DuplicateSelectedCandidate);
            }
        }

        if let Some(previous_state) = &expected_previous_state {
            if previous_state.day() != state.day() {
                return Err(DailyCrawlCommitError::ExpectedPreviousDayMismatch);
            }
            if !previous_state
                .selected_or_processed()
                .is_subset(state.selected_or_processed())
            {
                return Err(DailyCrawlCommitError::SelectedOrProcessedRegression);
            }
            if previous_state.new_releases_completed() && !state.new_releases_completed() {
                return Err(DailyCrawlCommitError::NewReleasesCompletionRegression);
            }
            if matches!(previous_state.browse_progress(), BrowseProgress::Exhausted)
                && !matches!(state.browse_progress(), BrowseProgress::Exhausted)
            {
                return Err(DailyCrawlCommitError::BrowseExhaustionRegression);
            }
        }

        Ok(Self {
            expected_previous_state,
            state,
            selected,
            jobs: Vec::new(),
        })
    }

    pub fn expected_previous_state(&self) -> Option<&DailyCrawlState> {
        self.expected_previous_state.as_ref()
    }

    pub fn state(&self) -> &DailyCrawlState {
        &self.state
    }

    pub fn selected(&self) -> &[DiscoveryCandidate] {
        &self.selected
    }

    /// Attach exactly one durable source-ingestion job for each selected candidate.
    ///
    /// The storage adapter must commit these jobs in the same transaction as the crawl state and
    /// selected candidates. The job identity is day-scoped, so replay is deduplicated while a
    /// later daily selection can reprocess the same numeric source product identity.
    pub fn with_source_ingestion_jobs(
        mut self,
        schedule: SourceIngestionJobSchedule,
        created_at: JobTimestamp,
    ) -> Result<Self, SourceIngestionJobScheduleError> {
        self.jobs = schedule.requests_for(self.state.day(), &self.selected, created_at)?;
        Ok(self)
    }

    pub fn jobs(&self) -> &[JobRequest] {
        &self.jobs
    }
}

/// A successful selection returned only after the application port commits it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DailyCrawlSelection {
    request: CrawlDiscoveryRequest,
    selected: Vec<DiscoveryCandidate>,
    state: DailyCrawlState,
}

impl DailyCrawlSelection {
    pub fn request(&self) -> CrawlDiscoveryRequest {
        self.request
    }

    pub fn selected(&self) -> &[DiscoveryCandidate] {
        &self.selected
    }

    pub fn state(&self) -> &DailyCrawlState {
        &self.state
    }
}

/// The result of one hourly selection attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DailyCrawlOutcome {
    Selected(DailyCrawlSelection),
    Exhausted(DailyCrawlState),
}

/// Plan, discover, select, and atomically publish one daily-crawl transition.
///
/// Discovery and commit failures return before any state transition is published.
pub fn execute_daily_crawl<S, D>(
    state_port: &mut S,
    source_port: &mut D,
    day: CrawlDayKey,
) -> Result<DailyCrawlOutcome, DailyCrawlError<S::Error, D::Error>>
where
    S: DailyCrawlStatePort,
    D: DailyCrawlSourcePort,
{
    let expected_previous_state = state_port.load(&day).map_err(DailyCrawlError::Load)?;
    let mut discovery = match prepare_daily_crawl(day.clone(), expected_previous_state.clone()) {
        DailyCrawlAction::Discover(discovery) => discovery,
        DailyCrawlAction::Exhausted(state) => return Ok(DailyCrawlOutcome::Exhausted(state)),
    };
    let initial_request = discovery.request();
    let mut selected = Vec::new();
    let mut browse_page_count = 0;

    loop {
        let request = discovery.request();
        count_browse_page(request, &mut browse_page_count)?;
        let page = source_port
            .discover(request)
            .map_err(DailyCrawlError::Source)?;
        let transition = select_daily_crawl_up_to(
            discovery,
            page.candidates()
                .iter()
                .map(DiscoveryCandidate::source_product_id),
            page.next_browse_cursor(),
            DAILY_CRAWL_SELECTION_LIMIT - selected.len(),
        );
        selected.extend(selected_candidates(&page, &transition));
        let state = transition.next_state().clone();

        if let Some(next_discovery) = next_browse_discovery(&day, request, &state, selected.len()) {
            discovery = next_discovery;
            continue;
        }

        if selected.len() != DAILY_CRAWL_SELECTION_LIMIT {
            return Ok(DailyCrawlOutcome::Exhausted(state));
        }

        let commit =
            DailyCrawlCommit::new(expected_previous_state, state.clone(), selected.clone())
                .map_err(DailyCrawlError::InvalidCommit)?;
        state_port.commit(commit).map_err(DailyCrawlError::Commit)?;

        return Ok(DailyCrawlOutcome::Selected(DailyCrawlSelection {
            request: initial_request,
            selected,
            state,
        }));
    }
}

/// Plan, asynchronously discover, select, and atomically publish one daily-crawl transition.
///
/// `load_state` completes before the source future is created, and `commit_state` runs only after
/// that future resolves. A worker can therefore acquire its SQLite mutex separately for load and
/// commit, without holding a lock or transaction across the awaited source request.
pub async fn execute_async_daily_crawl<L, C, D, StateError>(
    load_state: L,
    source_port: &D,
    commit_state: C,
    day: CrawlDayKey,
) -> Result<DailyCrawlOutcome, DailyCrawlError<StateError, D::Error>>
where
    L: FnOnce(&CrawlDayKey) -> Result<Option<DailyCrawlState>, StateError>,
    C: FnOnce(DailyCrawlCommit) -> Result<(), StateError>,
    D: AsyncDailyCrawlSourcePort,
{
    let expected_previous_state = load_state(&day).map_err(DailyCrawlError::Load)?;
    let mut discovery = match prepare_daily_crawl(day.clone(), expected_previous_state.clone()) {
        DailyCrawlAction::Discover(discovery) => discovery,
        DailyCrawlAction::Exhausted(state) => return Ok(DailyCrawlOutcome::Exhausted(state)),
    };
    let initial_request = discovery.request();
    let mut selected = Vec::new();
    let mut browse_page_count = 0;

    loop {
        let request = discovery.request();
        count_browse_page(request, &mut browse_page_count)?;
        let page = source_port
            .discover(request)
            .await
            .map_err(DailyCrawlError::Source)?;
        let transition = select_daily_crawl_up_to(
            discovery,
            page.candidates()
                .iter()
                .map(DiscoveryCandidate::source_product_id),
            page.next_browse_cursor(),
            DAILY_CRAWL_SELECTION_LIMIT - selected.len(),
        );
        selected.extend(selected_candidates(&page, &transition));
        let state = transition.next_state().clone();

        if let Some(next_discovery) = next_browse_discovery(&day, request, &state, selected.len()) {
            discovery = next_discovery;
            continue;
        }

        if selected.len() != DAILY_CRAWL_SELECTION_LIMIT {
            return Ok(DailyCrawlOutcome::Exhausted(state));
        }

        let commit =
            DailyCrawlCommit::new(expected_previous_state, state.clone(), selected.clone())
                .map_err(DailyCrawlError::InvalidCommit)?;
        commit_state(commit).map_err(DailyCrawlError::Commit)?;

        return Ok(DailyCrawlOutcome::Selected(DailyCrawlSelection {
            request: initial_request,
            selected,
            state,
        }));
    }
}

/// Plan, asynchronously discover, and atomically publish a daily transition with its derived
/// source-ingestion jobs.
///
/// Like `execute_async_daily_crawl`, the source future is awaited between the caller-controlled
/// load and commit closures. The commit closure receives one value that contains state,
/// candidates, and exactly one day-scoped source-ingestion job per selected candidate.
pub async fn execute_async_daily_crawl_with_source_ingestion_jobs<L, C, D, StateError>(
    load_state: L,
    source_port: &D,
    commit_state: C,
    day: CrawlDayKey,
    schedule: SourceIngestionJobSchedule,
    created_at: JobTimestamp,
) -> Result<DailyCrawlOutcome, DailyCrawlError<StateError, D::Error>>
where
    L: FnOnce(&CrawlDayKey) -> Result<Option<DailyCrawlState>, StateError>,
    C: FnOnce(DailyCrawlCommit) -> Result<(), StateError>,
    D: AsyncDailyCrawlSourcePort,
{
    let expected_previous_state = load_state(&day).map_err(DailyCrawlError::Load)?;
    let mut discovery = match prepare_daily_crawl(day.clone(), expected_previous_state.clone()) {
        DailyCrawlAction::Discover(discovery) => discovery,
        DailyCrawlAction::Exhausted(state) => return Ok(DailyCrawlOutcome::Exhausted(state)),
    };
    let initial_request = discovery.request();
    let mut selected = Vec::new();
    let mut browse_page_count = 0;

    loop {
        let request = discovery.request();
        count_browse_page(request, &mut browse_page_count)?;
        let page = source_port
            .discover(request)
            .await
            .map_err(DailyCrawlError::Source)?;
        let transition = select_daily_crawl_up_to(
            discovery,
            page.candidates()
                .iter()
                .map(DiscoveryCandidate::source_product_id),
            page.next_browse_cursor(),
            DAILY_CRAWL_SELECTION_LIMIT - selected.len(),
        );
        selected.extend(selected_candidates(&page, &transition));
        let state = transition.next_state().clone();

        if let Some(next_discovery) = next_browse_discovery(&day, request, &state, selected.len()) {
            discovery = next_discovery;
            continue;
        }

        if selected.len() != DAILY_CRAWL_SELECTION_LIMIT {
            return Ok(DailyCrawlOutcome::Exhausted(state));
        }

        let commit =
            DailyCrawlCommit::new(expected_previous_state, state.clone(), selected.clone())
                .map_err(DailyCrawlError::InvalidCommit)?
                .with_source_ingestion_jobs(schedule, created_at)
                .map_err(DailyCrawlError::JobSchedule)?;
        commit_state(commit).map_err(DailyCrawlError::Commit)?;

        return Ok(DailyCrawlOutcome::Selected(DailyCrawlSelection {
            request: initial_request,
            selected,
            state,
        }));
    }
}

fn count_browse_page<StateError, SourceError>(
    request: CrawlDiscoveryRequest,
    browse_page_count: &mut usize,
) -> Result<(), DailyCrawlError<StateError, SourceError>> {
    if matches!(request, CrawlDiscoveryRequest::NewestBrowse { .. }) {
        *browse_page_count += 1;
        if *browse_page_count > MAX_BROWSE_PAGES_PER_HOURLY_SELECTION {
            return Err(DailyCrawlError::BrowseContinuationLimit);
        }
    }
    Ok(())
}

fn next_browse_discovery(
    day: &CrawlDayKey,
    request: CrawlDiscoveryRequest,
    state: &DailyCrawlState,
    selected_count: usize,
) -> Option<gamepulse_domain::DailyCrawlDiscovery> {
    if selected_count == DAILY_CRAWL_SELECTION_LIMIT
        || matches!(state.browse_progress(), BrowseProgress::Exhausted)
    {
        return None;
    }

    let has_follow_up = match request {
        CrawlDiscoveryRequest::NewReleases => state.new_releases_completed(),
        CrawlDiscoveryRequest::NewestBrowse { .. } => true,
    };
    if !has_follow_up {
        return None;
    }

    match prepare_daily_crawl(day.clone(), Some(state.clone())) {
        DailyCrawlAction::Discover(discovery) => Some(discovery),
        DailyCrawlAction::Exhausted(_) => None,
    }
}

fn selected_candidates(
    page: &DiscoveryPage,
    transition: &DailyCrawlTransition,
) -> Vec<DiscoveryCandidate> {
    let selected_ids = transition
        .selected_product_ids()
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let mut emitted_ids = std::collections::BTreeSet::new();

    page.candidates()
        .iter()
        .filter(|candidate| {
            selected_ids.contains(&candidate.source_product_id())
                && emitted_ids.insert(candidate.source_product_id())
        })
        .cloned()
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryCandidateError {
    InvalidSourceProductId(SourceProductIdError),
    EmptySourceSlug,
}

impl fmt::Display for DiscoveryCandidateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSourceProductId(error) => error.fmt(formatter),
            Self::EmptySourceSlug => formatter.write_str("source slug must not be empty"),
        }
    }
}

impl std::error::Error for DiscoveryCandidateError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DailyCrawlCommitError {
    ExpectedPreviousDayMismatch,
    SelectedOrProcessedRegression,
    NewReleasesCompletionRegression,
    BrowseExhaustionRegression,
    SelectedCandidateAbsentFromState,
    DuplicateSelectedCandidate,
}

impl fmt::Display for DailyCrawlCommitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExpectedPreviousDayMismatch => {
                formatter.write_str("expected previous state must belong to the committed day")
            }
            Self::SelectedOrProcessedRegression => {
                formatter.write_str("selected or processed identities must not regress")
            }
            Self::NewReleasesCompletionRegression => {
                formatter.write_str("new releases completion must not regress")
            }
            Self::BrowseExhaustionRegression => {
                formatter.write_str("browse exhaustion must not regress")
            }
            Self::SelectedCandidateAbsentFromState => formatter
                .write_str("selected candidate identity must belong to the committed state"),
            Self::DuplicateSelectedCandidate => {
                formatter.write_str("selected candidate identities must be unique")
            }
        }
    }
}

impl std::error::Error for DailyCrawlCommitError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DailyCrawlError<StateError, SourceError> {
    Load(StateError),
    Source(SourceError),
    BrowseContinuationLimit,
    InvalidCommit(DailyCrawlCommitError),
    JobSchedule(SourceIngestionJobScheduleError),
    Commit(StateError),
}

/// The largest application-owned opaque queue value accepted at the boundary.
///
/// Job work references and failure descriptions are untrusted data. The queue
/// persists them for observability but never interprets or logs them.
pub const JOB_TEXT_MAX_BYTES: usize = 4_096;

/// A deterministic clock value supplied by the caller. Queue policy does not
/// read the system clock so claims, expiry, and retry behavior remain testable.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct JobTimestamp(i64);

impl JobTimestamp {
    pub fn new(value: i64) -> Result<Self, JobInputError> {
        if value < 0 {
            return Err(JobInputError::NegativeTimestamp);
        }
        Ok(Self(value))
    }

    pub const fn value(self) -> i64 {
        self.0
    }
}

/// The deterministic retry schedule shared by every durable queue adapter.
pub const RETRY_BACKOFF_BASE_SECONDS: i64 = 30;
pub const RETRY_BACKOFF_MAX_SECONDS: i64 = 300;

/// Return the bounded delay after a failed or expired attempt number.
pub fn retry_backoff_seconds(attempt_number: u32) -> i64 {
    let shift = attempt_number.saturating_sub(1).min(4);
    let exponential = RETRY_BACKOFF_BASE_SECONDS << shift;
    if exponential > RETRY_BACKOFF_MAX_SECONDS {
        RETRY_BACKOFF_MAX_SECONDS
    } else {
        exponential
    }
}

/// Compute the persisted eligibility time for a non-terminal retry.
pub fn retry_not_before(
    failed_at: JobTimestamp,
    attempt_number: u32,
) -> Result<JobTimestamp, JobInputError> {
    failed_at
        .value()
        .checked_add(retry_backoff_seconds(attempt_number))
        .map(JobTimestamp)
        .ok_or(JobInputError::RetryEligibilityOverflow)
}

/// An immutable job description supplied when work first enters the queue.
#[derive(Clone, Eq, PartialEq)]
pub struct JobRequest {
    identity: String,
    job_type: String,
    work_ref: String,
    max_attempts: u32,
    created_at: JobTimestamp,
}

impl JobRequest {
    pub fn new(
        identity: impl Into<String>,
        job_type: impl Into<String>,
        work_ref: impl Into<String>,
        max_attempts: u32,
        created_at: JobTimestamp,
    ) -> Result<Self, JobInputError> {
        let identity = identity.into();
        let job_type = job_type.into();
        let work_ref = work_ref.into();
        validate_job_text("job identity", &identity)?;
        validate_job_text("job type", &job_type)?;
        validate_job_text("job work reference", &work_ref)?;
        if max_attempts == 0 {
            return Err(JobInputError::ZeroMaxAttempts);
        }

        Ok(Self {
            identity,
            job_type,
            work_ref,
            max_attempts,
            created_at,
        })
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    pub fn job_type(&self) -> &str {
        &self.job_type
    }

    pub fn work_ref(&self) -> &str {
        &self.work_ref
    }

    pub const fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    pub const fn created_at(&self) -> JobTimestamp {
        self.created_at
    }
}

impl fmt::Debug for JobRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JobRequest")
            .field("identity", &self.identity)
            .field("job_type", &self.job_type)
            .field("work_ref_bytes", &self.work_ref.len())
            .field("max_attempts", &self.max_attempts)
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// A worker and deterministic lease duration used to claim one ready job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobClaimRequest {
    worker_id: String,
    claimed_at: JobTimestamp,
    lease_expires_at: JobTimestamp,
    pacing: Option<JobClaimPacing>,
}

/// A durable minimum interval between claims for one logical worker lane.
///
/// The queue persists the lane's next eligible claim time. This value is a
/// policy input, not an in-memory sleep instruction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobClaimPacing {
    lane_key: String,
    minimum_interval_seconds: i64,
}

impl JobClaimPacing {
    pub fn new(
        lane_key: impl Into<String>,
        minimum_interval_seconds: i64,
    ) -> Result<Self, JobInputError> {
        let lane_key = lane_key.into();
        validate_job_text("job lane key", &lane_key)?;
        if minimum_interval_seconds <= 0 {
            return Err(JobInputError::NonPositivePacingInterval);
        }
        Ok(Self {
            lane_key,
            minimum_interval_seconds,
        })
    }

    pub fn lane_key(&self) -> &str {
        &self.lane_key
    }

    pub const fn minimum_interval_seconds(&self) -> i64 {
        self.minimum_interval_seconds
    }

    pub fn next_claim_at(&self, claimed_at: JobTimestamp) -> Result<JobTimestamp, JobInputError> {
        claimed_at
            .value()
            .checked_add(self.minimum_interval_seconds)
            .map(JobTimestamp)
            .ok_or(JobInputError::PacingEligibilityOverflow)
    }
}

impl JobClaimRequest {
    pub fn new(
        worker_id: impl Into<String>,
        claimed_at: JobTimestamp,
        lease_duration: i64,
    ) -> Result<Self, JobInputError> {
        let worker_id = worker_id.into();
        validate_job_text("worker identity", &worker_id)?;
        if lease_duration <= 0 {
            return Err(JobInputError::NonPositiveLeaseDuration);
        }
        let lease_expires_at = claimed_at
            .value()
            .checked_add(lease_duration)
            .ok_or(JobInputError::LeaseExpiryOverflow)?;

        Ok(Self {
            worker_id,
            claimed_at,
            lease_expires_at: JobTimestamp(lease_expires_at),
            pacing: None,
        })
    }

    pub fn with_pacing(mut self, pacing: JobClaimPacing) -> Self {
        self.pacing = Some(pacing);
        self
    }

    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    pub const fn claimed_at(&self) -> JobTimestamp {
        self.claimed_at
    }

    pub const fn lease_expires_at(&self) -> JobTimestamp {
        self.lease_expires_at
    }

    pub fn pacing(&self) -> Option<&JobClaimPacing> {
        self.pacing.as_ref()
    }
}

/// The durable lifecycle state of one job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobStatus {
    Ready,
    Claimed,
    Succeeded,
    Failed,
}

impl JobStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed)
    }
}

/// The observable durable state of one deduplicated job.
#[derive(Clone, Eq, PartialEq)]
pub struct JobRecord {
    identity: String,
    job_type: String,
    work_ref: String,
    max_attempts: u32,
    attempt_count: u32,
    status: JobStatus,
    created_at: JobTimestamp,
    updated_at: JobTimestamp,
    retry_not_before: Option<JobTimestamp>,
    claimed_by: Option<String>,
    lease_expires_at: Option<JobTimestamp>,
    terminal_at: Option<JobTimestamp>,
    last_error: Option<String>,
}

impl JobRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn restored(
        identity: String,
        job_type: String,
        work_ref: String,
        max_attempts: u32,
        attempt_count: u32,
        status: JobStatus,
        created_at: JobTimestamp,
        updated_at: JobTimestamp,
        retry_not_before: Option<JobTimestamp>,
        claimed_by: Option<String>,
        lease_expires_at: Option<JobTimestamp>,
        terminal_at: Option<JobTimestamp>,
        last_error: Option<String>,
    ) -> Self {
        Self {
            identity,
            job_type,
            work_ref,
            max_attempts,
            attempt_count,
            status,
            created_at,
            updated_at,
            retry_not_before,
            claimed_by,
            lease_expires_at,
            terminal_at,
            last_error,
        }
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    pub fn job_type(&self) -> &str {
        &self.job_type
    }

    pub fn work_ref(&self) -> &str {
        &self.work_ref
    }

    pub const fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    pub const fn attempt_count(&self) -> u32 {
        self.attempt_count
    }

    pub const fn status(&self) -> JobStatus {
        self.status
    }

    pub const fn created_at(&self) -> JobTimestamp {
        self.created_at
    }

    pub const fn updated_at(&self) -> JobTimestamp {
        self.updated_at
    }

    pub const fn retry_not_before(&self) -> Option<JobTimestamp> {
        self.retry_not_before
    }

    pub fn claimed_by(&self) -> Option<&str> {
        self.claimed_by.as_deref()
    }

    pub const fn lease_expires_at(&self) -> Option<JobTimestamp> {
        self.lease_expires_at
    }

    pub const fn terminal_at(&self) -> Option<JobTimestamp> {
        self.terminal_at
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
}

impl fmt::Debug for JobRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JobRecord")
            .field("identity", &self.identity)
            .field("job_type", &self.job_type)
            .field("work_ref_bytes", &self.work_ref.len())
            .field("max_attempts", &self.max_attempts)
            .field("attempt_count", &self.attempt_count)
            .field("status", &self.status)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .field("retry_not_before", &self.retry_not_before)
            .field("claimed", &self.claimed_by.is_some())
            .field("lease_expires_at", &self.lease_expires_at)
            .field("terminal_at", &self.terminal_at)
            .field("has_last_error", &self.last_error.is_some())
            .finish()
    }
}

/// A single-use claim capability. The queue creates it after persisting the
/// claimed job and rejects completion or failure from an obsolete claim token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobClaim {
    identity: String,
    worker_id: String,
    claim_token: u32,
    claimed_at: JobTimestamp,
    lease_expires_at: JobTimestamp,
}

impl JobClaim {
    /// Reconstruct a claim capability from durable adapter state.
    pub fn restored(
        identity: String,
        worker_id: String,
        claim_token: u32,
        claimed_at: JobTimestamp,
        lease_expires_at: JobTimestamp,
    ) -> Self {
        Self {
            identity,
            worker_id,
            claim_token,
            claimed_at,
            lease_expires_at,
        }
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    pub const fn claim_token(&self) -> u32 {
        self.claim_token
    }

    pub const fn claimed_at(&self) -> JobTimestamp {
        self.claimed_at
    }

    pub const fn lease_expires_at(&self) -> JobTimestamp {
        self.lease_expires_at
    }
}

/// A claimed job together with the capability required to finish that attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimedJob {
    job: JobRecord,
    claim: JobClaim,
}

impl ClaimedJob {
    /// Create a claimed-job result after both the job and its active attempt
    /// have been persisted by an adapter.
    pub fn restored(job: JobRecord, claim: JobClaim) -> Self {
        Self { job, claim }
    }

    pub fn job(&self) -> &JobRecord {
        &self.job
    }

    pub fn claim(&self) -> &JobClaim {
        &self.claim
    }

    pub fn into_claim(self) -> JobClaim {
        self.claim
    }
}

/// Completion input for an active claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobCompletion {
    claim: JobClaim,
    completed_at: JobTimestamp,
}

impl JobCompletion {
    pub fn new(claim: JobClaim, completed_at: JobTimestamp) -> Result<Self, JobInputError> {
        if completed_at < claim.claimed_at {
            return Err(JobInputError::CompletionBeforeClaim);
        }
        Ok(Self {
            claim,
            completed_at,
        })
    }

    pub fn claim(&self) -> &JobClaim {
        &self.claim
    }

    pub const fn completed_at(&self) -> JobTimestamp {
        self.completed_at
    }
}

/// Failure input for an active claim. The error is stored as opaque data and
/// must not be treated as executable content or written to logs by adapters.
#[derive(Clone, Eq, PartialEq)]
pub struct JobFailure {
    claim: JobClaim,
    failed_at: JobTimestamp,
    error: String,
}

impl JobFailure {
    pub fn new(
        claim: JobClaim,
        failed_at: JobTimestamp,
        error: impl Into<String>,
    ) -> Result<Self, JobInputError> {
        if failed_at < claim.claimed_at {
            return Err(JobInputError::FailureBeforeClaim);
        }
        let error = error.into();
        validate_job_text("job failure description", &error)?;
        Ok(Self {
            claim,
            failed_at,
            error,
        })
    }

    pub fn claim(&self) -> &JobClaim {
        &self.claim
    }

    pub const fn failed_at(&self) -> JobTimestamp {
        self.failed_at
    }

    pub fn error(&self) -> &str {
        &self.error
    }
}

impl fmt::Debug for JobFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JobFailure")
            .field("claim", &self.claim)
            .field("failed_at", &self.failed_at)
            .field("error_bytes", &self.error.len())
            .finish()
    }
}

/// The result of inserting a job whose identity is globally deduplicated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JobEnqueueResult {
    Enqueued(JobRecord),
    Duplicate(JobRecord),
}

/// The durable result of a failed execution attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobFailureResult {
    ReadyForRetry,
    Failed,
}

/// The immutable audit outcome of one execution attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobAttemptOutcome {
    Active,
    Succeeded,
    RetryableFailure,
    TerminalFailure,
    Expired,
}

/// One persisted execution attempt, including the claim that owns it.
#[derive(Clone, Eq, PartialEq)]
pub struct JobAttempt {
    attempt_number: u32,
    claim_token: u32,
    worker_id: String,
    started_at: JobTimestamp,
    finished_at: Option<JobTimestamp>,
    outcome: JobAttemptOutcome,
    error: Option<String>,
}

impl JobAttempt {
    #[allow(clippy::too_many_arguments)]
    pub fn restored(
        attempt_number: u32,
        claim_token: u32,
        worker_id: String,
        started_at: JobTimestamp,
        finished_at: Option<JobTimestamp>,
        outcome: JobAttemptOutcome,
        error: Option<String>,
    ) -> Self {
        Self {
            attempt_number,
            claim_token,
            worker_id,
            started_at,
            finished_at,
            outcome,
            error,
        }
    }

    pub const fn attempt_number(&self) -> u32 {
        self.attempt_number
    }

    pub const fn claim_token(&self) -> u32 {
        self.claim_token
    }

    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    pub const fn started_at(&self) -> JobTimestamp {
        self.started_at
    }

    pub const fn finished_at(&self) -> Option<JobTimestamp> {
        self.finished_at
    }

    pub const fn outcome(&self) -> JobAttemptOutcome {
        self.outcome
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

impl fmt::Debug for JobAttempt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JobAttempt")
            .field("attempt_number", &self.attempt_number)
            .field("claim_token", &self.claim_token)
            .field("worker_id", &self.worker_id)
            .field("started_at", &self.started_at)
            .field("finished_at", &self.finished_at)
            .field("outcome", &self.outcome)
            .field("has_error", &self.error.is_some())
            .finish()
    }
}

/// Aggregate lifecycle counts for one acceptance-cycle job family.
///
/// This intentionally contains no job identity, work reference, worker, error,
/// timestamp, or source-derived data. The one-shot evaluator command uses it
/// only after requiring a fresh database, which makes the two mandatory job
/// families below belong to that one cycle.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AcceptanceJobProgress {
    total: usize,
    attempted: usize,
    ready: usize,
    claimed: usize,
    succeeded: usize,
    failed: usize,
}

impl AcceptanceJobProgress {
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        total: usize,
        attempted: usize,
        ready: usize,
        claimed: usize,
        succeeded: usize,
        failed: usize,
    ) -> Self {
        Self {
            total,
            attempted,
            ready,
            claimed,
            succeeded,
            failed,
        }
    }

    pub const fn total(self) -> usize {
        self.total
    }

    pub const fn attempted(self) -> usize {
        self.attempted
    }

    pub const fn ready(self) -> usize {
        self.ready
    }

    pub const fn claimed(self) -> usize {
        self.claimed
    }

    pub const fn succeeded(self) -> usize {
        self.succeeded
    }

    pub const fn failed(self) -> usize {
        self.failed
    }

    pub const fn is_terminal(self) -> bool {
        self.succeeded.saturating_add(self.failed) == self.total
    }
}

/// Fixed aggregate failure categories safe for an evaluator-facing report.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AcceptanceFailureCategories {
    source_review_continuation_link: usize,
    source_other_mandatory_stage: usize,
    summary: usize,
}

impl AcceptanceFailureCategories {
    pub const fn new(
        source_review_continuation_link: usize,
        source_other_mandatory_stage: usize,
        summary: usize,
    ) -> Self {
        Self {
            source_review_continuation_link,
            source_other_mandatory_stage,
            summary,
        }
    }

    pub const fn source_review_continuation_link(self) -> usize {
        self.source_review_continuation_link
    }

    pub const fn source_other_mandatory_stage(self) -> usize {
        self.source_other_mandatory_stage
    }

    pub const fn summary(self) -> usize {
        self.summary
    }
}

/// Aggregate-only durable observation for the fresh one-shot acceptance cycle.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AcceptanceCycleSnapshot {
    selected: usize,
    source_ingestion: AcceptanceJobProgress,
    summaries: AcceptanceJobProgress,
    persisted: usize,
    complete_video: usize,
    summaries_ready: usize,
    summaries_pending_or_missing: usize,
    failures: AcceptanceFailureCategories,
}

impl AcceptanceCycleSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        selected: usize,
        source_ingestion: AcceptanceJobProgress,
        summaries: AcceptanceJobProgress,
        persisted: usize,
        complete_video: usize,
        summaries_ready: usize,
        summaries_pending_or_missing: usize,
        failures: AcceptanceFailureCategories,
    ) -> Self {
        Self {
            selected,
            source_ingestion,
            summaries,
            persisted,
            complete_video,
            summaries_ready,
            summaries_pending_or_missing,
            failures,
        }
    }

    pub const fn selected(self) -> usize {
        self.selected
    }

    pub const fn source_ingestion(self) -> AcceptanceJobProgress {
        self.source_ingestion
    }

    pub const fn summaries(self) -> AcceptanceJobProgress {
        self.summaries
    }

    pub const fn persisted(self) -> usize {
        self.persisted
    }

    pub const fn complete_video(self) -> usize {
        self.complete_video
    }

    pub const fn summaries_ready(self) -> usize {
        self.summaries_ready
    }

    pub const fn summaries_pending_or_missing(self) -> usize {
        self.summaries_pending_or_missing
    }

    pub const fn failures(self) -> AcceptanceFailureCategories {
        self.failures
    }
}

/// Application-owned aggregate read port for the opt-in evaluator acceptance command.
///
/// The port cannot return titles, source identifiers, job identities, work references,
/// timestamps, paths, errors, or source payload fields.
pub trait AcceptanceCycleReadPort {
    type Error;

    fn acceptance_cycle_snapshot(&mut self) -> Result<AcceptanceCycleSnapshot, Self::Error>;
}

/// Application-owned durable queue boundary. Implementations must recover
/// expired claims before selecting ready work and must atomically update a job
/// and its attempt history for every lifecycle transition.
pub trait JobStore {
    type Error;

    fn enqueue(&mut self, request: JobRequest) -> Result<JobEnqueueResult, Self::Error>;

    fn claim_next(&mut self, request: JobClaimRequest) -> Result<Option<ClaimedJob>, Self::Error>;

    /// Claim only one of the explicitly application-owned job types. Adapters must filter in the
    /// same durable transaction that recovers leases and creates the claim attempt.
    fn claim_next_matching(
        &mut self,
        request: JobClaimRequest,
        accepted_types: &[RuntimeJobType],
    ) -> Result<Option<ClaimedJob>, Self::Error>;

    /// Return the next durable time at which a matching claim or lease recovery
    /// may make progress. This is advisory wake scheduling only; a later claim
    /// remains the authoritative SQLite transition.
    fn next_claim_eligible_at(
        &mut self,
        request: JobClaimRequest,
        accepted_types: &[RuntimeJobType],
    ) -> Result<Option<JobTimestamp>, Self::Error>;

    fn complete(&mut self, completion: JobCompletion) -> Result<(), Self::Error>;

    fn fail(&mut self, failure: JobFailure) -> Result<JobFailureResult, Self::Error>;

    fn job(&mut self, identity: &str) -> Result<Option<JobRecord>, Self::Error>;

    fn attempts(&mut self, identity: &str) -> Result<Vec<JobAttempt>, Self::Error>;
}

/// Validation failures at the application queue boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobInputError {
    BlankText(&'static str),
    TextTooLong(&'static str),
    ZeroMaxAttempts,
    NegativeTimestamp,
    NonPositiveLeaseDuration,
    LeaseExpiryOverflow,
    NonPositivePacingInterval,
    PacingEligibilityOverflow,
    RetryEligibilityOverflow,
    CompletionBeforeClaim,
    FailureBeforeClaim,
}

impl fmt::Display for JobInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlankText(field) => write!(formatter, "{field} must not be blank"),
            Self::TextTooLong(field) => {
                write!(
                    formatter,
                    "{field} must not exceed {JOB_TEXT_MAX_BYTES} bytes"
                )
            }
            Self::ZeroMaxAttempts => formatter.write_str("job maximum attempts must be positive"),
            Self::NegativeTimestamp => formatter.write_str("job timestamp must not be negative"),
            Self::NonPositiveLeaseDuration => {
                formatter.write_str("job lease duration must be positive")
            }
            Self::LeaseExpiryOverflow => formatter.write_str("job lease expiry overflows"),
            Self::NonPositivePacingInterval => {
                formatter.write_str("job pacing interval must be positive")
            }
            Self::PacingEligibilityOverflow => {
                formatter.write_str("job pacing eligibility overflows")
            }
            Self::RetryEligibilityOverflow => {
                formatter.write_str("job retry eligibility overflows")
            }
            Self::CompletionBeforeClaim => {
                formatter.write_str("job completion must not predate its claim")
            }
            Self::FailureBeforeClaim => {
                formatter.write_str("job failure must not predate its claim")
            }
        }
    }
}

impl std::error::Error for JobInputError {}

fn validate_job_text(field: &'static str, value: &str) -> Result<(), JobInputError> {
    if value.trim().is_empty() {
        return Err(JobInputError::BlankText(field));
    }
    if value.len() > JOB_TEXT_MAX_BYTES {
        return Err(JobInputError::TextTooLong(field));
    }
    Ok(())
}

/// The typed M006/M009 jobs accepted by the bounded runtime.
///
/// Adding another execution kind requires an explicitly adopted application use case rather than
/// accepting an arbitrary queue string.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RuntimeJobType {
    SourceHourlyDiscovery,
    SourceGameIngestion,
    LlmReviewSummary,
}

impl RuntimeJobType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceHourlyDiscovery => "source.hourly-discovery",
            Self::SourceGameIngestion => "source.game-ingestion",
            Self::LlmReviewSummary => "llm.review-summary",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "source.hourly-discovery" => Some(Self::SourceHourlyDiscovery),
            "source.game-ingestion" => Some(Self::SourceGameIngestion),
            "llm.review-summary" => Some(Self::LlmReviewSummary),
            _ => None,
        }
    }
}

/// A small application-owned allowlist used at durable claim time, not merely when routing an
/// already claimed job. This keeps source and summary workers from taking each other's work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeJobTypeFilter(Vec<RuntimeJobType>);

impl RuntimeJobTypeFilter {
    pub fn new(
        job_types: impl IntoIterator<Item = RuntimeJobType>,
    ) -> Result<Self, RuntimeJobTypeFilterError> {
        let mut types = job_types.into_iter().collect::<Vec<_>>();
        types.sort_unstable();
        types.dedup();
        if types.is_empty() {
            return Err(RuntimeJobTypeFilterError::Empty);
        }
        Ok(Self(types))
    }

    pub fn source_lane() -> Self {
        Self(vec![
            RuntimeJobType::SourceHourlyDiscovery,
            RuntimeJobType::SourceGameIngestion,
        ])
    }

    pub fn llm_lane() -> Self {
        Self(vec![RuntimeJobType::LlmReviewSummary])
    }

    pub fn all() -> Self {
        Self(vec![
            RuntimeJobType::SourceHourlyDiscovery,
            RuntimeJobType::SourceGameIngestion,
            RuntimeJobType::LlmReviewSummary,
        ])
    }

    pub fn job_types(&self) -> &[RuntimeJobType] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeJobTypeFilterError {
    Empty,
}

impl fmt::Display for RuntimeJobTypeFilterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("runtime claim filter must include at least one job type")
    }
}

impl std::error::Error for RuntimeJobTypeFilterError {}

/// The canonical opaque work-reference prefix for one Metacritic source-ingestion job.
pub const SOURCE_INGESTION_WORK_REFERENCE_PREFIX: &str = "metacritic-game:";
/// The opaque work-reference prefix for a source-ingestion item owned by a durable run.
pub const RUN_SOURCE_INGESTION_WORK_REFERENCE_PREFIX: &str = "metacritic-run:";
/// The opaque work-reference prefix for a durable run's next discovery page.
pub const RUN_PROGRESS_WORK_REFERENCE_PREFIX: &str = "metacritic-run-progress:";

/// A source-ingestion request tied to one durable mandatory run.
///
/// The run identifier is application control metadata. It intentionally carries no title, URL,
/// source payload, or error material; the mutable source slug remains only the routing component
/// needed by the existing source adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunSourceIngestionRequest {
    run_id: String,
    source: SourceIngestionRequest,
}

impl RunSourceIngestionRequest {
    pub fn new(
        run_id: impl Into<String>,
        source_product_id: u64,
        source_slug: impl Into<String>,
    ) -> Result<Self, RunSourceIngestionRequestError> {
        let run_id = run_id.into();
        if run_id.is_empty() || run_id.len() > 128 || run_id.contains(':') {
            return Err(RunSourceIngestionRequestError::InvalidRunId);
        }
        Ok(Self {
            run_id,
            source: SourceIngestionRequest::new(source_product_id, source_slug)
                .map_err(RunSourceIngestionRequestError::Source)?,
        })
    }

    pub fn from_work_reference(value: &str) -> Result<Self, RunSourceIngestionRequestError> {
        let encoded = value
            .strip_prefix(RUN_SOURCE_INGESTION_WORK_REFERENCE_PREFIX)
            .ok_or(RunSourceIngestionRequestError::MalformedWorkReference)?;
        let mut parts = encoded.split(':');
        let run_id = parts
            .next()
            .ok_or(RunSourceIngestionRequestError::MalformedWorkReference)?;
        let source_product_id = parts
            .next()
            .ok_or(RunSourceIngestionRequestError::MalformedWorkReference)?;
        let source_slug = parts
            .next()
            .ok_or(RunSourceIngestionRequestError::MalformedWorkReference)?;
        if parts.next().is_some()
            || source_product_id.is_empty()
            || !source_product_id.bytes().all(|byte| byte.is_ascii_digit())
            || (source_product_id.len() > 1 && source_product_id.starts_with('0'))
        {
            return Err(RunSourceIngestionRequestError::MalformedWorkReference);
        }
        let source_product_id = source_product_id
            .parse::<u64>()
            .map_err(|_| RunSourceIngestionRequestError::MalformedWorkReference)?;
        Self::new(run_id, source_product_id, source_slug)
            .map_err(|_| RunSourceIngestionRequestError::MalformedWorkReference)
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn source(&self) -> &SourceIngestionRequest {
        &self.source
    }

    pub fn work_reference(&self) -> String {
        format!(
            "{RUN_SOURCE_INGESTION_WORK_REFERENCE_PREFIX}{}:{}:{}",
            self.run_id,
            self.source.source_product_id().value(),
            self.source.source_slug()
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunSourceIngestionRequestError {
    InvalidRunId,
    Source(SourceIngestionRequestError),
    MalformedWorkReference,
}

impl fmt::Display for RunSourceIngestionRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRunId => {
                formatter.write_str("run identifier must be bounded and colon-free")
            }
            Self::Source(error) => error.fmt(formatter),
            Self::MalformedWorkReference => {
                formatter.write_str("malformed durable run work reference")
            }
        }
    }
}

impl std::error::Error for RunSourceIngestionRequestError {}

/// Application-owned policy for a source-ingestion job derived from a selected candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceIngestionJobSchedule {
    max_attempts: u32,
}

impl SourceIngestionJobSchedule {
    pub fn new(max_attempts: u32) -> Result<Self, JobInputError> {
        if max_attempts == 0 {
            return Err(JobInputError::ZeroMaxAttempts);
        }
        Ok(Self { max_attempts })
    }

    pub const fn max_attempts(self) -> u32 {
        self.max_attempts
    }

    /// Build one day-scoped, replay-deduplicated request per selected candidate.
    pub fn requests_for(
        self,
        day: &CrawlDayKey,
        selected: &[DiscoveryCandidate],
        created_at: JobTimestamp,
    ) -> Result<Vec<JobRequest>, SourceIngestionJobScheduleError> {
        selected
            .iter()
            .map(|candidate| {
                let request = SourceIngestionRequest::new(
                    candidate.source_product_id().value(),
                    candidate.source_slug(),
                )
                .map_err(SourceIngestionJobScheduleError::Request)?;
                JobRequest::new(
                    format!(
                        "{}:{}:{}",
                        RuntimeJobType::SourceGameIngestion.as_str(),
                        day.as_str(),
                        request.source_product_id().value()
                    ),
                    RuntimeJobType::SourceGameIngestion.as_str(),
                    request.work_reference(),
                    self.max_attempts,
                    created_at,
                )
                .map_err(SourceIngestionJobScheduleError::JobRequest)
            })
            .collect()
    }

    /// Build the one durable source-ingestion request for a candidate owned by a run.
    pub fn request_for_run(
        self,
        run_id: &str,
        candidate: &DiscoveryCandidate,
        created_at: JobTimestamp,
    ) -> Result<JobRequest, SourceIngestionJobScheduleError> {
        let request = RunSourceIngestionRequest::new(
            run_id,
            candidate.source_product_id().value(),
            candidate.source_slug(),
        )
        .map_err(|error| match error {
            RunSourceIngestionRequestError::Source(error) => {
                SourceIngestionJobScheduleError::Request(error)
            }
            RunSourceIngestionRequestError::InvalidRunId
            | RunSourceIngestionRequestError::MalformedWorkReference => {
                SourceIngestionJobScheduleError::RunRequest(error)
            }
        })?;
        JobRequest::new(
            format!(
                "{}:{}:{}",
                RuntimeJobType::SourceGameIngestion.as_str(),
                run_id,
                request.source().source_product_id().value()
            ),
            RuntimeJobType::SourceGameIngestion.as_str(),
            request.work_reference(),
            self.max_attempts,
            created_at,
        )
        .map_err(SourceIngestionJobScheduleError::JobRequest)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceIngestionJobScheduleError {
    Request(SourceIngestionRequestError),
    RunRequest(RunSourceIngestionRequestError),
    JobRequest(JobInputError),
}

impl fmt::Display for SourceIngestionJobScheduleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(error) => error.fmt(formatter),
            Self::RunRequest(error) => error.fmt(formatter),
            Self::JobRequest(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SourceIngestionJobScheduleError {}

/// The hour width used to derive a durable schedule identity from a caller-supplied timestamp.
pub const HOURLY_SCHEDULE_SECONDS: i64 = 60 * 60;

/// Application-owned policy for one durable hourly job family.
///
/// The durable job identity, not process-local memory, suppresses duplicate
/// scheduler ticks for the same hour slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HourlyJobSchedule {
    job_type: RuntimeJobType,
    max_attempts: u32,
}

impl HourlyJobSchedule {
    pub fn new(job_type: RuntimeJobType, max_attempts: u32) -> Result<Self, JobInputError> {
        if max_attempts == 0 {
            return Err(JobInputError::ZeroMaxAttempts);
        }
        Ok(Self {
            job_type,
            max_attempts,
        })
    }

    pub const fn job_type(self) -> RuntimeJobType {
        self.job_type
    }

    pub const fn max_attempts(self) -> u32 {
        self.max_attempts
    }

    /// Construct the durable request for the supplied hour without reading a wall clock.
    pub fn request_for(&self, scheduled_at: JobTimestamp) -> Result<JobRequest, JobInputError> {
        let hour_slot = scheduled_at.value() / HOURLY_SCHEDULE_SECONDS;
        JobRequest::new(
            format!("hourly:{}:{hour_slot}", self.job_type.as_str()),
            self.job_type.as_str(),
            format!("hour-slot:{hour_slot}"),
            self.max_attempts,
            scheduled_at,
        )
    }
}

/// A validated queued job whose type can be routed without interpreting its opaque work reference.
#[derive(Clone, Eq, PartialEq)]
pub struct TypedJob {
    identity: String,
    job_type: RuntimeJobType,
    work_ref: String,
    created_at: JobTimestamp,
    claimed_at: Option<JobTimestamp>,
    claim_fence: Option<JobClaimFence>,
    max_attempts: u32,
    attempt_count: u32,
}

/// The bounded portion of a queue claim that a handler may present to another durable adapter.
///
/// It intentionally omits the worker identity and carries only the monotonic token plus expiry
/// needed to reject a reclaimed worker before it changes application state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JobClaimFence {
    claim_token: u32,
    lease_expires_at: JobTimestamp,
}

impl JobClaimFence {
    pub const fn from_claim(claim: &JobClaim) -> Self {
        Self {
            claim_token: claim.claim_token(),
            lease_expires_at: claim.lease_expires_at(),
        }
    }

    pub const fn claim_token(self) -> u32 {
        self.claim_token
    }

    pub const fn lease_expires_at(self) -> JobTimestamp {
        self.lease_expires_at
    }
}

impl TypedJob {
    pub fn from_record(record: &JobRecord) -> Option<Self> {
        Some(Self {
            identity: record.identity().to_owned(),
            job_type: RuntimeJobType::parse(record.job_type())?,
            work_ref: record.work_ref().to_owned(),
            created_at: record.created_at(),
            claimed_at: None,
            claim_fence: None,
            max_attempts: record.max_attempts(),
            attempt_count: record.attempt_count(),
        })
    }

    /// Construct a handler-visible job from the current fenced durable claim.
    pub fn from_claimed(claimed: &ClaimedJob) -> Option<Self> {
        let mut typed = Self::from_record(claimed.job())?;
        typed.claimed_at = Some(claimed.claim().claimed_at());
        typed.claim_fence = Some(JobClaimFence::from_claim(claimed.claim()));
        Some(typed)
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    pub const fn job_type(&self) -> RuntimeJobType {
        self.job_type
    }

    pub fn work_ref(&self) -> &str {
        &self.work_ref
    }

    pub const fn is_final_attempt(&self) -> bool {
        self.attempt_count >= self.max_attempts
    }

    pub const fn created_at(&self) -> JobTimestamp {
        self.created_at
    }

    /// The durable claim timestamp when the dispatcher supplied one; compatibility test fixtures
    /// built directly from a record fall back to the record creation time.
    pub const fn claimed_at(&self) -> JobTimestamp {
        match self.claimed_at {
            Some(value) => value,
            None => self.created_at,
        }
    }

    /// The queue fence for a job dispatched from an active durable claim.
    /// Compatibility fixtures built from records have no fence and cannot settle durable runs.
    pub const fn claim_fence(&self) -> Option<JobClaimFence> {
        self.claim_fence
    }
}

impl fmt::Debug for TypedJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TypedJob")
            .field("identity", &self.identity)
            .field("job_type", &self.job_type)
            .field("work_ref_bytes", &self.work_ref.len())
            .finish()
    }
}

/// Fixed, process-local categories for evaluator-safe failure observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerFailureCategory {
    MissingRequiredVideo,
    SourceTransportOrContract,
    PersistenceOrQueue,
    OtherMandatory,
}

impl WorkerFailureCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingRequiredVideo => "missing_required_video",
            Self::SourceTransportOrContract => "source_transport_or_contract",
            Self::PersistenceOrQueue => "persistence_or_queue",
            Self::OtherMandatory => "other_mandatory",
        }
    }
}

/// Process-local aggregate counts. It contains no job, source, error, or path data.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FailureCategoryCounts {
    missing_required_video: usize,
    source_transport_or_contract: usize,
    persistence_or_queue: usize,
    other_mandatory: usize,
}

impl FailureCategoryCounts {
    pub const fn zero() -> Self {
        Self {
            missing_required_video: 0,
            source_transport_or_contract: 0,
            persistence_or_queue: 0,
            other_mandatory: 0,
        }
    }

    pub fn increment(&mut self, category: WorkerFailureCategory) {
        match category {
            WorkerFailureCategory::MissingRequiredVideo => {
                self.missing_required_video = self.missing_required_video.saturating_add(1)
            }
            WorkerFailureCategory::SourceTransportOrContract => {
                self.source_transport_or_contract =
                    self.source_transport_or_contract.saturating_add(1)
            }
            WorkerFailureCategory::PersistenceOrQueue => {
                self.persistence_or_queue = self.persistence_or_queue.saturating_add(1)
            }
            WorkerFailureCategory::OtherMandatory => {
                self.other_mandatory = self.other_mandatory.saturating_add(1)
            }
        }
    }

    pub fn merge(&mut self, other: Self) {
        self.missing_required_video = self
            .missing_required_video
            .saturating_add(other.missing_required_video);
        self.source_transport_or_contract = self
            .source_transport_or_contract
            .saturating_add(other.source_transport_or_contract);
        self.persistence_or_queue = self
            .persistence_or_queue
            .saturating_add(other.persistence_or_queue);
        self.other_mandatory = self.other_mandatory.saturating_add(other.other_mandatory);
    }

    pub fn reset(&mut self) {
        *self = Self::zero();
    }

    pub const fn missing_required_video(self) -> usize {
        self.missing_required_video
    }

    pub const fn source_transport_or_contract(self) -> usize {
        self.source_transport_or_contract
    }

    pub const fn persistence_or_queue(self) -> usize {
        self.persistence_or_queue
    }

    pub const fn other_mandatory(self) -> usize {
        self.other_mandatory
    }
}

/// Opaque handler failure data. The runtime persists only `message`; observation is ephemeral.
#[derive(Clone, Eq, PartialEq)]
pub struct JobHandlerFailure {
    message: String,
    observation: WorkerFailureCategory,
}

impl JobHandlerFailure {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            observation: WorkerFailureCategory::OtherMandatory,
        }
    }

    pub fn with_observation(
        message: impl Into<String>,
        observation: WorkerFailureCategory,
    ) -> Self {
        Self {
            message: message.into(),
            observation,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub const fn observation(&self) -> WorkerFailureCategory {
        self.observation
    }
}

impl fmt::Debug for JobHandlerFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JobHandlerFailure")
            .field("message_bytes", &self.message.len())
            .finish()
    }
}

/// The only terminal signals a typed M006 handler may return to the dispatcher.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JobHandlerResult {
    Succeeded,
    /// Successful durable settlement with a fixed aggregate-only observation.
    SucceededWithObservation(WorkerFailureCategory),
    Failed(JobHandlerFailure),
}

/// Object-safe future type used by typed worker adapters without adding an async-trait dependency.
pub type JobHandlerFuture = Pin<Box<dyn Future<Output = JobHandlerResult> + Send + 'static>>;

/// Application port implemented by outer worker lanes.
///
/// A handler receives a validated job type plus opaque work reference, but never
/// receives the durable claim capability. Only the dispatcher may settle it.
pub trait JobHandler: Send + Sync {
    fn job_type(&self) -> RuntimeJobType;

    fn handle(&self, job: TypedJob) -> JobHandlerFuture;
}

/// Immutable typed handler lookup. Duplicate registrations are rejected while wiring the process.
#[derive(Clone, Default)]
pub struct JobHandlerRegistry {
    handlers: BTreeMap<RuntimeJobType, Arc<dyn JobHandler>>,
}

impl JobHandlerRegistry {
    pub fn new(
        handlers: impl IntoIterator<Item = Arc<dyn JobHandler>>,
    ) -> Result<Self, JobHandlerRegistryError> {
        let mut registered = BTreeMap::new();
        for handler in handlers {
            let job_type = handler.job_type();
            if registered.insert(job_type, handler).is_some() {
                return Err(JobHandlerRegistryError::DuplicateJobType(job_type));
            }
        }
        Ok(Self {
            handlers: registered,
        })
    }

    pub fn handler(&self, job_type: RuntimeJobType) -> Option<Arc<dyn JobHandler>> {
        self.handlers.get(&job_type).cloned()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobHandlerRegistryError {
    DuplicateJobType(RuntimeJobType),
}

impl fmt::Display for JobHandlerRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateJobType(job_type) => {
                write!(
                    formatter,
                    "duplicate typed job handler for {}",
                    job_type.as_str()
                )
            }
        }
    }
}

impl std::error::Error for JobHandlerRegistryError {}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::{Future, Ready, ready};
    use std::sync::Mutex;
    use std::task::{Context, Poll, Waker};

    use super::*;

    #[derive(Default)]
    struct FixtureStore {
        candidates: Vec<CoverBackfillCandidate>,
        outcomes: VecDeque<CoverBackfillPersistOutcome>,
    }

    impl CoverBackfillStorePort for FixtureStore {
        type Error = ();

        fn cover_backfill_candidates(
            &mut self,
            limit: usize,
        ) -> Result<Vec<CoverBackfillCandidate>, Self::Error> {
            assert!(self.candidates.len() <= limit);
            Ok(std::mem::take(&mut self.candidates))
        }

        fn store_cover_if_current(
            &mut self,
            _candidate: &CoverBackfillCandidate,
            _cover: &StoredCoverImage,
        ) -> Result<CoverBackfillPersistOutcome, Self::Error> {
            Ok(self
                .outcomes
                .pop_front()
                .expect("fixture persistence outcome must exist"))
        }
    }

    struct FixtureSource {
        responses: Mutex<VecDeque<Result<CoverBackfillFetchOutcome, ()>>>,
    }

    impl AsyncCoverImageSourcePort for FixtureSource {
        type Error = ();
        type FetchFuture<'a>
            = Ready<Result<CoverBackfillFetchOutcome, Self::Error>>
        where
            Self: 'a;

        fn fetch_cover(&self, _candidate: &CoverBackfillCandidate) -> Self::FetchFuture<'_> {
            ready(
                self.responses
                    .lock()
                    .expect("fixture source lock must hold")
                    .pop_front()
                    .expect("fixture source response must exist"),
            )
        }
    }

    fn candidate(source_product_id: u64, filename: &str) -> CoverBackfillCandidate {
        CoverBackfillCandidate::new(
            SourceProductId::new(source_product_id).expect("test identity must be valid"),
            GameCoverDescriptor::new(
                format!("/provider/7/2/{filename}"),
                "catalog",
                filename,
                "cardImage",
            )
            .expect("test descriptor must be valid"),
        )
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = std::pin::pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    #[test]
    fn cover_backfill_report_counts_each_unavailable_reason_without_identity_leakage() {
        let image = StoredCoverImage::new(CoverImageContentType::Png, vec![1])
            .expect("test image must be valid");
        let mut store = FixtureStore {
            candidates: (0..14)
                .map(|offset| candidate(4_242 + offset, "private-cover-name.png"))
                .collect(),
            outcomes: VecDeque::from([
                CoverBackfillPersistOutcome::Stored,
                CoverBackfillPersistOutcome::Stale,
                CoverBackfillPersistOutcome::AlreadyCurrent,
            ]),
        };
        let source = FixtureSource {
            responses: Mutex::new(VecDeque::from([
                Ok(CoverBackfillFetchOutcome::Stored(image.clone())),
                Ok(CoverBackfillFetchOutcome::Unavailable(
                    CoverBackfillUnavailableReason::DescriptorRejected,
                )),
                Ok(CoverBackfillFetchOutcome::Unavailable(
                    CoverBackfillUnavailableReason::UnexpectedHttpStatus(
                        CoverBackfillHttpStatusClass::Informational,
                    ),
                )),
                Ok(CoverBackfillFetchOutcome::Unavailable(
                    CoverBackfillUnavailableReason::UnexpectedHttpStatus(
                        CoverBackfillHttpStatusClass::SuccessfulOther,
                    ),
                )),
                Ok(CoverBackfillFetchOutcome::Unavailable(
                    CoverBackfillUnavailableReason::UnexpectedHttpStatus(
                        CoverBackfillHttpStatusClass::Redirection,
                    ),
                )),
                Ok(CoverBackfillFetchOutcome::Unavailable(
                    CoverBackfillUnavailableReason::UnexpectedHttpStatus(
                        CoverBackfillHttpStatusClass::ClientError,
                    ),
                )),
                Ok(CoverBackfillFetchOutcome::Unavailable(
                    CoverBackfillUnavailableReason::UnexpectedHttpStatus(
                        CoverBackfillHttpStatusClass::ServerError,
                    ),
                )),
                Ok(CoverBackfillFetchOutcome::Unavailable(
                    CoverBackfillUnavailableReason::UnexpectedHttpStatus(
                        CoverBackfillHttpStatusClass::Other,
                    ),
                )),
                Ok(CoverBackfillFetchOutcome::Unavailable(
                    CoverBackfillUnavailableReason::UnsupportedContentType,
                )),
                Ok(CoverBackfillFetchOutcome::Unavailable(
                    CoverBackfillUnavailableReason::SignatureMismatch,
                )),
                Ok(CoverBackfillFetchOutcome::Unavailable(
                    CoverBackfillUnavailableReason::InvalidBody,
                )),
                Ok(CoverBackfillFetchOutcome::Stored(image.clone())),
                Ok(CoverBackfillFetchOutcome::Stored(image)),
                Err(()),
            ])),
        };

        let report = block_on(execute_cover_backfill(&mut store, &source, 20))
            .expect("application coordinator must complete");

        assert_eq!(report.attempted(), 14);
        assert_eq!(report.stored(), 1);
        assert_eq!(report.unavailable(), 10);
        assert_eq!(report.unavailable(), report.unavailable_reasons().total());
        assert_eq!(report.stale(), 1);
        assert_eq!(report.already_current(), 1);
        assert_eq!(report.failed(), 1);
        assert!(report.made_progress());
        assert_eq!(report.exit_code(), 1);
        assert_eq!(
            report.to_json(),
            concat!(
                "{\"schema_version\":\"gamepulse.cover_backfill.v3\",",
                "\"attempted\":14,\"stored\":1,\"unavailable\":10,",
                "\"unavailable_reasons\":{",
                "\"descriptor_rejected\":1,",
                "\"unexpected_http_status\":{",
                "\"informational\":1,\"successful_other\":1,\"redirection\":1,",
                "\"client_error\":1,\"server_error\":1,\"other\":1},",
                "\"unsupported_content_type\":1,\"signature_mismatch\":1,\"invalid_body\":1},",
                "\"stale\":1,\"already_current\":1,\"failed\":1,\"made_progress\":true}"
            )
        );
        for prohibited in [
            "4242",
            "private-cover-name.png",
            "/provider/",
            "https://",
            "metacritic.com",
            "source_product_id",
            "cookie",
            "header",
        ] {
            assert!(!report.to_json().contains(prohibited));
        }
    }

    #[test]
    fn cover_backfill_reports_no_candidates_without_progress() {
        let mut store = FixtureStore::default();
        let source = FixtureSource {
            responses: Mutex::new(VecDeque::new()),
        };
        let repeated = block_on(execute_cover_backfill(&mut store, &source, 20))
            .expect("empty repeat must complete");
        assert_eq!(repeated.attempted(), 0);
        assert_eq!(repeated.unavailable(), 0);
        assert_eq!(
            repeated.unavailable(),
            repeated.unavailable_reasons().total()
        );
        assert_eq!(repeated.failed(), 0);
        assert_eq!(repeated.exit_code(), 0);
        assert!(!repeated.made_progress());
    }

    #[test]
    fn descriptor_fingerprint_is_stable_and_component_unambiguous() {
        let first = candidate(101, "a.png");
        let second = candidate(101, "a.png");
        let different = candidate(101, "b.png");
        assert_eq!(
            first.descriptor_fingerprint(),
            second.descriptor_fingerprint()
        );
        assert_ne!(
            first.descriptor_fingerprint(),
            different.descriptor_fingerprint()
        );
    }

    #[test]
    fn application_rejects_a_cover_backfill_limit_above_the_source_budget() {
        let mut store = FixtureStore::default();
        let source = FixtureSource {
            responses: Mutex::new(VecDeque::new()),
        };
        assert!(matches!(
            block_on(execute_cover_backfill(
                &mut store,
                &source,
                MAX_COVER_BACKFILL_CANDIDATES + 1
            )),
            Err(CoverBackfillExecutionError::InvalidLimit)
        ));
    }
}
