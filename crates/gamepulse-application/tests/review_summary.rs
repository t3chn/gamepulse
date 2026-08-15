#![forbid(unsafe_code)]

use gamepulse_application::{
    GameReviewRefresh, GameReviewRefreshError, JobTimestamp, ReviewExcerpt, ReviewInput,
    ReviewKind, ReviewSummaryJobSchedule, SourceProductId,
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
