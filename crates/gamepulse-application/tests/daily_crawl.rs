#![forbid(unsafe_code)]

use std::collections::{BTreeMap, VecDeque};

use gamepulse_application::{
    BrowseCursor, BrowseProgress, CrawlDayKey, CrawlDiscoveryRequest, DailyCrawlCommit,
    DailyCrawlCommitError, DailyCrawlError, DailyCrawlOutcome, DailyCrawlSourcePort,
    DailyCrawlState, DailyCrawlStatePort, DiscoveryCandidate, DiscoveryPage, execute_daily_crawl,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StateError {
    Load,
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
    fail_load: bool,
    fail_commit: bool,
}

impl DailyCrawlStatePort for MemoryStatePort {
    type Error = StateError;

    fn load(&mut self, day: &CrawlDayKey) -> Result<Option<DailyCrawlState>, Self::Error> {
        if self.fail_load {
            return Err(StateError::Load);
        }
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
            .expect("test must supply a source response")
    }
}

fn day(value: &str) -> CrawlDayKey {
    CrawlDayKey::new(value).expect("test day must be valid")
}

fn candidate(id: u64, slug: &str) -> DiscoveryCandidate {
    DiscoveryCandidate::new(id, slug).expect("test candidate must be valid")
}

fn page(candidates: Vec<DiscoveryCandidate>, next: Option<u64>) -> DiscoveryPage {
    DiscoveryPage::new(candidates, next.map(BrowseCursor::new))
}

fn selected(outcome: DailyCrawlOutcome) -> gamepulse_application::DailyCrawlSelection {
    match outcome {
        DailyCrawlOutcome::Selected(selection) => selection,
        DailyCrawlOutcome::Exhausted(_) => panic!("expected a selection"),
    }
}

#[test]
fn fresh_day_uses_new_releases_and_preserves_source_order() {
    let mut state = MemoryStatePort::default();
    let mut source = FakeSourcePort::from_pages([Ok(page(
        vec![candidate(3, "third"), candidate(1, "first")],
        Some(999),
    ))]);

    let selection = selected(
        execute_daily_crawl(&mut state, &mut source, day("2026-08-14"))
            .expect("selection must succeed"),
    );

    assert_eq!(source.calls, [CrawlDiscoveryRequest::NewReleases]);
    assert_eq!(
        selection
            .selected()
            .iter()
            .map(|candidate| candidate.source_slug())
            .collect::<Vec<_>>(),
        ["third", "first"]
    );
    assert_eq!(selection.state().browse_progress(), BrowseProgress::Initial);
    assert_eq!(state.commits.len(), 1);
    assert_eq!(state.commits[0].expected_previous_state(), None);
}

#[test]
fn later_runs_use_newest_browse_and_saved_continuation() {
    let mut state = MemoryStatePort::default();
    let mut source = FakeSourcePort::from_pages([
        Ok(page(vec![], Some(999))),
        Ok(page(vec![candidate(2, "second")], Some(24))),
        Ok(page(vec![candidate(3, "third")], None)),
    ]);

    selected(
        execute_daily_crawl(&mut state, &mut source, day("2026-08-14"))
            .expect("first selection must succeed"),
    );
    selected(
        execute_daily_crawl(&mut state, &mut source, day("2026-08-14"))
            .expect("second selection must succeed"),
    );
    let third = selected(
        execute_daily_crawl(&mut state, &mut source, day("2026-08-14"))
            .expect("third selection must succeed"),
    );

    assert_eq!(
        source.calls,
        [
            CrawlDiscoveryRequest::NewReleases,
            CrawlDiscoveryRequest::NewestBrowse { cursor: None },
            CrawlDiscoveryRequest::NewestBrowse {
                cursor: Some(BrowseCursor::new(24)),
            },
        ]
    );
    assert_eq!(third.state().browse_progress(), BrowseProgress::Exhausted);
}

#[test]
fn two_browse_runs_consume_all_twenty_four_eligible_candidates_before_advancing() {
    let candidates = (1..=24)
        .map(|id| candidate(id, &format!("game-{id}")))
        .collect::<Vec<_>>();
    let mut state = MemoryStatePort::default();
    let mut source = FakeSourcePort::from_pages([
        Ok(page(vec![], None)),
        Ok(page(candidates.clone(), Some(24))),
        Ok(page(candidates, Some(24))),
    ]);

    selected(
        execute_daily_crawl(&mut state, &mut source, day("2026-08-14"))
            .expect("new releases selection must succeed"),
    );
    let first_browse = selected(
        execute_daily_crawl(&mut state, &mut source, day("2026-08-14"))
            .expect("first browse selection must succeed"),
    );
    let second_browse = selected(
        execute_daily_crawl(&mut state, &mut source, day("2026-08-14"))
            .expect("second browse selection must succeed"),
    );

    assert_eq!(first_browse.selected().len(), 20);
    assert_eq!(
        second_browse
            .selected()
            .iter()
            .map(|candidate| candidate.source_product_id().value())
            .collect::<Vec<_>>(),
        [21, 22, 23, 24]
    );
    assert_eq!(
        source.calls,
        [
            CrawlDiscoveryRequest::NewReleases,
            CrawlDiscoveryRequest::NewestBrowse { cursor: None },
            CrawlDiscoveryRequest::NewestBrowse { cursor: None },
        ]
    );
    assert_eq!(
        second_browse.state().browse_progress(),
        BrowseProgress::Continue(BrowseCursor::new(24))
    );
    assert_eq!(second_browse.state().selected_or_processed().len(), 24);
}

#[test]
fn numeric_identity_deduplicates_changed_slugs_and_intra_run_candidates() {
    let existing = gamepulse_application::SourceProductId::new(7).expect("valid ID");
    let mut state = MemoryStatePort {
        states: BTreeMap::from([(
            day("2026-08-14"),
            DailyCrawlState::restored(day("2026-08-14"), [existing], true, BrowseProgress::Initial),
        )]),
        ..Default::default()
    };
    let mut source = FakeSourcePort::from_pages([Ok(page(
        vec![
            candidate(7, "renamed-seven"),
            candidate(8, "first-eight"),
            candidate(8, "renamed-eight"),
            candidate(9, "nine"),
        ],
        None,
    ))]);

    let selection = selected(
        execute_daily_crawl(&mut state, &mut source, day("2026-08-14"))
            .expect("selection must succeed"),
    );

    assert_eq!(
        selection
            .selected()
            .iter()
            .map(|candidate| candidate.source_slug())
            .collect::<Vec<_>>(),
        ["first-eight", "nine"]
    );
}

#[test]
fn day_rollover_starts_a_new_sequence_without_rewriting_the_previous_day() {
    let mut state = MemoryStatePort::default();
    let mut source = FakeSourcePort::from_pages([
        Ok(page(vec![candidate(1, "first")], None)),
        Ok(page(vec![candidate(1, "new-day-first")], None)),
    ]);

    selected(
        execute_daily_crawl(&mut state, &mut source, day("2026-08-14"))
            .expect("first day must succeed"),
    );
    let next_day = selected(
        execute_daily_crawl(&mut state, &mut source, day("2026-08-15"))
            .expect("second day must succeed"),
    );

    assert_eq!(source.calls, [CrawlDiscoveryRequest::NewReleases; 2]);
    assert_eq!(next_day.selected()[0].source_slug(), "new-day-first");
    assert_eq!(state.states.len(), 2);
    assert_eq!(
        state
            .states
            .get(&day("2026-08-14"))
            .expect("old state must remain")
            .selected_or_processed()
            .len(),
        1
    );
}

#[test]
fn exhausted_continuation_does_not_issue_another_source_request() {
    let mut state = MemoryStatePort::default();
    let mut source = FakeSourcePort::from_pages([
        Ok(page(vec![candidate(1, "first")], None)),
        Ok(page(vec![], None)),
    ]);

    selected(
        execute_daily_crawl(&mut state, &mut source, day("2026-08-14"))
            .expect("first selection must succeed"),
    );
    selected(
        execute_daily_crawl(&mut state, &mut source, day("2026-08-14"))
            .expect("browse selection must succeed"),
    );
    let outcome = execute_daily_crawl(&mut state, &mut source, day("2026-08-14"))
        .expect("exhaustion is a successful no-op");

    assert!(matches!(outcome, DailyCrawlOutcome::Exhausted(_)));
    assert_eq!(source.calls.len(), 2);
    assert_eq!(state.commits.len(), 2);
}

#[test]
fn source_or_commit_failure_does_not_publish_a_partial_transition() {
    let mut source_failure_state = MemoryStatePort::default();
    let mut failing_source = FakeSourcePort::from_pages([Err(SourceError::Unavailable)]);

    assert_eq!(
        execute_daily_crawl(
            &mut source_failure_state,
            &mut failing_source,
            day("2026-08-14")
        ),
        Err(DailyCrawlError::Source(SourceError::Unavailable))
    );
    assert!(source_failure_state.states.is_empty());
    assert!(source_failure_state.commits.is_empty());

    let mut commit_failure_state = MemoryStatePort {
        fail_commit: true,
        ..Default::default()
    };
    let mut source = FakeSourcePort::from_pages([Ok(page(vec![candidate(1, "first")], None))]);

    assert_eq!(
        execute_daily_crawl(&mut commit_failure_state, &mut source, day("2026-08-14")),
        Err(DailyCrawlError::Commit(StateError::Commit))
    );
    assert!(commit_failure_state.states.is_empty());
    assert!(commit_failure_state.commits.is_empty());
}

#[test]
fn commit_payload_preserves_the_loaded_previous_state() {
    let previous = DailyCrawlState::restored(
        day("2026-08-14"),
        [gamepulse_application::SourceProductId::new(1).expect("valid ID")],
        true,
        BrowseProgress::Continue(BrowseCursor::new(24)),
    );
    let mut state = MemoryStatePort {
        states: BTreeMap::from([(day("2026-08-14"), previous.clone())]),
        ..Default::default()
    };
    let mut source = FakeSourcePort::from_pages([Ok(page(vec![candidate(2, "second")], None))]);

    selected(
        execute_daily_crawl(&mut state, &mut source, day("2026-08-14"))
            .expect("selection must succeed"),
    );

    assert_eq!(state.commits.len(), 1);
    assert_eq!(state.commits[0].expected_previous_state(), Some(&previous));
}

#[test]
fn public_commit_constructor_rejects_completion_and_exhaustion_regressions() {
    let complete = DailyCrawlState::restored(day("2026-08-14"), [], true, BrowseProgress::Initial);
    let incomplete =
        DailyCrawlState::restored(day("2026-08-14"), [], false, BrowseProgress::Initial);
    assert_eq!(
        DailyCrawlCommit::new(Some(complete), incomplete, vec![]),
        Err(DailyCrawlCommitError::NewReleasesCompletionRegression)
    );

    let exhausted =
        DailyCrawlState::restored(day("2026-08-14"), [], true, BrowseProgress::Exhausted);
    let continued = DailyCrawlState::restored(
        day("2026-08-14"),
        [],
        true,
        BrowseProgress::Continue(BrowseCursor::new(24)),
    );
    assert_eq!(
        DailyCrawlCommit::new(Some(exhausted), continued, vec![]),
        Err(DailyCrawlCommitError::BrowseExhaustionRegression)
    );
}
