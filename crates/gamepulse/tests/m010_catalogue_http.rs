#![forbid(unsafe_code)]

use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{self, Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use axum::body::to_bytes;
use axum::http::StatusCode;
use axum::response::Response;
use gamepulse_application::{
    GameCoverDescriptor, GameDeveloper, GamePlatformScore, GamePublicCoverUrl, GameSnapshot,
    GameVideoLink, Metascore, SourceProductId, Userscore, upsert_game_snapshot,
};
use gamepulse_storage_sqlite::{SqliteGameCatalogueReadStore, SqliteGameSnapshotStore};

static NEXT_TEMPORARY_DATABASE: AtomicU64 = AtomicU64::new(0);
static NEXT_BROWSER_SMOKE_DIRECTORY: AtomicU64 = AtomicU64::new(0);

const BROWSER_SMOKE_ATTEMPTS: usize = 40;
const BROWSER_SMOKE_DELAY: Duration = Duration::from_millis(100);
const BROWSER_INSPECTION_WINDOW: Duration = Duration::from_secs(40);

struct TemporaryDatabase {
    path: PathBuf,
    remove_on_drop: bool,
}

impl TemporaryDatabase {
    fn new() -> Self {
        let sequence = NEXT_TEMPORARY_DATABASE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "gamepulse-m010-catalogue-http-{}-{sequence}.sqlite3",
            process::id()
        ));
        let _ = fs::remove_file(&path);
        Self {
            path,
            remove_on_drop: true,
        }
    }

    fn retained(path: PathBuf) -> Self {
        assert!(path.is_absolute(), "fixture database path must be absolute");
        assert!(
            !path.exists(),
            "fixture database path must not already exist"
        );
        Self {
            path,
            remove_on_drop: false,
        }
    }
}

impl Drop for TemporaryDatabase {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = fs::remove_file(&self.path);
            let _ = fs::remove_file(self.path.with_extension("sqlite3-shm"));
            let _ = fs::remove_file(self.path.with_extension("sqlite3-wal"));
        }
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
            &[(9, "handheld", None, Some(0.0))],
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

#[test]
#[ignore = "manual source-disabled production UI smoke fixture setup"]
fn seeds_deterministic_visual_fixture_at_requested_path() {
    let path = std::env::var_os("GAMEPULSE_M019_FIXTURE_PATH")
        .map(PathBuf::from)
        .expect("GAMEPULSE_M019_FIXTURE_PATH must be set");
    let database = TemporaryDatabase::retained(path);
    let catalogue = fixture_catalogue(&database);
    drop(catalogue);
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

struct BrowserSmokeDirectory {
    path: PathBuf,
}

impl BrowserSmokeDirectory {
    fn new() -> Self {
        let sequence = NEXT_BROWSER_SMOKE_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "gamepulse-m019-browser-{}-{sequence}",
            process::id()
        ));
        fs::create_dir(&path).expect("browser smoke directory must be created exactly once");
        Self { path }
    }

    fn database_path(&self) -> PathBuf {
        self.path.join("gamepulse.sqlite3")
    }

    fn log_path(&self) -> PathBuf {
        self.path.join("gamepulse.log")
    }
}

impl Drop for BrowserSmokeDirectory {
    fn drop(&mut self) {
        let database = self.database_path();
        let _ = fs::remove_file(&database);
        let _ = fs::remove_file(database.with_extension("sqlite3-shm"));
        let _ = fs::remove_file(database.with_extension("sqlite3-wal"));
        let _ = fs::remove_file(self.log_path());
        let _ = fs::remove_dir(&self.path);
    }
}

struct BrowserSmokeProcess {
    child: Child,
}

impl BrowserSmokeProcess {
    fn shutdown(&mut self) {
        let signal = Command::new("/bin/kill")
            .arg("-INT")
            .arg(self.child.id().to_string())
            .status()
            .expect("browser smoke SIGINT helper must start");
        assert!(signal.success(), "browser smoke SIGINT helper must succeed");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    assert!(status.success(), "browser smoke binary must stop cleanly");
                    return;
                }
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Ok(None) => panic!("browser smoke binary did not stop within five seconds"),
                Err(error) => panic!("browser smoke binary status check failed: {error}"),
            }
        }
    }
}

impl Drop for BrowserSmokeProcess {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn reserve_loopback_port() -> u16 {
    let listener =
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("browser smoke must reserve a port");
    let port = listener
        .local_addr()
        .expect("reserved browser smoke listener must have an address")
        .port();
    drop(listener);
    port
}

fn browser_smoke_http_status(port: u16, target: &str) -> std::io::Result<u16> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, BROWSER_SMOKE_DELAY)?;
    stream.set_read_timeout(Some(BROWSER_SMOKE_DELAY))?;
    stream.write_all(
        format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    response
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse::<u16>().ok())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing HTTP status"))
}

