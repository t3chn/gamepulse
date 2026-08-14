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

        if selected_product_ids.len() < DAILY_CRAWL_SELECTION_LIMIT {
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
}
