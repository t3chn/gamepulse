#![forbid(unsafe_code)]

//! Pure domain types and policy for GamePulse.

use std::collections::BTreeSet;
use std::fmt;

/// Human-readable application name.
pub const APP_NAME: &str = "GamePulse";

/// The maximum number of games a single daily-crawl selection may return.
pub const DAILY_CRAWL_SELECTION_LIMIT: usize = 20;

/// An owner-supplied key that partitions daily crawl policy without assuming a clock format.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CrawlDayKey(String);

impl CrawlDayKey {
    pub fn new(value: impl Into<String>) -> Result<Self, CrawlDayKeyError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(CrawlDayKeyError::Blank);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A stable numeric product identity verified by the source adapter.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SourceProductId(u64);

impl SourceProductId {
    pub fn new(value: u64) -> Result<Self, SourceProductIdError> {
        if value == 0 {
            return Err(SourceProductIdError::Zero);
        }
        Ok(Self(value))
    }

    pub fn value(self) -> u64 {
        self.0
    }
}

/// A bounded critic score on the common 0-100 scale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Metascore(u8);

impl Metascore {
    pub fn new(value: u8) -> Result<Self, MetascoreError> {
        if value > 100 {
            return Err(MetascoreError::OutOfRange);
        }
        Ok(Self(value))
    }

    pub const fn value(self) -> u8 {
        self.0
    }
}

/// A bounded user score on the common 0-10 scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Userscore(f64);

impl Userscore {
    pub fn new(value: f64) -> Result<Self, UserscoreError> {
        if !value.is_finite() || !(0.0..=10.0).contains(&value) {
            return Err(UserscoreError::OutOfRange);
        }
        Ok(Self(value))
    }

    pub const fn value(self) -> f64 {
        self.0
    }
}

/// The original source descriptor for a cover image. Rendering policy is intentionally external.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameCoverDescriptor {
    bucket_path: String,
    bucket_type: String,
    filename: String,
    kind: String,
}

