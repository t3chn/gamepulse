#![forbid(unsafe_code)]

//! Application use cases and ports for GamePulse.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub use gamepulse_domain::{
    APP_NAME, BrowseCursor, BrowseProgress, CrawlDayKey, CrawlDayKeyError, CrawlDiscoveryRequest,
    DailyCrawlAction, DailyCrawlState, DailyCrawlTransition, GameCoverDescriptor, GameDeveloper,
    GamePlatformScore, GameSnapshot, GameSnapshotValidationError, GameVideoLink, Metascore,
    MetascoreError, SourceProductId, SourceProductIdError, Userscore, UserscoreError,
};
use gamepulse_domain::{prepare_daily_crawl, select_daily_crawl};

/// Application-owned durable boundary for replacing one complete game snapshot.
///
/// Implementations must key the game by `source_product_id`, update its mutable source slug, and
/// make the game row plus all platform-score and developer rows visible together or not at all.
pub trait GameSnapshotStore {
    type Error;

    fn upsert_snapshot(&mut self, snapshot: &GameSnapshot) -> Result<(), Self::Error>;
}

/// The compact fields rendered on a catalogue card.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogueGameCard {
    source_product_id: SourceProductId,
    title: String,
    highest_metascore: Option<u8>,
    platforms: Vec<String>,
    developers: Vec<String>,
}

