#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::to_bytes;
use axum::http::StatusCode;
use axum::response::Response;
use gamepulse_application::{
    GameCoverDescriptor, GameDeveloper, GamePlatformScore, GamePublicCoverUrl, GameSnapshot,
    GameVideoLink, Metascore, SourceProductId, Userscore, upsert_game_snapshot,
};
use gamepulse_storage_sqlite::{SqliteGameCatalogueReadStore, SqliteGameSnapshotStore};

static NEXT_TEMPORARY_DATABASE: AtomicU64 = AtomicU64::new(0);

struct TemporaryDatabase {
    path: PathBuf,
}

impl TemporaryDatabase {
    fn new() -> Self {
        let sequence = NEXT_TEMPORARY_DATABASE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "gamepulse-m010-catalogue-http-{}-{sequence}.sqlite3",
            process::id()
        ));
        let _ = fs::remove_file(&path);
        Self { path }
    }
}

impl Drop for TemporaryDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(self.path.with_extension("sqlite3-shm"));
        let _ = fs::remove_file(self.path.with_extension("sqlite3-wal"));
    }
}

fn snapshot(
    source_product_id: u64,
    title: &str,
    description: &str,
    platforms: &[(u64, &str, Option<u8>, Option<f64>)],
    developers: &[&str],
    with_cover_and_video: bool,
) -> GameSnapshot {
    GameSnapshot::new(
        SourceProductId::new(source_product_id).expect("test source identity must be valid"),
        format!("game-{source_product_id}"),
        title,
        description,
        with_cover_and_video.then(|| {
            GameCoverDescriptor::new("products/example", "image", "cover.jpg", "cardImage")
                .expect("test cover descriptor must be valid")
        }),
        with_cover_and_video.then(|| {
            GameVideoLink::new("https://video.example.test/embed")
                .expect("test video must be valid")
        }),
        platforms
            .iter()
            .map(|(platform_id, slug, metascore, userscore)| {
                GamePlatformScore::new(
                    *platform_id,
                    *slug,
                    metascore
                        .map(|value| Metascore::new(value).expect("test Metascore must be valid")),
                    userscore
                        .map(|value| Userscore::new(value).expect("test Userscore must be valid")),
                )
                .expect("test platform must be valid")
            })
            .collect(),
        developers
            .iter()
            .map(|developer| GameDeveloper::new(*developer).expect("test developer must be valid"))
            .collect(),
    )
    .expect("test snapshot must be valid")
    .with_public_cover_url(with_cover_and_video.then(|| {
        GamePublicCoverUrl::new("https://www.metacritic.com/images/example-game.jpg")
            .expect("test public cover URL must be valid")
    }))
}

fn fixture_catalogue(database: &TemporaryDatabase) -> Arc<Mutex<SqliteGameCatalogueReadStore>> {
    let mut snapshots =
        SqliteGameSnapshotStore::open(&database.path).expect("snapshot store must open");
    for game in [
        snapshot(
            101,
            "Alpha",
            "Alpha <untrusted> description",
            &[(7, "pc", Some(80), Some(8.2))],
            &["Studio A"],
            true,
        ),
        snapshot(
            102,
            "Beta",
            "Stored Beta description",
            &[
                (7, "pc", Some(90), Some(7.2)),
                (8, "console", Some(70), None),
            ],
            &["Studio B"],
            false,
        ),
        snapshot(
            103,
            "Gamma",
            "Stored Gamma description",
            &[(8, "console", Some(95), Some(9.0))],
            &["Studio A"],
            false,
        ),
        snapshot(
            104,
            "Delta",
            "Stored Delta description",
            &[(7, "pc", Some(85), None)],
            &["Studio A"],
            false,
        ),
        snapshot(
            105,
            "Echo",
            "Stored Echo description",
            &[(9, "handheld", None, None)],
            &["Studio C"],
            false,
        ),
        snapshot(
            106,
            "Foxtrot",
            "Stored Foxtrot description",
            &[(7, "pc", Some(40), Some(5.0))],
            &["Studio D"],
            false,
        ),
    ] {
        upsert_game_snapshot(&mut snapshots, &game).expect("fixture snapshot must persist");
    }
    drop(snapshots);

    let catalogue =
        SqliteGameCatalogueReadStore::open(&database.path).expect("catalogue must open");
    Arc::new(Mutex::new(catalogue))
}

