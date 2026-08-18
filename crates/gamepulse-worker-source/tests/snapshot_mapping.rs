#![forbid(unsafe_code)]

use gamepulse_worker_source::{
    GameId, GameIdentity, SnapshotUserScoreBindingError, SourceError, map_game_detail_to_snapshot,
    parse_game_detail, parse_platform_user_score_for_snapshot,
};

const DETAIL: &str = include_str!("fixtures/product-detail.json");
const USER_SCORE: &str = include_str!("fixtures/user-score.json");

fn example_game() -> GameIdentity {
    GameIdentity {
        id: GameId(101),
        slug: "example-game".to_owned(),
    }
}

#[test]
fn local_detail_and_user_score_fixtures_map_to_one_validated_snapshot() {
    let detail = parse_game_detail(&example_game(), DETAIL).expect("detail fixture must parse");
    let pc = detail
        .platforms
        .iter()
        .find(|platform| platform.slug == "pc")
        .expect("detail fixture must include PC");
    let user_score = parse_platform_user_score_for_snapshot(&example_game(), pc, USER_SCORE)
        .expect("score fixture must parse and bind to the detail request");

    let snapshot = map_game_detail_to_snapshot(&detail, [user_score])
        .expect("fixtures must map without a source request");

    assert_eq!(snapshot.source_product_id().value(), 101);
    assert_eq!(snapshot.source_slug(), "example-game");
    assert_eq!(snapshot.title(), "Example Game");
    assert_eq!(snapshot.description(), "Synthetic detail fixture.");
    let cover = snapshot
        .cover()
        .expect("fixture cover must be retained as descriptors");
    assert_eq!(cover.bucket_path(), "products/example-game");
    assert_eq!(cover.bucket_type(), "image");
    assert_eq!(cover.filename(), "cover.jpg");
    assert_eq!(cover.kind(), "cardImage");
    assert_eq!(
        snapshot
            .video()
            .expect("fixture video must be retained")
            .as_str(),
        "https://cdn.example.test/player/example-game.html"
    );
    assert_eq!(snapshot.platform_scores().len(), 2);
    assert_eq!(snapshot.platform_scores()[0].source_platform_id(), 7);
    assert_eq!(
        snapshot.platform_scores()[0]
            .metascore()
            .expect("score")
            .value(),
        82
    );
    assert_eq!(
        snapshot.platform_scores()[0]
            .userscore()
            .expect("score")
            .value(),
        8.4
    );
    assert_eq!(snapshot.platform_scores()[1].source_platform_id(), 8);
    assert_eq!(snapshot.platform_scores()[1].metascore(), None);
    assert_eq!(snapshot.platform_scores()[1].userscore(), None);
    assert_eq!(snapshot.developers().len(), 1);
    assert_eq!(snapshot.developers()[0].as_str(), "Example Studio");
}

#[test]
fn rejects_a_userscore_fixture_with_a_foreign_game_self_link() {
    let detail = parse_game_detail(&example_game(), DETAIL).expect("detail fixture must parse");
    let pc = detail
        .platforms
        .iter()
        .find(|platform| platform.slug == "pc")
        .expect("detail fixture must include PC");
    let foreign_game_body = USER_SCORE.replace(
        "/games/example-game/platform/pc/",
        "/games/other-game/platform/pc/",
    );

    let result = parse_platform_user_score_for_snapshot(&example_game(), pc, &foreign_game_body);

    assert!(matches!(
        result,
        Err(SnapshotUserScoreBindingError::Source(
            SourceError::MismatchedSelfLink {
                field: "user-score.links.self.href"
            }
        ))
    ));
}

#[test]
fn rejects_a_userscore_fixture_with_a_foreign_platform_self_link() {
    let detail = parse_game_detail(&example_game(), DETAIL).expect("detail fixture must parse");
    let pc = detail
        .platforms
        .iter()
        .find(|platform| platform.slug == "pc")
        .expect("detail fixture must include PC");
    let foreign_platform_body = USER_SCORE.replace(
        "/games/example-game/platform/pc/",
        "/games/example-game/platform/console/",
    );

    let result =
        parse_platform_user_score_for_snapshot(&example_game(), pc, &foreign_platform_body);

    assert!(matches!(
        result,
        Err(SnapshotUserScoreBindingError::Source(
            SourceError::MismatchedSelfLink {
                field: "user-score.links.self.href"
            }
        ))
    ));
}

#[test]
fn missing_source_fields_stay_explicitly_absent_without_an_eligibility_policy() {
    let mut detail = parse_game_detail(&example_game(), DETAIL).expect("detail fixture must parse");
    detail.images.clear();
    detail.video = None;
    detail.developers.clear();

    let snapshot = map_game_detail_to_snapshot(&detail, [])
        .expect("missing optional source fields must still map");

    assert!(snapshot.cover().is_none());
    assert!(snapshot.video().is_none());
    assert!(snapshot.developers().is_empty());
    assert!(
        snapshot
            .platform_scores()
            .iter()
            .all(|platform| platform.userscore().is_none())
    );
}

#[test]
fn derives_only_the_observed_first_party_catalog_cover_shape() {
    let mut detail = parse_game_detail(&example_game(), DETAIL).expect("detail fixture must parse");
    assert!(!detail.images.is_empty(), "fixture image must exist");
    detail.images[0].bucket_type = "catalog".to_owned();
    detail.images[0].bucket_path = "/provider/7/2/7-example.jpg".to_owned();
    detail.images[0].filename = "7-example.jpg".to_owned();

    let snapshot = map_game_detail_to_snapshot(&detail, []).expect("observed shape must map");
    assert_eq!(
        snapshot.public_cover_url().map(|url| url.as_str()),
        Some("https://www.metacritic.com/a/img/catalog/provider/7/2/7-example.jpg")
    );

    for invalid_path in [
        "/provider/7/2/other.jpg",
        "/provider/7/../7-example.jpg",
        "/provider/7/2/7-example.jpg?size=large",
    ] {
        detail.images[0].bucket_path = invalid_path.to_owned();
        let snapshot = map_game_detail_to_snapshot(&detail, []).expect("descriptor must still map");
        assert!(snapshot.public_cover_url().is_none());
    }
}
