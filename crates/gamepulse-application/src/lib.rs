#![forbid(unsafe_code)]

//! Application use cases and ports for GamePulse.

use std::collections::BTreeSet;
use std::fmt;

pub use gamepulse_domain::{
    APP_NAME, BrowseCursor, BrowseProgress, CrawlDayKey, CrawlDayKeyError, CrawlDiscoveryRequest,
    DailyCrawlAction, DailyCrawlState, DailyCrawlTransition, SourceProductId, SourceProductIdError,
};
use gamepulse_domain::{prepare_daily_crawl, select_daily_crawl};

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
    Commit(StateError),
}