#[test]
#[ignore = "manual browser visual inspection of the source-disabled release binary"]
fn source_disabled_release_fixture_stays_available_for_bounded_browser_inspection() {
    let temporary = BrowserSmokeDirectory::new();
    let database = TemporaryDatabase::retained(temporary.database_path());
    let catalogue = fixture_catalogue(&database);
    drop(catalogue);
    drop(database);

    let port = reserve_loopback_port();
    let manifest_directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_directory
        .parent()
        .and_then(|path| path.parent())
        .expect("gamepulse manifest must remain two levels below workspace root")
        .to_path_buf();
    let release_binary = workspace_root.join("target/release/gamepulse");
    assert!(
        release_binary.is_file(),
        "build the release binary before browser inspection"
    );
    let log = fs::File::create(temporary.log_path()).expect("browser smoke log must be created");
    let stdout = log
        .try_clone()
        .expect("browser smoke log must be cloneable for stdout");
    let mut process = BrowserSmokeProcess {
        child: Command::new(release_binary)
            .env("GAMEPULSE_DATABASE_PATH", temporary.database_path())
            .env("GAMEPULSE_HTTP_ADDRESS", format!("127.0.0.1:{port}"))
            .env("GAMEPULSE_LOG_FORMAT", "human")
            .env("GAMEPULSE_SOURCE_WORK_ENABLED", "false")
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("source-disabled release binary must start"),
    };
    for attempt in 0..BROWSER_SMOKE_ATTEMPTS {
        if matches!(browser_smoke_http_status(port, "/health/ready"), Ok(200)) {
            break;
        }
        if attempt + 1 == BROWSER_SMOKE_ATTEMPTS {
            panic!("source-disabled release binary did not become ready");
        }
        thread::sleep(BROWSER_SMOKE_DELAY);
    }
    assert_eq!(
        browser_smoke_http_status(port, "/health/live")
            .expect("browser smoke liveness request must return a status"),
        200
    );
    assert_eq!(
        browser_smoke_http_status(port, "/games")
            .expect("browser smoke catalogue request must return a status"),
        200
    );
    assert_eq!(
        browser_smoke_http_status(port, "/games/101")
            .expect("browser smoke detail request must return a status"),
        200
    );
    println!("M019_BROWSER_READY http://127.0.0.1:{port}/games");
    thread::sleep(BROWSER_INSPECTION_WINDOW);
    process.shutdown();
    let log = fs::read_to_string(temporary.log_path()).expect("browser smoke log must be readable");
    assert!(log.contains("source work disabled"));
    assert!(log.contains("process started"));
    assert!(log.contains("process stopped"));
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
    assert!(
        all_games
            .contains("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">")
    );
    assert!(all_games.contains("<main id=\"main-content\" class=\"page-shell\">"));
    assert!(all_games.contains(
        "<form class=\"catalogue-controls\" action=\"/games\" method=\"get\" role=\"search\">"
    ));
    assert!(all_games.contains("<ol class=\"game-grid\" aria-label=\"Stored games\">"));
    assert!(all_games.contains(
        "<img class=\"cover-image\" src=\"https://www.metacritic.com/images/example-game.jpg\" alt=\"Cover for Alpha\">"
    ));
    assert!(all_games.contains("class=\"cover-placeholder\" aria-hidden=\"true\""));
    assert!(!all_games.contains("No local cover image stored"));
    assert!(all_games.contains("class=\"score-badge__label\">Best score"));
    assert!(
        all_games
            .contains("background: var(--primary-strong); color: var(--canvas); font-weight: 800;")
    );

    let (status, search) = read_response(
        gamepulse_web::catalogue_response(Arc::clone(&catalogue), Some("q=aLpHa")).await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(search.contains("href=\"/games/101\">Alpha"));
    assert!(!search.contains("href=\"/games/102\">Beta"));
    assert!(search.contains("value=\"aLpHa\""));

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
    assert!(platform.contains("class=\"score-badge__label\">PC"));
    assert!(platform.contains("Sorted by the stored PC Metascore"));

    let (status, detail) =
        read_response(gamepulse_web::game_detail_response(Arc::clone(&catalogue), 101).await).await;
    assert_eq!(status, StatusCode::OK);
    assert!(detail.contains("Alpha &#60;untrusted&#62; description"));
    assert!(detail.contains("<nav class=\"breadcrumb\" aria-label=\"Breadcrumb\">"));
    assert!(detail.contains(
        "<img class=\"cover-image cover-image--large\" src=\"https://www.metacritic.com/images/example-game.jpg\" alt=\"Cover for Alpha\">"
    ));
    assert!(detail.contains("<caption>Stored score comparison by platform</caption>"));
    assert!(detail.contains("<th scope=\"col\">Userscore</th>"));
    assert!(detail.contains("<td>PC</td>"));
    assert!(detail.contains("<details class=\"provenance\">"));
    assert!(detail.contains("products/example"));
    assert!(detail.contains(
        "href=\"https://video.example.test/embed\" rel=\"noopener noreferrer\" target=\"_blank\""
    ));
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

    let (status, zero_score_detail) =
        read_response(gamepulse_web::game_detail_response(Arc::clone(&catalogue), 105).await).await;
    assert_eq!(status, StatusCode::OK);
    assert!(zero_score_detail.contains("<td>handheld</td>"));
    assert!(!zero_score_detail.contains("<span class=\"score-value\">0</span>"));

    let (status, linked_detail) =
        read_response(gamepulse_web::game_detail_response(Arc::clone(&catalogue), 104).await).await;
    assert_eq!(status, StatusCode::OK);
    assert!(linked_detail.contains("<h1>Delta</h1>"));
    assert!(linked_detail.contains("No local cover image stored"));

    let (status, empty) = read_response(
        gamepulse_web::catalogue_response(Arc::clone(&catalogue), Some("q=missing")).await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(empty.contains("No stored games match this catalogue query."));
    assert!(empty.contains(
        "<section class=\"empty-state\" aria-labelledby=\"empty-title\" role=\"status\">"
    ));
    assert!(empty.contains("href=\"/games\">Clear catalogue filters</a>"));

    let (status, not_found) =
        read_response(gamepulse_web::game_detail_response(catalogue, 999).await).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(not_found.contains("<main id=\"main-content\" class=\"page-shell\">"));
    assert!(
        not_found.contains(
            "<section class=\"empty-state not-found\" aria-labelledby=\"not-found-title\">"
        )
    );
    assert!(not_found.contains("This game is not in the stored catalogue."));
}
