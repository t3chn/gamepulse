#![forbid(unsafe_code)]

use gamepulse_application::{CoverImageContentType, GameCoverDescriptor};
use gamepulse_worker_source::{decode_local_cover_image, resolve_local_cover_source_url};

const ONE_PIXEL_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 8, 29, 99, 248, 207, 192, 240, 31, 0, 5,
    128, 2, 63, 73, 194, 253, 97, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

#[test]
fn resolves_only_the_observed_descriptor_and_accepts_a_matching_fixture_asset() {
    let descriptor = GameCoverDescriptor::new(
        "/provider/7/2/7-example.png",
        "catalog",
        "7-example.png",
        "cardImage",
    )
    .expect("fixture descriptor must be valid");

    assert_eq!(
        resolve_local_cover_source_url(&descriptor).map(|url| url.as_str().to_owned()),
        Some("https://www.metacritic.com/a/img/catalog/provider/7/2/7-example.png".to_owned())
    );
    let cover = decode_local_cover_image(CoverImageContentType::Png, ONE_PIXEL_PNG.to_vec())
        .expect("matching local fixture image must be accepted");
    assert_eq!(cover.content_type(), CoverImageContentType::Png);
    assert_eq!(cover.bytes(), ONE_PIXEL_PNG);
}

#[test]
fn rejects_an_unsafe_descriptor_or_mismatched_content_type() {
    let descriptor = GameCoverDescriptor::new(
        "/provider/7/../7-example.png",
        "catalog",
        "7-example.png",
        "cardImage",
    )
    .expect("fixture descriptor must be structurally valid");
    assert!(resolve_local_cover_source_url(&descriptor).is_none());
    assert!(
        decode_local_cover_image(CoverImageContentType::Jpeg, ONE_PIXEL_PNG.to_vec()).is_none()
    );
}