impl GameCoverDescriptor {
    pub fn new(
        bucket_path: impl Into<String>,
        bucket_type: impl Into<String>,
        filename: impl Into<String>,
        kind: impl Into<String>,
    ) -> Result<Self, GameSnapshotValidationError> {
        let bucket_path = bucket_path.into();
        let bucket_type = bucket_type.into();
        let filename = filename.into();
        let kind = kind.into();
        validate_snapshot_text("cover bucket path", &bucket_path)?;
        validate_snapshot_text("cover bucket type", &bucket_type)?;
        validate_snapshot_text("cover filename", &filename)?;
        validate_snapshot_text("cover kind", &kind)?;

        Ok(Self {
            bucket_path,
            bucket_type,
            filename,
            kind,
        })
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

/// A video link preserved as a source-supplied value without a provider-specific policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameVideoLink(String);

impl GameVideoLink {
    pub fn new(value: impl Into<String>) -> Result<Self, GameSnapshotValidationError> {
        let value = value.into();
        validate_snapshot_text("video link", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated public cover URL that the source adapter may persist with a snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GamePublicCoverUrl(String);

impl GamePublicCoverUrl {
    pub fn new(value: impl Into<String>) -> Result<Self, GameSnapshotValidationError> {
        let value = value.into();
        validate_snapshot_text("public cover URL", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One available platform and its independently optional scores.
#[derive(Clone, Debug, PartialEq)]
pub struct GamePlatformScore {
    source_platform_id: u64,
    source_slug: String,
    metascore: Option<Metascore>,
    userscore: Option<Userscore>,
}

impl GamePlatformScore {
    pub fn new(
        source_platform_id: u64,
        source_slug: impl Into<String>,
        metascore: Option<Metascore>,
        userscore: Option<Userscore>,
    ) -> Result<Self, GameSnapshotValidationError> {
        if source_platform_id == 0 {
            return Err(GameSnapshotValidationError::ZeroPlatformSourceId);
        }
        let source_slug = source_slug.into();
        validate_snapshot_text("platform source slug", &source_slug)?;

        Ok(Self {
            source_platform_id,
            source_slug,
            metascore,
            userscore,
        })
    }

    pub const fn source_platform_id(&self) -> u64 {
        self.source_platform_id
    }

    pub fn source_slug(&self) -> &str {
        &self.source_slug
    }

    pub const fn metascore(&self) -> Option<Metascore> {
        self.metascore
    }

    pub const fn userscore(&self) -> Option<Userscore> {
        self.userscore
    }
}

/// One source-supplied developer name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameDeveloper(String);

impl GameDeveloper {
    pub fn new(value: impl Into<String>) -> Result<Self, GameSnapshotValidationError> {
        let value = value.into();
        validate_snapshot_text("developer name", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A source-agnostic, validated game representation ready for one durable replacement write.
#[derive(Clone, Debug, PartialEq)]
pub struct GameSnapshot {
    source_product_id: SourceProductId,
    source_slug: String,
    title: String,
    description: String,
    cover: Option<GameCoverDescriptor>,
    public_cover_url: Option<GamePublicCoverUrl>,
    video: Option<GameVideoLink>,
    platform_scores: Vec<GamePlatformScore>,
    developers: Vec<GameDeveloper>,
}

impl GameSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_product_id: SourceProductId,
        source_slug: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
        cover: Option<GameCoverDescriptor>,
        video: Option<GameVideoLink>,
        platform_scores: Vec<GamePlatformScore>,
        developers: Vec<GameDeveloper>,
    ) -> Result<Self, GameSnapshotValidationError> {
        let source_slug = source_slug.into();
        let title = title.into();
        let description = description.into();
        validate_snapshot_text("source slug", &source_slug)?;
        validate_snapshot_text("title", &title)?;
        validate_snapshot_text("description", &description)?;

        let mut platform_ids = BTreeSet::new();
        for platform in &platform_scores {
            if !platform_ids.insert(platform.source_platform_id()) {
                return Err(GameSnapshotValidationError::DuplicatePlatformSourceId);
            }
        }
        let mut developer_names = BTreeSet::new();
        for developer in &developers {
            if !developer_names.insert(developer.as_str()) {
                return Err(GameSnapshotValidationError::DuplicateDeveloperName);
            }
        }

        Ok(Self {
            source_product_id,
            source_slug,
            title,
            description,
            cover,
            public_cover_url: None,
            video,
            platform_scores,
            developers,
        })
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

    pub fn cover(&self) -> Option<&GameCoverDescriptor> {
        self.cover.as_ref()
    }

    /// Attach one source-validated public cover URL without changing the original descriptor.
    pub fn with_public_cover_url(mut self, public_cover_url: Option<GamePublicCoverUrl>) -> Self {
        self.public_cover_url = public_cover_url;
        self
    }

    pub fn public_cover_url(&self) -> Option<&GamePublicCoverUrl> {
        self.public_cover_url.as_ref()
    }

    pub fn video(&self) -> Option<&GameVideoLink> {
        self.video.as_ref()
    }

    pub fn platform_scores(&self) -> &[GamePlatformScore] {
        &self.platform_scores
    }

    pub fn developers(&self) -> &[GameDeveloper] {
        &self.developers
    }
}

fn validate_snapshot_text(
    field: &'static str,
    value: &str,
) -> Result<(), GameSnapshotValidationError> {
    if value.trim().is_empty() {
        return Err(GameSnapshotValidationError::BlankField(field));
    }
    Ok(())
}

/// The two independently sourced review populations required by the assignment.
///
/// This value crosses every inner port so a critic input can never be silently
/// interpreted as a user input, or vice versa.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ReviewKind {
    Critic,
    User,
}

impl ReviewKind {
    pub const ALL: [Self; 2] = [Self::Critic, Self::User];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Critic => "critic",
            Self::User => "user",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "critic" => Some(Self::Critic),
            "user" => Some(Self::User),
            _ => None,
        }
    }
}

/// The M011 request boundary: one source page per kind, never unbounded pagination.
pub const REVIEW_INPUT_LIMIT: usize = 20;

/// The maximum UTF-8 byte length retained from one untrusted source excerpt.
pub const REVIEW_EXCERPT_MAX_BYTES: usize = 1_024;

/// One non-empty, bounded review excerpt that may be handed to a local summarizer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewExcerpt {
    text: String,
    polarity: Option<ReviewPolarity>,
}

impl ReviewExcerpt {
    pub fn new(value: impl Into<String>) -> Result<Self, ReviewExcerptError> {
        Self::with_polarity(value, None)
    }

    /// Preserve a source-derived polarity alongside the bounded excerpt for deterministic local
    /// classification. A missing polarity deliberately remains distinct from a neutral excerpt.
    pub fn with_polarity(
        value: impl Into<String>,
        polarity: Option<ReviewPolarity>,
    ) -> Result<Self, ReviewExcerptError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ReviewExcerptError::Blank);
        }
        if value.len() > REVIEW_EXCERPT_MAX_BYTES {
            return Err(ReviewExcerptError::TooLong);
        }
        Ok(Self {
            text: value,
            polarity,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub const fn polarity(&self) -> Option<ReviewPolarity> {
        self.polarity
    }
}

/// A bounded source-score signal retained only when it is clearly positive or negative.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewPolarity {
    Positive,
    Negative,
}

impl ReviewPolarity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Positive => "positive",
            Self::Negative => "negative",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "positive" => Some(Self::Positive),
            "negative" => Some(Self::Negative),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewExcerptError {
    Blank,
    TooLong,
}

impl fmt::Display for ReviewExcerptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blank => formatter.write_str("review excerpt must not be blank"),
            Self::TooLong => formatter.write_str("review excerpt exceeds the retained bound"),
        }
    }
}

impl std::error::Error for ReviewExcerptError {}

/// A source-adapter cursor for a later newest-first browse request.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct BrowseCursor(u64);

impl BrowseCursor {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

/// The persisted position of the newest-first browse sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowseProgress {
    /// Use the initial browse cursor for the next request, replaying it while eligible candidates
    /// remain on a partially consumed page.
    Initial,
    /// Use this explicit cursor for the next request, replaying it while eligible candidates
    /// remain on a partially consumed page.
    Continue(BrowseCursor),
    /// The source supplied no continuation; another browse request must not be fabricated.
    Exhausted,
}

/// The only discovery requests the daily selection policy may make.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CrawlDiscoveryRequest {
    NewReleases,
    NewestBrowse { cursor: Option<BrowseCursor> },
}

/// Application-owned state for one day's selection sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DailyCrawlState {
    day: CrawlDayKey,
    selected_or_processed: BTreeSet<SourceProductId>,
    new_releases_completed: bool,
    browse_progress: BrowseProgress,
}

