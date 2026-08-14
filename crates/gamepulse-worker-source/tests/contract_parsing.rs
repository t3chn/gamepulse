#![forbid(unsafe_code)]

use gamepulse_worker_source::{
    GameId, GameIdentity, ListMode, ReviewKind, SourceError, parse_game_detail, parse_listing_page,
    parse_review_page, parse_user_score_summary,
};

const LISTING: &str = include_str!("fixtures/listing-page.json");
const DETAIL: &str = include_str!("fixtures/product-detail.json");
const CRITIC_REVIEWS: &str = include_str!("fixtures/review-page.json");
const USER_REVIEWS: &str = include_str!("fixtures/user-review-page.json");
const USER_SCORE: &str = include_str!("fixtures/user-score.json");

fn example_game() -> GameIdentity {
    GameIdentity {
        id: GameId(101),
        slug: "example-game".to_owned(),
    }
}

#[test]
fn parses_new_releases_and_semantic_continuation() {
    let page =
        parse_listing_page(ListMode::NewReleases, 0, 20, LISTING).expect("fixture must parse");

    assert_eq!(page.games.len(), 2);
    assert_eq!(page.games[0].id.0, 101);
    assert_eq!(page.games[0].metascore.expect("score").value(), 82);
    assert_eq!(page.games[0].userscore.expect("score").value(), 8.4);
    assert_eq!(page.games[1].metascore, None);
    assert_eq!(page.games[1].userscore, None);
    assert_eq!(page.next.expect("next").offset, 20);
}

#[test]
fn rejects_continuations_that_do_not_match_the_request_context() {
    let host_mismatch = LISTING.replace("backend.metacritic.com", "invalid.example.test");
    let path_mismatch = LISTING.replace("/finder/metacritic/web", "/finder/other/web");
    let non_advancing = LISTING.replace("offset=20&limit=20", "offset=0&limit=20");
    let limit_mismatch = LISTING.replace("offset=20&limit=20", "offset=20&limit=10");
    let beyond_total = LISTING.replace("\"totalResults\": 42", "\"totalResults\": 20");
    let duplicate_offset_after_invalid =
        LISTING.replace("offset=20&limit=20", "offset=bad&offset=20&limit=20");
    let duplicate_offset = LISTING.replace("offset=20&limit=20", "offset=20&offset=20&limit=20");
    let duplicate_limit_after_invalid =
        LISTING.replace("offset=20&limit=20", "offset=20&limit=bad&limit=20");
    let duplicate_limit = LISTING.replace("offset=20&limit=20", "offset=20&limit=20&limit=20");

    for body in [
        host_mismatch.as_str(),
        path_mismatch.as_str(),
        non_advancing.as_str(),
        limit_mismatch.as_str(),
        beyond_total.as_str(),
        duplicate_offset_after_invalid.as_str(),
        duplicate_offset.as_str(),
        duplicate_limit_after_invalid.as_str(),
        duplicate_limit.as_str(),
    ] {
        assert!(matches!(
            parse_listing_page(ListMode::NewReleases, 0, 20, body),
            Err(SourceError::InvalidContinuation)
        ));
    }
    assert!(matches!(
        parse_listing_page(ListMode::NewReleases, u32::MAX, 1, LISTING),
        Err(SourceError::InvalidContinuation)
    ));
}

#[test]
fn parses_required_detail_fields_and_optional_score_variants() {
    let detail = parse_game_detail(&example_game(), DETAIL).expect("fixture must parse");

    assert_eq!(detail.id.0, 101);
    assert_eq!(detail.cover_image().expect("cover").filename, "cover.jpg");
    assert_eq!(detail.platforms.len(), 2);
    assert_eq!(detail.platforms[1].metascore, None);
    assert_eq!(detail.developers, ["Example Studio"]);
    assert_eq!(detail.genres[0].name, "Adventure");
    assert!(detail.video.is_some());
}

#[test]
fn rejects_detail_identity_mismatches() {
    let id_mismatch = DETAIL.replace("\"id\": 101", "\"id\": 999");
    let slug_mismatch = DETAIL.replace("\"slug\": \"example-game\"", "\"slug\": \"other-game\"");

    for body in [id_mismatch, slug_mismatch] {
        assert!(matches!(
            parse_game_detail(&example_game(), &body),
            Err(SourceError::MismatchedGameIdentity)
        ));
    }
}

#[test]
fn validates_platform_user_score_self_link() {
    let summary =
        parse_user_score_summary("example-game", "pc", USER_SCORE).expect("fixture must parse");

    assert_eq!(summary.score.expect("score").value(), 8.4);
    assert!(matches!(
        parse_user_score_summary(
            "example-game",
            "pc",
            &USER_SCORE.replace("backend.metacritic.com", "invalid.example.test"),
        ),
        Err(SourceError::MismatchedSelfLink { .. })
    ));
    assert!(matches!(
        parse_user_score_summary(
            "example-game",
            "pc",
            &USER_SCORE.replace("/platform/pc/", "/platform/console/"),
        ),
        Err(SourceError::MismatchedSelfLink { .. })
    ));
}

#[test]
fn preserves_critic_and_user_review_kinds_without_review_text() {
    let critic = parse_review_page(ReviewKind::Critic, "example-game", 0, 3, CRITIC_REVIEWS)
        .expect("critic fixture must parse");
    let user = parse_review_page(ReviewKind::User, "example-game", 0, 3, USER_REVIEWS)
        .expect("user fixture must parse");

    assert_eq!(critic.kind, ReviewKind::Critic);
    assert_eq!(user.kind, ReviewKind::User);
    assert_eq!(critic.reviews[0].score, Some(95.0));
    assert_eq!(user.reviews[0].score, Some(9.5));
    assert!(critic.reviews.iter().all(|review| !review.quote_available));
    assert_eq!(critic.next.expect("next").offset, 3);
    assert_eq!(user.next.expect("next").offset, 3);
}

#[test]
fn rejects_malformed_scores_and_continuations() {
    let malformed_score = LISTING.replace("\"score\": 82", "\"score\": \"not-a-number\"");
    let malformed_continuation = LISTING.replace("offset=20&limit=20", "offset=next");

    assert!(matches!(
        parse_listing_page(ListMode::NewReleases, 0, 20, &malformed_score),
        Err(SourceError::InvalidScore { .. })
    ));
    assert!(matches!(
        parse_listing_page(ListMode::NewReleases, 0, 20, &malformed_continuation),
        Err(SourceError::InvalidContinuation)
    ));
}

#[test]
fn user_score_summary_degrades_for_null_and_rejects_invalid_data() {
    let null_score = USER_SCORE.replace("\"score\": 8.4", "\"score\": null");
    let invalid_score = USER_SCORE.replace("\"score\": 8.4", "\"score\": 11");

    assert_eq!(
        parse_user_score_summary("example-game", "pc", &null_score)
            .expect("null is explicit unavailability")
            .score,
        None
    );
    assert!(matches!(
        parse_user_score_summary("example-game", "pc", &invalid_score),
        Err(SourceError::InvalidScore { .. })
    ));
}