async fn read_response(response: Response) -> (StatusCode, String) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body must collect");
    (
        status,
        String::from_utf8(bytes.to_vec()).expect("response body must be UTF-8"),
    )
}

fn assert_in_order(body: &str, fragments: &[&str]) {
    let mut previous = 0;
    for fragment in fragments {
        let found = body[previous..]
            .find(fragment)
            .map(|offset| previous + offset)
            .unwrap_or_else(|| panic!("missing fragment {fragment:?} in response"));
        previous = found + fragment.len();
    }
}

#[tokio::test]
async fn renders_a_deterministic_offline_catalogue_from_accepted_snapshots() {
    let database = TemporaryDatabase::new();
    let catalogue = fixture_catalogue(&database);
    let _router = gamepulse_web::catalogue_router(Arc::clone(&catalogue));

    let (status, all_games) =
        read_response(gamepulse_web::catalogue_response(Arc::clone(&catalogue), None).await).await;
    assert_eq!(status, StatusCode::OK);
    assert_in_order(
        &all_games,
        &[
            "href=\"/games/103\">Gamma",
            "href=\"/games/102\">Beta",
            "href=\"/games/104\">Delta",
            "href=\"/games/101\">Alpha",
            "href=\"/games/106\">Foxtrot",
            "href=\"/games/105\">Echo",
        ],
    );
    assert!(all_games.contains("Metascore: Not stored"));
    assert!(all_games.contains("src=\"https://www.metacritic.com/images/example-game.jpg\""));
    assert!(all_games.contains("<p>Cover unavailable.</p>"));

    let (status, search) = read_response(
        gamepulse_web::catalogue_response(Arc::clone(&catalogue), Some("q=aLpHa")).await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(search.contains("href=\"/games/101\">Alpha"));
    assert!(!search.contains("href=\"/games/102\">Beta"));

    let (status, platform) = read_response(
        gamepulse_web::catalogue_response(Arc::clone(&catalogue), Some("platform=PC")).await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_in_order(
        &platform,
        &[
            "href=\"/games/102\">Beta",
            "href=\"/games/104\">Delta",
            "href=\"/games/101\">Alpha",
            "href=\"/games/106\">Foxtrot",
        ],
    );
    assert!(!platform.contains("href=\"/games/103\">Gamma"));

    let (status, detail) =
        read_response(gamepulse_web::game_detail_response(Arc::clone(&catalogue), 101).await).await;
    assert_eq!(status, StatusCode::OK);
    assert!(detail.contains("Alpha &#60;untrusted&#62; description"));
    assert!(detail.contains("src=\"https://www.metacritic.com/images/example-game.jpg\""));
    assert!(detail.contains("onerror=\"this.onerror=null;this.replaceWith(document.createTextNode('Cover unavailable.'))\""));
    assert!(detail.contains("products/example"));
    assert!(detail.contains("href=\"https://video.example.test/embed\""));
    assert_in_order(
        &detail,
        &[
            "href=\"/games/104\">Delta",
            "href=\"/games/102\">Beta",
            "href=\"/games/106\">Foxtrot",
            "href=\"/games/103\">Gamma",
        ],
    );
    assert!(!detail.contains("Unseeded game"));

    let (status, linked_detail) =
        read_response(gamepulse_web::game_detail_response(Arc::clone(&catalogue), 104).await).await;
    assert_eq!(status, StatusCode::OK);
    assert!(linked_detail.contains("<h1>Delta</h1>"));
    assert!(linked_detail.contains("<p>Cover unavailable.</p>"));

    let (status, empty) = read_response(
        gamepulse_web::catalogue_response(Arc::clone(&catalogue), Some("q=missing")).await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(empty.contains("No stored games match this catalogue query."));

    let (status, not_found) =
        read_response(gamepulse_web::game_detail_response(catalogue, 999).await).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(not_found.is_empty());
}