impl DailyCrawlState {
    /// Begin a fresh day without altering any state belonging to another day.
    pub fn fresh(day: CrawlDayKey) -> Self {
        Self {
            day,
            selected_or_processed: BTreeSet::new(),
            new_releases_completed: false,
            browse_progress: BrowseProgress::Initial,
        }
    }

    /// Restore an already committed state through an application-owned persistence adapter.
    pub fn restored(
        day: CrawlDayKey,
        selected_or_processed: impl IntoIterator<Item = SourceProductId>,
        new_releases_completed: bool,
        browse_progress: BrowseProgress,
    ) -> Self {
        Self {
            day,
            selected_or_processed: selected_or_processed.into_iter().collect(),
            new_releases_completed,
            browse_progress,
        }
    }

    pub fn day(&self) -> &CrawlDayKey {
        &self.day
    }

    pub fn selected_or_processed(&self) -> &BTreeSet<SourceProductId> {
        &self.selected_or_processed
    }

    pub fn new_releases_completed(&self) -> bool {
        self.new_releases_completed
    }

    pub fn browse_progress(&self) -> BrowseProgress {
        self.browse_progress
    }
}

/// A source request together with the normalized state it may transition from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DailyCrawlDiscovery {
    state: DailyCrawlState,
    request: CrawlDiscoveryRequest,
}

impl DailyCrawlDiscovery {
    pub fn request(&self) -> CrawlDiscoveryRequest {
        self.request
    }

    pub fn state(&self) -> &DailyCrawlState {
        &self.state
    }
}

/// The policy's next action. Exhaustion is explicit and performs no discovery request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DailyCrawlAction {
    Discover(DailyCrawlDiscovery),
    Exhausted(DailyCrawlState),
}

/// A complete, not-yet-persisted daily selection transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DailyCrawlTransition {
    request: CrawlDiscoveryRequest,
    selected_product_ids: Vec<SourceProductId>,
    next_state: DailyCrawlState,
}

impl DailyCrawlTransition {
    pub fn request(&self) -> CrawlDiscoveryRequest {
        self.request
    }

    pub fn selected_product_ids(&self) -> &[SourceProductId] {
        &self.selected_product_ids
    }

    pub fn next_state(&self) -> &DailyCrawlState {
        &self.next_state
    }
}

