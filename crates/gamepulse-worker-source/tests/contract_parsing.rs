#![forbid(unsafe_code)]

use gamepulse_domain::SourceProductId;
use gamepulse_worker_source::{
    GameId, GameIdentity, ListMode, ReviewKind, SourceError, SourceIngestionFailureCategory,
    classify_review_page_source_error, map_listing_page_for_daily_crawl, map_review_page_to_input,
    parse_game_detail, parse_listing_page, parse_review_page, parse_user_score_summary,
};

const LISTING: &str = include_str!("fixtures/listing-page.json");
const DETAIL: &str = include_str!("fixtures/product-detail.json");
const CRITIC_REVIEWS: &str = include_str!("fixtures/review-page.json");
const USER_REVIEWS: &str = include_str!("fixtures/user-review-page.json");
const USER_SCORE: &str = include_str!("fixtures/user-score.json");
const M011_CRITIC_REVIEWS: &str = include_str!("fixtures/m011-critic-review-page.json");
const M011_USER_REVIEWS: &str = include_str!("fixtures/m011-user-review-page.json");
const M015_CRITIC_SERVER_CLAMP: &str = include_str!("fixtures/m015-critic-server-clamp-page.json");
const M017_REVIEW_TERMINAL_WITH_ITEMS: &str =
    include_str!("fixtures/m017-review-terminal-with-items.json");
const M017_REVIEW_TERMINAL_EMPTY: &str = include_str!("fixtures/m017-review-terminal-empty.json");

fn example_game() -> GameIdentity {
    GameIdentity {
        id: GameId(101),
        slug: "example-game".to_owned(),
    }
}

fn without_next(body: &str) -> String {
    let mut document: serde_json::Value = serde_json::from_str(body).expect("fixture must decode");
    document["links"]
        .as_object_mut()
        .expect("fixture links must be an object")
        .remove("next");
    serde_json::to_string(&document).expect("fixture must encode")
}

fn with_explicit_null_next(body: &str) -> String {
    let mut document: serde_json::Value = serde_json::from_str(body).expect("fixture must decode");
    document["links"]["next"] = serde_json::Value::Null;
    serde_json::to_string(&document).expect("fixture must encode")
}

