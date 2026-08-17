#![forbid(unsafe_code)]

use gamepulse_application::{
    GameReviewRefresh, GameReviewRefreshError, JobTimestamp, ReviewExcerpt, ReviewInput,
    ReviewKind, ReviewPolarity, ReviewSummaryJobSchedule, SourceProductId,
};

fn snapshot() -> gamepulse_application::GameSnapshot {
    gamepulse_application::GameSnapshot::new(
        SourceProductId::new(101).expect("test identity must be valid"),
        "example-game",
        "Example Game",
        "Synthetic description",
        None,
        None,
        Vec::new(),
        Vec::new(),
    )
    .expect("test snapshot must be valid")
}

fn input(kind: ReviewKind, excerpts: &[&str]) -> ReviewInput {
    ReviewInput::new(
        SourceProductId::new(101).expect("test identity must be valid"),
        kind,
        excerpts
            .iter()
            .map(|excerpt| ReviewExcerpt::new(*excerpt).expect("test excerpt must be valid"))
            .collect(),
    )
    .expect("test review input must be valid")
}

fn refresh(critic: ReviewInput, user: ReviewInput) -> GameReviewRefresh {
    GameReviewRefresh::new(
        snapshot(),
        critic,
        user,
        ReviewSummaryJobSchedule::new(2).expect("test schedule must be valid"),
        JobTimestamp::new(10).expect("test timestamp must be valid"),
    )
    .expect("test refresh must be valid")
}

#[test]
fn kind_separated_hashes_produce_exactly_two_fingerprint_scoped_jobs() {
    let refresh = refresh(
        input(ReviewKind::Critic, &["Critic synthetic praise."]),
        input(ReviewKind::User, &["User synthetic complaint."]),
    );

    assert_ne!(
        refresh.input(ReviewKind::Critic).content_hash(),
        refresh.input(ReviewKind::User).content_hash()
    );
    assert_eq!(refresh.jobs().len(), 2);
    assert!(refresh.jobs().iter().all(|job| {
        job.job_type() == "llm.review-summary"
            && job.work_ref().contains(refresh.fingerprint().as_str())
    }));
    assert_ne!(
        refresh.input(ReviewKind::Critic).content_hash().as_str(),
        refresh.fingerprint().as_str()
    );
}

#[test]
fn legacy_unpolarized_hashes_stay_stable_and_polarity_hashes_use_v2_encoding() {
    const LEGACY_EXCERPT: &str = "A great legacy review excerpt.";
    const LEGACY_HASH: &str = "00bcb53e4dcdb6a2fb1614b107de5495101bd18f9fc776b19713e16eb6c437f1";
    const POLARITY_AWARE_V2_HASH: &str =
        "43b669036eceb450670c995ae130ba4624f40bd40d49d5e0b989def51cfd5dfa";

    let legacy = input(ReviewKind::Critic, &[LEGACY_EXCERPT]);
    let polarity_aware = ReviewInput::new(
        SourceProductId::new(101).expect("test identity must be valid"),
        ReviewKind::Critic,
        vec![
            ReviewExcerpt::with_polarity(LEGACY_EXCERPT, Some(ReviewPolarity::Positive))
                .expect("test excerpt must be valid"),
        ],
    )
    .expect("test review input must be valid");

    assert_eq!(legacy.content_hash().as_str(), LEGACY_HASH);
    assert_eq!(
        polarity_aware.content_hash().as_str(),
        POLARITY_AWARE_V2_HASH
    );
    assert_ne!(legacy.content_hash(), polarity_aware.content_hash());
}

#[test]
fn refresh_rejects_critic_user_mixing_and_changes_fingerprint_when_content_changes() {
    let critic = input(ReviewKind::Critic, &["Critic synthetic praise."]);
    let user = input(ReviewKind::User, &["User synthetic complaint."]);
    let first = refresh(critic.clone(), user.clone());
    let second = refresh(
        input(ReviewKind::Critic, &["Critic synthetic changed praise."]),
        user,
    );

    assert_ne!(first.fingerprint(), second.fingerprint());
    assert_eq!(
        GameReviewRefresh::new(
            snapshot(),
            input(ReviewKind::User, &["wrong kind"]),
            critic,
            ReviewSummaryJobSchedule::new(1).expect("test schedule must be valid"),
            JobTimestamp::new(1).expect("test timestamp must be valid"),
        ),
        Err(GameReviewRefreshError::ReviewKindsMustBeSeparate)
    );
}