impl CatalogueGameCard {
    pub fn new(
        source_product_id: SourceProductId,
        title: impl Into<String>,
        highest_metascore: Option<u8>,
        platforms: Vec<String>,
        developers: Vec<String>,
    ) -> Self {
        Self {
            source_product_id,
            title: title.into(),
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
    video_url: Option<String>,
    platform_scores: Vec<CataloguePlatformScore>,
    developers: Vec<String>,
    similar_games: Vec<SimilarCatalogueGame>,
}

impl CatalogueGameDetail {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_product_id: SourceProductId,
        source_slug: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
        cover: Option<CatalogueCoverDescriptor>,
        video_url: Option<String>,
        platform_scores: Vec<CataloguePlatformScore>,
        developers: Vec<String>,
        similar_games: Vec<SimilarCatalogueGame>,
    ) -> Self {
        Self {
            source_product_id,
            source_slug: source_slug.into(),
            title: title.into(),
            description: description.into(),
            cover,
            video_url,
            platform_scores,
            developers,
            similar_games,
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

/// Persist one previously validated inner snapshot through the application-owned boundary.
pub fn upsert_game_snapshot<S>(store: &mut S, snapshot: &GameSnapshot) -> Result<(), S::Error>
where
    S: GameSnapshotStore,
{
    store.upsert_snapshot(snapshot)
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

    let discovery = match prepare_daily_crawl(day, expected_previous_state.clone()) {
        DailyCrawlAction::Discover(discovery) => discovery,
        DailyCrawlAction::Exhausted(state) => return Ok(DailyCrawlOutcome::Exhausted(state)),
    };
    let request = discovery.request();
    let page = source_port
        .discover(request)
        .map_err(DailyCrawlError::Source)?;
    let transition = select_daily_crawl(
        discovery,
        page.candidates()
            .iter()
            .map(DiscoveryCandidate::source_product_id),
        page.next_browse_cursor(),
    );
    let selected = selected_candidates(&page, &transition);
    let state = transition.next_state().clone();
    let commit = DailyCrawlCommit::new(expected_previous_state, state.clone(), selected.clone())
        .map_err(DailyCrawlError::InvalidCommit)?;

    state_port.commit(commit).map_err(DailyCrawlError::Commit)?;

    Ok(DailyCrawlOutcome::Selected(DailyCrawlSelection {
        request,
        selected,
        state,
    }))
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
    let discovery = match prepare_daily_crawl(day, expected_previous_state.clone()) {
        DailyCrawlAction::Discover(discovery) => discovery,
        DailyCrawlAction::Exhausted(state) => return Ok(DailyCrawlOutcome::Exhausted(state)),
    };
    let request = discovery.request();
    let page = source_port
        .discover(request)
        .await
        .map_err(DailyCrawlError::Source)?;
    let transition = select_daily_crawl(
        discovery,
        page.candidates()
            .iter()
            .map(DiscoveryCandidate::source_product_id),
        page.next_browse_cursor(),
    );
    let selected = selected_candidates(&page, &transition);
    let state = transition.next_state().clone();
    let commit = DailyCrawlCommit::new(expected_previous_state, state.clone(), selected.clone())
        .map_err(DailyCrawlError::InvalidCommit)?;

    commit_state(commit).map_err(DailyCrawlError::Commit)?;

    Ok(DailyCrawlOutcome::Selected(DailyCrawlSelection {
        request,
        selected,
        state,
    }))
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
    let discovery = match prepare_daily_crawl(day, expected_previous_state.clone()) {
        DailyCrawlAction::Discover(discovery) => discovery,
        DailyCrawlAction::Exhausted(state) => return Ok(DailyCrawlOutcome::Exhausted(state)),
    };
    let request = discovery.request();
    let page = source_port
        .discover(request)
        .await
        .map_err(DailyCrawlError::Source)?;
    let transition = select_daily_crawl(
        discovery,
        page.candidates()
            .iter()
            .map(DiscoveryCandidate::source_product_id),
        page.next_browse_cursor(),
    );
    let selected = selected_candidates(&page, &transition);
    let state = transition.next_state().clone();
    let commit = DailyCrawlCommit::new(expected_previous_state, state.clone(), selected.clone())
        .map_err(DailyCrawlError::InvalidCommit)?
        .with_source_ingestion_jobs(schedule, created_at)
        .map_err(DailyCrawlError::JobSchedule)?;

    commit_state(commit).map_err(DailyCrawlError::Commit)?;

    Ok(DailyCrawlOutcome::Selected(DailyCrawlSelection {
        request,
        selected,
        state,
    }))
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
        })
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

/// Application-owned durable queue boundary. Implementations must recover
/// expired claims before selecting ready work and must atomically update a job
/// and its attempt history for every lifecycle transition.
pub trait JobStore {
    type Error;

    fn enqueue(&mut self, request: JobRequest) -> Result<JobEnqueueResult, Self::Error>;

    fn claim_next(&mut self, request: JobClaimRequest) -> Result<Option<ClaimedJob>, Self::Error>;

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
}

impl RuntimeJobType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceHourlyDiscovery => "source.hourly-discovery",
            Self::SourceGameIngestion => "source.game-ingestion",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "source.hourly-discovery" => Some(Self::SourceHourlyDiscovery),
            "source.game-ingestion" => Some(Self::SourceGameIngestion),
            _ => None,
        }
    }
}

/// The canonical opaque work-reference prefix for one Metacritic source-ingestion job.
pub const SOURCE_INGESTION_WORK_REFERENCE_PREFIX: &str = "metacritic-game:";

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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceIngestionJobScheduleError {
    Request(SourceIngestionRequestError),
    JobRequest(JobInputError),
}

impl fmt::Display for SourceIngestionJobScheduleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(error) => error.fmt(formatter),
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
}

impl TypedJob {
    pub fn from_record(record: &JobRecord) -> Option<Self> {
        Some(Self {
            identity: record.identity().to_owned(),
            job_type: RuntimeJobType::parse(record.job_type())?,
            work_ref: record.work_ref().to_owned(),
            created_at: record.created_at(),
        })
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

    pub const fn created_at(&self) -> JobTimestamp {
        self.created_at
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

/// Opaque handler failure data. The runtime persists it through `JobStore` but never logs it.
#[derive(Clone, Eq, PartialEq)]
pub struct JobHandlerFailure(String);

impl JobHandlerFailure {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for JobHandlerFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JobHandlerFailure")
            .field("message_bytes", &self.0.len())
            .finish()
    }
}

/// The only terminal signals a typed M006 handler may return to the dispatcher.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JobHandlerResult {
    Succeeded,
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
