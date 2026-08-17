#![forbid(unsafe_code)]

use std::collections::{BTreeMap, VecDeque};

use gamepulse_application::{
    BrowseCursor, BrowseProgress, CrawlDayKey, CrawlDiscoveryRequest, DailyCrawlCommit,
    DailyCrawlError, DailyCrawlOutcome, DailyCrawlSourcePort, DailyCrawlState, DailyCrawlStatePort,
    DiscoveryCandidate, DiscoveryPage, SourceProductId, execute_daily_crawl,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StateError {
    Commit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceError {
    Unavailable,
}

#[derive(Default)]
struct MemoryStatePort {
    states: BTreeMap<CrawlDayKey, DailyCrawlState>,
    commits: Vec<DailyCrawlCommit>,
    fail_commit: bool,
}

impl DailyCrawlStatePort for MemoryStatePort {
    type Error = StateError;

    fn load(&mut self, day: &CrawlDayKey) -> Result<Option<DailyCrawlState>, Self::Error> {
        Ok(self.states.get(day).cloned())
    }

    fn commit(&mut self, commit: DailyCrawlCommit) -> Result<(), Self::Error> {
        if self.fail_commit {
            return Err(StateError::Commit);
        }
        self.states
            .insert(commit.state().day().clone(), commit.state().clone());
        self.commits.push(commit);
        Ok(())
    }
}

struct FakeSourcePort {
    responses: VecDeque<Result<DiscoveryPage, SourceError>>,
    calls: Vec<CrawlDiscoveryRequest>,
}

impl FakeSourcePort {
    fn from_pages(pages: impl IntoIterator<Item = Result<DiscoveryPage, SourceError>>) -> Self {
        Self {
            responses: pages.into_iter().collect(),
            calls: Vec::new(),
        }
    }
}

impl DailyCrawlSourcePort for FakeSourcePort {
    type Error = SourceError;

    fn discover(&mut self, request: CrawlDiscoveryRequest) -> Result<DiscoveryPage, Self::Error> {
        self.calls.push(request);
        self.responses
            .pop_front()
            .expect("test must supply one source response per request")
    }
}

fn day() -> CrawlDayKey {
    CrawlDayKey::new("2026-08-14").expect("test day must be valid")
}

fn candidate(id: u64, slug: impl Into<String>) -> DiscoveryCandidate {
    DiscoveryCandidate::new(id, slug).expect("test candidate must be valid")
}

fn candidates(ids: impl IntoIterator<Item = u64>) -> Vec<DiscoveryCandidate> {
    ids.into_iter()
        .map(|id| candidate(id, format!("game-{id}")))
        .collect()
}

fn page(candidates: Vec<DiscoveryCandidate>, next: Option<u64>) -> DiscoveryPage {
    DiscoveryPage::new(candidates, next.map(BrowseCursor::new))
}

fn selection(outcome: DailyCrawlOutcome) -> gamepulse_application::DailyCrawlSelection {
    match outcome {
        DailyCrawlOutcome::Selected(selection) => selection,
        DailyCrawlOutcome::Exhausted(_) => panic!("expected an exact selection"),
    }
}

#[test]
fn first_run_short_new_releases_continues_to_an_atomic_exact_twenty() {
    let mut state = MemoryStatePort::default();
    let mut source = FakeSourcePort::from_pages([
        Ok(page(candidates(1..=4), Some(4))),
        Ok(page(candidates(5..=20), Some(24))),
    ]);

    let selected = selection(
        execute_daily_crawl(&mut state, &mut source, day()).expect("selection must succeed"),
    );

    assert_eq!(selected.selected().len(), 20);
    assert_eq!(
        selected
            .selected()
            .iter()
            .map(|candidate| candidate.source_product_id().value())
            .collect::<Vec<_>>(),
        (1..=20).collect::<Vec<_>>(),
    );
    assert_eq!(
        source.calls,
        [
            CrawlDiscoveryRequest::NewReleases,
            CrawlDiscoveryRequest::NewestBrowse { cursor: None },
        ],
    );
    assert_eq!(state.commits.len(), 1);
    assert_eq!(state.commits[0].selected().len(), 20);
}

#[test]
fn initial_new_releases_exactly_twenty_does_not_browse() {
    let mut state = MemoryStatePort::default();
    let mut source = FakeSourcePort::from_pages([Ok(page(candidates(1..=20), Some(20)))]);

    let selected = selection(
        execute_daily_crawl(&mut state, &mut source, day()).expect("selection must succeed"),
    );

    assert_eq!(selected.selected().len(), 20);
    assert_eq!(source.calls, [CrawlDiscoveryRequest::NewReleases]);
    assert_eq!(selected.state().browse_progress(), BrowseProgress::Initial);
    assert_eq!(state.commits.len(), 1);
}

#[test]
fn initial_new_releases_above_twenty_commits_stable_first_twenty() {
    let mut state = MemoryStatePort::default();
    let mut source = FakeSourcePort::from_pages([Ok(page(candidates(1..=24), Some(24)))]);

    let selected = selection(
        execute_daily_crawl(&mut state, &mut source, day()).expect("selection must succeed"),
    );

    assert_eq!(
        selected
            .selected()
            .iter()
            .map(|candidate| candidate.source_product_id().value())
            .collect::<Vec<_>>(),
        (1..=20).collect::<Vec<_>>(),
    );
    assert_eq!(source.calls, [CrawlDiscoveryRequest::NewReleases]);
    assert_eq!(state.commits.len(), 1);
}

#[test]
fn first_run_deduplicates_processed_and_repeated_source_identities() {
    let already_processed = SourceProductId::new(1).expect("test ID must be valid");
    let mut state = MemoryStatePort {
        states: BTreeMap::from([(
            day(),
            DailyCrawlState::restored(day(), [already_processed], false, BrowseProgress::Initial),
        )]),
        ..Default::default()
    };
    let mut source = FakeSourcePort::from_pages([
        Ok(page(
            vec![
                candidate(1, "renamed-one"),
                candidate(2, "first-two"),
                candidate(2, "renamed-two"),
                candidate(3, "three"),
            ],
            Some(4),
        )),
        Ok(page(candidates(4..=21), None)),
    ]);

    let selected = selection(
        execute_daily_crawl(&mut state, &mut source, day()).expect("selection must succeed"),
    );

    assert_eq!(selected.selected().len(), 20);
    assert_eq!(selected.selected()[0].source_slug(), "first-two");
    assert_eq!(
        selected
            .selected()
            .iter()
            .map(|candidate| candidate.source_product_id().value())
            .collect::<Vec<_>>(),
        (2..=21).collect::<Vec<_>>(),
    );
    assert_eq!(state.commits.len(), 1);
}

#[test]
fn source_exhaustion_returns_fail_closed_without_a_partial_commit() {
    let mut state = MemoryStatePort::default();
    let mut source = FakeSourcePort::from_pages([
        Ok(page(candidates(1..=2), Some(2))),
        Ok(page(candidates(3..=19), None)),
    ]);

    let outcome = execute_daily_crawl(&mut state, &mut source, day())
        .expect("exhaustion must be a controlled outcome");

    assert!(matches!(outcome, DailyCrawlOutcome::Exhausted(_)));
    assert_eq!(
        source.calls,
        [
            CrawlDiscoveryRequest::NewReleases,
            CrawlDiscoveryRequest::NewestBrowse { cursor: None },
        ],
    );
    assert!(state.commits.is_empty());
    assert!(state.states.is_empty());
}

#[test]
fn bounded_browse_ceiling_fails_without_a_partial_commit() {
    let mut state = MemoryStatePort::default();
    let mut responses = vec![Ok(page(Vec::new(), Some(24)))];
    responses.extend((1..=8).map(|index| Ok(page(Vec::new(), Some(index * 24)))));
    let mut source = FakeSourcePort::from_pages(responses);

    assert!(matches!(
        execute_daily_crawl(&mut state, &mut source, day()),
        Err(DailyCrawlError::BrowseContinuationLimit)
    ));
    assert_eq!(source.calls.len(), 9);
    assert!(state.commits.is_empty());
    assert!(state.states.is_empty());
}

#[test]
fn source_or_commit_failure_never_publishes_partial_selection() {
    let mut source_failure_state = MemoryStatePort::default();
    let mut failing_source = FakeSourcePort::from_pages([Err(SourceError::Unavailable)]);
    assert_eq!(
        execute_daily_crawl(&mut source_failure_state, &mut failing_source, day()),
        Err(DailyCrawlError::Source(SourceError::Unavailable))
    );
    assert!(source_failure_state.commits.is_empty());

    let mut commit_failure_state = MemoryStatePort {
        fail_commit: true,
        ..Default::default()
    };
    let mut source = FakeSourcePort::from_pages([Ok(page(candidates(1..=20), None))]);
    assert_eq!(
        execute_daily_crawl(&mut commit_failure_state, &mut source, day()),
        Err(DailyCrawlError::Commit(StateError::Commit))
    );
    assert!(commit_failure_state.commits.is_empty());
    assert!(commit_failure_state.states.is_empty());
}