/// Choose the next source request without mutating a persisted state.
///
/// A state from another day is intentionally ignored rather than rewritten: the supplied day
/// starts its own New Releases sequence and its own uniqueness set.
pub fn prepare_daily_crawl(
    day: CrawlDayKey,
    persisted_state: Option<DailyCrawlState>,
) -> DailyCrawlAction {
    let state = match persisted_state {
        Some(state) if state.day == day => state,
        Some(_) | None => DailyCrawlState::fresh(day),
    };

    if !state.new_releases_completed {
        return DailyCrawlAction::Discover(DailyCrawlDiscovery {
            state,
            request: CrawlDiscoveryRequest::NewReleases,
        });
    }

    match state.browse_progress {
        BrowseProgress::Initial => DailyCrawlAction::Discover(DailyCrawlDiscovery {
            state,
            request: CrawlDiscoveryRequest::NewestBrowse { cursor: None },
        }),
        BrowseProgress::Continue(cursor) => DailyCrawlAction::Discover(DailyCrawlDiscovery {
            state,
            request: CrawlDiscoveryRequest::NewestBrowse {
                cursor: Some(cursor),
            },
        }),
        BrowseProgress::Exhausted => DailyCrawlAction::Exhausted(state),
    }
}

/// Apply one successful discovery page to a prepared request without persisting it.
pub fn select_daily_crawl(
    discovery: DailyCrawlDiscovery,
    candidate_ids: impl IntoIterator<Item = SourceProductId>,
    next_browse_cursor: Option<BrowseCursor>,
) -> DailyCrawlTransition {
    select_daily_crawl_up_to(
        discovery,
        candidate_ids,
        next_browse_cursor,
        DAILY_CRAWL_SELECTION_LIMIT,
    )
}

/// Apply one successful discovery page while accepting no more than the remaining capacity of a
/// larger atomic selection. Keeping the page-local replay rule here prevents continuation callers
/// from accidentally advancing past candidates that did not fit in the current hourly batch.
pub fn select_daily_crawl_up_to(
    discovery: DailyCrawlDiscovery,
    candidate_ids: impl IntoIterator<Item = SourceProductId>,
    next_browse_cursor: Option<BrowseCursor>,
    selection_limit: usize,
) -> DailyCrawlTransition {
    let selection_limit = selection_limit.min(DAILY_CRAWL_SELECTION_LIMIT);
    let mut seen_in_run = BTreeSet::new();
    let mut selected_product_ids = Vec::new();
    let mut has_remaining_eligible_candidate = false;

    for candidate_id in candidate_ids {
        if discovery
            .state
            .selected_or_processed
            .contains(&candidate_id)
            || !seen_in_run.insert(candidate_id)
        {
            continue;
        }

        if selected_product_ids.len() < selection_limit {
            selected_product_ids.push(candidate_id);
        } else {
            has_remaining_eligible_candidate = true;
        }
    }

    let mut next_state = discovery.state;
    next_state
        .selected_or_processed
        .extend(selected_product_ids.iter().copied());
    next_state.browse_progress = match discovery.request {
        CrawlDiscoveryRequest::NewReleases => {
            next_state.new_releases_completed = true;
            BrowseProgress::Initial
        }
        CrawlDiscoveryRequest::NewestBrowse { cursor } if has_remaining_eligible_candidate => {
            match cursor {
                Some(cursor) => BrowseProgress::Continue(cursor),
                None => BrowseProgress::Initial,
            }
        }
        CrawlDiscoveryRequest::NewestBrowse { .. } => match next_browse_cursor {
            Some(cursor) => BrowseProgress::Continue(cursor),
            None => BrowseProgress::Exhausted,
        },
    };

    DailyCrawlTransition {
        request: discovery.request,
        selected_product_ids,
        next_state,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CrawlDayKeyError {
    Blank,
}

impl fmt::Display for CrawlDayKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("daily crawl key must not be blank")
    }
}

impl std::error::Error for CrawlDayKeyError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceProductIdError {
    Zero,
}

impl fmt::Display for SourceProductIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("source product identity must be non-zero")
    }
}

impl std::error::Error for SourceProductIdError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetascoreError {
    OutOfRange,
}

impl fmt::Display for MetascoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Metascore must be between 0 and 100")
    }
}

impl std::error::Error for MetascoreError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserscoreError {
    OutOfRange,
}

impl fmt::Display for UserscoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Userscore must be finite and between 0 and 10")
    }
}

impl std::error::Error for UserscoreError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GameSnapshotValidationError {
    BlankField(&'static str),
    ZeroPlatformSourceId,
    DuplicatePlatformSourceId,
    DuplicateDeveloperName,
}

impl fmt::Display for GameSnapshotValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlankField(field) => write!(formatter, "{field} must not be blank"),
            Self::ZeroPlatformSourceId => {
                formatter.write_str("platform source identity must be non-zero")
            }
            Self::DuplicatePlatformSourceId => {
                formatter.write_str("platform source identities must be unique")
            }
            Self::DuplicateDeveloperName => formatter.write_str("developer names must be unique"),
        }
    }
}

