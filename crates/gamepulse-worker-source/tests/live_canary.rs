#![forbid(unsafe_code)]

use gamepulse_worker_source::MetacriticCanaryClient;

#[tokio::test]
#[ignore = "performs exactly one anonymous public Metacritic request"]
async fn live_new_releases_contract_canary() {
    assert_eq!(
        std::env::var("METACRITIC_LIVE_CANARY").as_deref(),
        Ok("1"),
        "set METACRITIC_LIVE_CANARY=1 to opt in"
    );

    let client = MetacriticCanaryClient::new().expect("client configuration must be valid");
    let page = client
        .fetch_new_releases()
        .await
        .expect("public New Releases contract must remain readable");

    eprintln!(
        "metacritic live canary: mode={:?} items={} total_results={} has_next={}",
        page.mode,
        page.games.len(),
        page.total_results,
        page.next.is_some()
    );
    assert!(
        !page.games.is_empty(),
        "New Releases must expose at least one game"
    );
}