fn with_explicit_null_href(body: &str) -> String {
    let mut document: serde_json::Value = serde_json::from_str(body).expect("fixture must decode");
    document["links"]["next"]["href"] = serde_json::Value::Null;
    serde_json::to_string(&document).expect("fixture must encode")
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
fn maps_source_listing_to_the_application_discovery_contract() {
    let listing =
        parse_listing_page(ListMode::NewestBrowse, 0, 20, LISTING).expect("fixture must parse");

    let page = map_listing_page_for_daily_crawl(&listing).expect("mapping must preserve valid IDs");

    assert_eq!(page.candidates().len(), 2);
    assert_eq!(page.candidates()[0].source_product_id().value(), 101);
    assert_eq!(page.candidates()[0].source_slug(), "example-game");
    assert_eq!(page.next_browse_cursor().expect("next").value(), 20);
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
fn maps_only_one_bounded_first_page_into_separate_synthetic_review_inputs() {
    let critic = parse_review_page(
        ReviewKind::Critic,
        "example-game",
        0,
        20,
        M011_CRITIC_REVIEWS,
    )
    .expect("synthetic critic fixture must parse");
    let user = parse_review_page(ReviewKind::User, "example-game", 0, 20, M011_USER_REVIEWS)
        .expect("synthetic user fixture must parse");
    let source_product_id = SourceProductId::new(101).expect("test identity must be valid");

    let critic_input =
        map_review_page_to_input(source_product_id, &critic).expect("critic input must map");
    let user_input =
        map_review_page_to_input(source_product_id, &user).expect("user input must map");

    assert_eq!(critic_input.kind(), ReviewKind::Critic);
    assert_eq!(user_input.kind(), ReviewKind::User);
    assert_eq!(critic_input.excerpts().len(), 2);
    assert_eq!(user_input.excerpts().len(), 2);
    assert_ne!(critic_input.content_hash(), user_input.content_hash());
}

#[test]
fn accepts_only_the_verified_server_clamp_for_the_first_critic_page() {
    let page = parse_review_page(
        ReviewKind::Critic,
        "example-game",
        0,
        20,
        M015_CRITIC_SERVER_CLAMP,
    )
    .expect("verified server clamp must parse");
    let input = map_review_page_to_input(
        SourceProductId::new(101).expect("test identity must be valid"),
        &page,
    )
    .expect("server-clamped first page must remain eligible for bounded ingestion");

    assert_eq!(page.reviews.len(), 10);
    assert_eq!(page.total_results, 12);
    assert_eq!(
        page.next,
        Some(gamepulse_worker_source::Continuation {
            offset: 10,
            limit: 10,
        })
    );
    assert!(input.excerpts().is_empty());

    let host_mismatch =
        M015_CRITIC_SERVER_CLAMP.replace("backend.metacritic.com", "invalid.example.test");
    let scheme_mismatch = M015_CRITIC_SERVER_CLAMP.replacen("https://", "http://", 1);
    let path_mismatch = M015_CRITIC_SERVER_CLAMP.replace(
        "/reviews/metacritic/critic/games/example-game/web",
        "/reviews/metacritic/critic/games/other-game/web",
    );
    let non_advancing = M015_CRITIC_SERVER_CLAMP.replace("offset=10&limit=10", "offset=0&limit=10");
    let inconsistent_limit =
        M015_CRITIC_SERVER_CLAMP.replace("offset=10&limit=10", "offset=10&limit=9");
    let duplicate_offset =
        M015_CRITIC_SERVER_CLAMP.replace("offset=10&limit=10", "offset=10&offset=10&limit=10");
    let duplicate_limit =
        M015_CRITIC_SERVER_CLAMP.replace("offset=10&limit=10", "offset=10&limit=10&limit=10");
    let total_boundary =
        M015_CRITIC_SERVER_CLAMP.replace("\"totalResults\": 12", "\"totalResults\": 10");
    let requested_limit_bypass = M015_CRITIC_SERVER_CLAMP
        .replace("\"totalResults\": 12", "\"totalResults\": 30")
        .replace("offset=10&limit=10", "offset=20&limit=20");
    let item_count_mismatch = M015_CRITIC_SERVER_CLAMP.replace(
        "      { \"id\": \"synthetic-clamp-10\", \"score\": 0, \"quote\": null }",
        "      { \"id\": \"synthetic-clamp-10\", \"score\": 0, \"quote\": null },\n      { \"id\": \"synthetic-clamp-11\", \"score\": 0, \"quote\": null }",
    );

    for body in [
        host_mismatch.as_str(),
        scheme_mismatch.as_str(),
        path_mismatch.as_str(),
        non_advancing.as_str(),
        inconsistent_limit.as_str(),
        duplicate_offset.as_str(),
        duplicate_limit.as_str(),
        total_boundary.as_str(),
        requested_limit_bypass.as_str(),
        item_count_mismatch.as_str(),
    ] {
        assert!(matches!(
            parse_review_page(ReviewKind::Critic, "example-game", 0, 20, body),
            Err(SourceError::InvalidContinuation)
        ));
    }
    let continuation_error =
        parse_review_page(ReviewKind::Critic, "example-game", 0, 20, &non_advancing)
            .expect_err("non-progressing continuation must fail before review input mapping");
    assert_eq!(
        classify_review_page_source_error(&continuation_error),
        SourceIngestionFailureCategory::ReviewContinuationLink
    );

    let user_path = M015_CRITIC_SERVER_CLAMP.replace("/critic/", "/user/");
    assert!(matches!(
        parse_review_page(ReviewKind::User, "example-game", 0, 20, &user_path),
        Err(SourceError::InvalidContinuation)
    ));
    assert!(matches!(
        parse_review_page(
            ReviewKind::Critic,
            "example-game",
            u32::MAX,
            20,
            M015_CRITIC_SERVER_CLAMP,
        ),
        Err(SourceError::InvalidContinuation)
    ));
}

#[test]
fn accepts_only_exhausted_review_placeholders_as_terminal() {
    let game = example_game();
    let critic = parse_review_page(
        ReviewKind::Critic,
        &game.slug,
        0,
        20,
        M017_REVIEW_TERMINAL_WITH_ITEMS,
    )
    .expect("exhausted critic placeholder must be terminal");
    let user = parse_review_page(
        ReviewKind::User,
        &game.slug,
        0,
        20,
        M017_REVIEW_TERMINAL_EMPTY,
    )
    .expect("exhausted user placeholder must be terminal");

    assert_eq!(critic.reviews.len(), 4);
    assert_eq!(critic.total_results, 4);
    assert_eq!(critic.next, None);
    assert!(user.reviews.is_empty());
    assert_eq!(user.total_results, 0);
    assert_eq!(user.next, None);

    let missing_next = without_next(M017_REVIEW_TERMINAL_EMPTY);
    assert_eq!(
        parse_review_page(ReviewKind::User, &game.slug, 0, 20, &missing_next)
            .expect("missing next must remain terminal")
            .next,
        None
    );
    assert_eq!(
        parse_review_page(
            ReviewKind::Critic,
            &game.slug,
            0,
            3,
            &without_next(CRITIC_REVIEWS),
        )
        .expect("missing next must preserve the existing terminal behavior")
        .next,
        None
    );

    for body in [
        with_explicit_null_next(M017_REVIEW_TERMINAL_WITH_ITEMS),
        with_explicit_null_href(M017_REVIEW_TERMINAL_WITH_ITEMS),
        with_explicit_null_next(M017_REVIEW_TERMINAL_EMPTY),
        with_explicit_null_href(M017_REVIEW_TERMINAL_EMPTY),
    ] {
        assert!(matches!(
            parse_review_page(ReviewKind::Critic, &game.slug, 0, 20, &body),
            Err(SourceError::InvalidContinuation)
        ));
    }

    let non_exhausted =
        M017_REVIEW_TERMINAL_WITH_ITEMS.replace("\"totalResults\": 4", "\"totalResults\": 5");
    assert!(matches!(
        parse_review_page(ReviewKind::Critic, &game.slug, 0, 20, &non_exhausted),
        Err(SourceError::InvalidContinuation)
    ));

    let exhausted_listing = LISTING.replace("\"totalResults\": 42", "\"totalResults\": 2");
    for body in [
        with_explicit_null_next(&exhausted_listing),
        with_explicit_null_href(&exhausted_listing),
    ] {
        assert!(matches!(
            parse_listing_page(ListMode::NewReleases, 0, 20, &body),
            Err(SourceError::InvalidContinuation)
        ));
    }

    let listing_placeholder = LISTING.replace("\"href\":", "\"placeholder\":");
    assert!(matches!(
        parse_listing_page(ListMode::NewReleases, 0, 20, &listing_placeholder),
        Err(SourceError::InvalidContinuation)
    ));
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