impl std::error::Error for GameSnapshotValidationError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(value: &str) -> CrawlDayKey {
        CrawlDayKey::new(value).expect("test day must be valid")
    }

    fn product(value: u64) -> SourceProductId {
        SourceProductId::new(value).expect("test product ID must be valid")
    }

    fn discovery(action: DailyCrawlAction) -> DailyCrawlDiscovery {
        match action {
            DailyCrawlAction::Discover(discovery) => discovery,
            DailyCrawlAction::Exhausted(_) => panic!("expected a discovery request"),
        }
    }

    #[test]
    fn fresh_day_always_starts_with_new_releases() {
        let action = prepare_daily_crawl(day("2026-08-14"), None);

        assert_eq!(
            discovery(action).request(),
            CrawlDiscoveryRequest::NewReleases
        );
    }

    #[test]
    fn state_from_another_day_resets_to_new_releases_without_rewriting_it() {
        let old_state = DailyCrawlState::restored(
            day("2026-08-14"),
            [product(1)],
            true,
            BrowseProgress::Continue(BrowseCursor::new(24)),
        );
        let old_state_before = old_state.clone();

        let action = prepare_daily_crawl(day("2026-08-15"), Some(old_state));

        let discovery = discovery(action);
        assert_eq!(discovery.request(), CrawlDiscoveryRequest::NewReleases);
        assert!(discovery.state().selected_or_processed().is_empty());
        assert_eq!(old_state_before.day().as_str(), "2026-08-14");
        assert_eq!(
            old_state_before.selected_or_processed(),
            &BTreeSet::from([product(1)])
        );
    }

    #[test]
    fn later_same_day_uses_saved_browse_progress() {
        let initial = DailyCrawlState::restored(
            day("2026-08-14"),
            [product(1)],
            true,
            BrowseProgress::Initial,
        );
        let continued = DailyCrawlState::restored(
            day("2026-08-14"),
            [product(1)],
            true,
            BrowseProgress::Continue(BrowseCursor::new(24)),
        );

        assert_eq!(
            discovery(prepare_daily_crawl(day("2026-08-14"), Some(initial))).request(),
            CrawlDiscoveryRequest::NewestBrowse { cursor: None }
        );
        assert_eq!(
            discovery(prepare_daily_crawl(day("2026-08-14"), Some(continued))).request(),
            CrawlDiscoveryRequest::NewestBrowse {
                cursor: Some(BrowseCursor::new(24)),
            }
        );
    }

    #[test]
    fn capped_browse_page_replays_its_cursor_until_all_eligible_candidates_are_selected() {
        let state = DailyCrawlState::restored(
            day("2026-08-14"),
            [product(2)],
            true,
            BrowseProgress::Initial,
        );
        let candidates = std::iter::once(product(2))
            .chain(std::iter::once(product(1)))
            .chain(std::iter::once(product(1)))
            .chain((3..=25).map(product));

        let transition = select_daily_crawl(
            discovery(prepare_daily_crawl(day("2026-08-14"), Some(state))),
            candidates,
            Some(BrowseCursor::new(24)),
        );

        assert_eq!(
            transition.selected_product_ids().len(),
            DAILY_CRAWL_SELECTION_LIMIT
        );
        assert_eq!(transition.selected_product_ids()[0], product(1));
        assert_eq!(transition.selected_product_ids()[1], product(3));
        assert_eq!(transition.selected_product_ids().last(), Some(&product(21)));
        assert_eq!(
            transition.next_state().browse_progress(),
            BrowseProgress::Initial
        );
        assert_eq!(
            discovery(prepare_daily_crawl(
                day("2026-08-14"),
                Some(transition.next_state().clone())
            ))
            .request(),
            CrawlDiscoveryRequest::NewestBrowse { cursor: None }
        );
    }

    #[test]
    fn consumed_browse_page_advances_to_its_explicit_continuation() {
        let state = DailyCrawlState::restored(
            day("2026-08-14"),
            [product(2)],
            true,
            BrowseProgress::Initial,
        );
        let candidates = std::iter::once(product(2))
            .chain(std::iter::once(product(1)))
            .chain(std::iter::once(product(1)))
            .chain((3..=25).map(product));
        let first = select_daily_crawl(
            discovery(prepare_daily_crawl(day("2026-08-14"), Some(state))),
            candidates.clone(),
            Some(BrowseCursor::new(24)),
        );
        let second = select_daily_crawl(
            discovery(prepare_daily_crawl(
                day("2026-08-14"),
                Some(first.next_state().clone()),
            )),
            candidates,
            Some(BrowseCursor::new(24)),
        );

        assert_eq!(second.selected_product_ids().len(), 4);
        assert_eq!(
            second.next_state().browse_progress(),
            BrowseProgress::Continue(BrowseCursor::new(24))
        );
    }

    #[test]
    fn selection_handles_zero_one_twenty_and_more_than_twenty_candidates() {
        for (candidate_count, expected_count) in [(0_u64, 0_usize), (1, 1), (20, 20), (21, 20)] {
            let candidates = (1..=candidate_count).map(product);
            let transition = select_daily_crawl(
                discovery(prepare_daily_crawl(day("2026-08-14"), None)),
                candidates,
                None,
            );
            assert_eq!(transition.selected_product_ids().len(), expected_count);
        }
    }

    #[test]
    fn new_releases_continuation_is_not_reused_as_browse_progress() {
        let transition = select_daily_crawl(
            discovery(prepare_daily_crawl(day("2026-08-14"), None)),
            [product(1)],
            Some(BrowseCursor::new(999)),
        );

        assert_eq!(transition.request(), CrawlDiscoveryRequest::NewReleases);
        assert_eq!(
            transition.next_state().browse_progress(),
            BrowseProgress::Initial
        );
    }

    #[test]
    fn exhausted_browse_has_no_follow_up_request() {
        let state = DailyCrawlState::restored(
            day("2026-08-14"),
            [product(1)],
            true,
            BrowseProgress::Initial,
        );
        let transition = select_daily_crawl(
            discovery(prepare_daily_crawl(day("2026-08-14"), Some(state))),
            [],
            None,
        );

        assert!(matches!(
            prepare_daily_crawl(day("2026-08-14"), Some(transition.next_state().clone())),
            DailyCrawlAction::Exhausted(_)
        ));
    }

    #[test]
    fn rejects_blank_days_and_zero_source_ids() {
        assert_eq!(CrawlDayKey::new(" "), Err(CrawlDayKeyError::Blank));
        assert_eq!(SourceProductId::new(0), Err(SourceProductIdError::Zero));
    }

    #[test]
    fn snapshot_scores_enforce_their_domain_bounds() {
        assert_eq!(
            Metascore::new(100)
                .expect("upper bound must be valid")
                .value(),
            100
        );
        assert_eq!(Metascore::new(101), Err(MetascoreError::OutOfRange));
        assert_eq!(
            Userscore::new(10.0)
                .expect("upper bound must be valid")
                .value(),
            10.0
        );
        assert_eq!(Userscore::new(-0.1), Err(UserscoreError::OutOfRange));
        assert_eq!(Userscore::new(10.1), Err(UserscoreError::OutOfRange));
        assert_eq!(Userscore::new(f64::NAN), Err(UserscoreError::OutOfRange));
    }

    #[test]
    fn snapshot_keeps_optional_source_data_explicit_and_rejects_duplicate_children() {
        let platform = GamePlatformScore::new(7, "pc", None, None)
            .expect("available platform with unavailable scores must be valid");
        let snapshot = GameSnapshot::new(
            product(101),
            "example-game",
            "Example Game",
            "Example description",
            None,
            None,
            vec![platform],
            Vec::new(),
        )
        .expect("explicitly absent cover, video, scores, and developers must be valid");

        assert!(snapshot.cover().is_none());
        assert!(snapshot.video().is_none());
        assert_eq!(snapshot.platform_scores()[0].metascore(), None);
        assert_eq!(snapshot.platform_scores()[0].userscore(), None);
        assert!(snapshot.developers().is_empty());

        let duplicate_platform = GamePlatformScore::new(7, "pc-renamed", None, None)
            .expect("individually valid platform");
        assert_eq!(
            GameSnapshot::new(
                product(101),
                "example-game",
                "Example Game",
                "Example description",
                None,
                None,
                vec![snapshot.platform_scores()[0].clone(), duplicate_platform],
                Vec::new(),
            ),
            Err(GameSnapshotValidationError::DuplicatePlatformSourceId)
        );
        assert_eq!(
            GameDeveloper::new(" "),
            Err(GameSnapshotValidationError::BlankField("developer name"))
        );
    }
}
