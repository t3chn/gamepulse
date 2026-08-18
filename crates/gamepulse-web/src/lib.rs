#![forbid(unsafe_code)]

//! Server-rendered HTTP and embedded UI adapter.

use std::sync::{Arc, Mutex};

use askama::Template;
use axum::Router;
use axum::extract::{Path, RawQuery, State};
use axum::http::{StatusCode, header::CONTENT_TYPE};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use gamepulse_application::{
    CatalogueGameDetail, CataloguePage, CatalogueQuery, CatalogueReviewSummary,
    GameCatalogueReadPort, ServiceReadinessPort, SourceProductId, load_catalogue,
    load_catalogue_cover, load_catalogue_game,
};

// This stylesheet is compiled into the single binary with the Askama templates. It deliberately
// contains no remote assets, font imports, or client-side runtime.
const UI_CSS: &str = r#"
:root {
  color-scheme: dark;
  --canvas: oklch(0.1 0 0);
  --surface: oklch(0.15 0.015 140);
  --surface-raised: oklch(0.2 0.018 140);
  --line: oklch(0.34 0.03 140);
  --line-strong: oklch(0.52 0.06 140);
  --ink: oklch(0.96 0.01 140);
  --muted: oklch(0.74 0.025 140);
  --primary: oklch(0.72 0.11 140);
  --primary-strong: oklch(0.62 0.12 140);
  --accent: oklch(0.73 0.12 72);
  --focus: oklch(0.88 0.14 104);
  --danger: oklch(0.75 0.12 28);
  --max-width: 76rem;
  font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}

* { box-sizing: border-box; }
html { background: var(--canvas); color: var(--ink); scroll-behavior: smooth; }
body { margin: 0; min-width: 20rem; background: var(--canvas); color: var(--ink); }
a { color: inherit; text-decoration-color: var(--primary); text-decoration-thickness: 0.12em; text-underline-offset: 0.18em; }
a:hover { color: var(--primary); }
a:focus-visible, button:focus-visible, input:focus-visible, select:focus-visible, summary:focus-visible {
  outline: 0.2rem solid var(--focus);
  outline-offset: 0.22rem;
}
button, input, select { font: inherit; }
button { cursor: pointer; }

.skip-link {
  position: fixed;
  left: 1rem;
  top: -5rem;
  z-index: 2;
  padding: 0.7rem 1rem;
  border-radius: 0.5rem;
  background: var(--ink);
  color: var(--canvas);
  font-weight: 700;
}
.skip-link:focus { top: 1rem; }
.site-header { border-bottom: 1px solid var(--line); background: var(--surface); }
.site-header__inner, .page-shell { width: min(calc(100% - 2rem), var(--max-width)); margin-inline: auto; }
.site-header__inner { display: flex; align-items: center; justify-content: space-between; gap: 1rem; min-height: 4.5rem; }
.brand { color: var(--ink); font-size: 1.08rem; font-weight: 800; letter-spacing: -0.02em; text-decoration: none; }
.brand__mark { color: var(--primary); }
.header-note { margin: 0; color: var(--muted); font-size: 0.88rem; text-align: right; }
.page-shell { padding-block: clamp(2rem, 5vw, 4.5rem); }
.page-intro { max-width: 48rem; margin-bottom: 2rem; }
.section-label { margin: 0 0 0.65rem; color: var(--primary); font-size: 0.78rem; font-weight: 800; letter-spacing: 0.08em; text-transform: uppercase; }
h1, h2, h3, p { overflow-wrap: anywhere; }
h1, h2, h3 { text-wrap: balance; }
h1 { max-width: 14ch; margin: 0; font-size: clamp(2.25rem, 6vw, 4.75rem); line-height: 0.98; letter-spacing: -0.035em; }
h2 { margin: 0; font-size: 1.45rem; letter-spacing: -0.02em; }
h3 { margin: 0; font-size: 1.05rem; letter-spacing: -0.012em; }
.lede { max-width: 65ch; margin: 1rem 0 0; color: var(--muted); font-size: 1.08rem; line-height: 1.65; text-wrap: pretty; }
.filter-panel { margin-bottom: 2.5rem; padding: 1rem; border: 1px solid var(--line); border-radius: 1rem; background: var(--surface); }
.catalogue-controls { display: grid; grid-template-columns: minmax(14rem, 1.5fr) minmax(12rem, 1fr) auto; gap: 0.9rem; align-items: end; }
.field { display: grid; gap: 0.45rem; }
.field label { color: var(--muted); font-size: 0.86rem; font-weight: 700; }
.field input, .field select {
  width: 100%; min-height: 2.8rem; padding: 0.65rem 0.75rem; border: 1px solid var(--line-strong); border-radius: 0.55rem;
  background: var(--canvas); color: var(--ink);
}
.field input::placeholder { color: var(--muted); opacity: 1; }
.primary-button { min-height: 2.8rem; padding: 0.65rem 1rem; border: 1px solid var(--primary); border-radius: 0.55rem; background: var(--primary-strong); color: var(--canvas); font-weight: 800; }
.primary-button:hover { border-color: var(--primary); background: var(--primary); color: var(--canvas); }
.results-heading { display: flex; align-items: end; justify-content: space-between; gap: 1rem; margin-bottom: 1.2rem; }
.results-heading p { margin: 0; color: var(--muted); font-size: 0.9rem; text-align: right; }
.game-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(17rem, 1fr)); gap: 1rem; margin: 0; padding: 0; list-style: none; align-items: stretch; }
.game-grid > li { display: flex; min-width: 0; }
.game-card { display: flex; width: 100%; height: 100%; padding: 1rem; border: 1px solid var(--line); border-radius: 1rem; background: var(--surface); flex-direction: column; }
.game-card:hover, .game-card:focus-within, .similar-card:hover, .similar-card:focus-within { border-color: var(--line-strong); }
.game-card__top { display: grid; grid-template-columns: 4.25rem minmax(0, 1fr); gap: 0.6rem 0.85rem; align-content: start; }
.game-card__top > .cover-image, .game-card__top > .cover-placeholder { grid-column: 1; grid-row: 1 / span 2; align-self: start; }
.game-card__top > div:not(.cover-placeholder):not(.score-badge) { grid-column: 2; grid-row: 1; min-width: 0; min-height: 4.4rem; }
.game-card__top > .score-badge { grid-column: 2; grid-row: 2; justify-self: start; }
.game-card h3 { overflow-wrap: break-word; hyphens: auto; text-wrap: pretty; }
.cover-placeholder { display: grid; width: 100%; min-height: 5.25rem; place-items: center; border: 1px solid var(--line-strong); border-radius: 0.7rem; background: var(--surface-raised); color: var(--primary); font-size: 1.25rem; font-weight: 850; letter-spacing: -0.07em; }
.cover-placeholder--large { width: 9.5rem; min-height: 12rem; font-size: 2rem; }
.cover-image { display: block; width: 100%; min-height: 5.25rem; max-height: 12rem; border: 1px solid var(--line-strong); border-radius: 0.7rem; background: var(--surface-raised); object-fit: cover; }
.cover-image--large { width: 9.5rem; min-height: 12rem; }
.cover-status { margin: 0.65rem 0 0; color: var(--muted); font-size: 0.78rem; line-height: 1.4; }
.game-title { color: var(--ink); text-decoration-color: transparent; }
.game-title:hover, .game-title:focus-visible { color: var(--primary); text-decoration-color: currentColor; }
.score-badge { display: grid; min-width: 3.25rem; gap: 0.1rem; padding: 0.42rem 0.48rem; border: 1px solid var(--primary); border-radius: 0.6rem; background: oklch(0.22 0.035 140); text-align: center; }
.score-badge { max-width: 7.5rem; overflow-wrap: anywhere; }
.score-badge__label { color: var(--muted); font-size: 0.65rem; font-weight: 800; letter-spacing: 0.05em; text-transform: uppercase; }
.score-badge strong { color: var(--primary); font-size: 1.25rem; line-height: 1; }
.score-badge--empty { border-color: var(--line-strong); background: transparent; }
.score-badge--empty strong { color: var(--muted); font-size: 0.82rem; line-height: 1.25; }
.chip-list { display: flex; flex-wrap: wrap; gap: 0.42rem; margin: 1.1rem 0 0; padding: 0; list-style: none; }
.chip { padding: 0.28rem 0.5rem; border: 1px solid var(--line); border-radius: 999px; color: var(--muted); font-size: 0.78rem; font-weight: 700; line-height: 1.2; }
.chip--platform { border-color: oklch(0.46 0.065 140); color: var(--primary); }
.metadata { margin: 1.1rem 0 0; color: var(--muted); font-size: 0.85rem; line-height: 1.5; }
.game-card > .metadata:last-child { margin-top: auto; padding-top: 1.1rem; }
.metadata strong { color: var(--ink); }
.empty-state { max-width: 38rem; padding: clamp(1.5rem, 4vw, 2.5rem); border: 1px dashed var(--line-strong); border-radius: 1rem; background: var(--surface); }
.empty-state h2 { margin-bottom: 0.65rem; }
.empty-state p { max-width: 55ch; margin: 0; color: var(--muted); line-height: 1.6; }
.empty-state a { display: inline-block; margin-top: 1rem; color: var(--primary); font-weight: 800; }
.breadcrumb { display: flex; flex-wrap: wrap; gap: 0.55rem; margin-bottom: 1.5rem; color: var(--muted); font-size: 0.9rem; }
.breadcrumb a { color: var(--ink); }
.detail-hero { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: clamp(1.25rem, 4vw, 2.5rem); align-items: start; padding-bottom: 2rem; border-bottom: 1px solid var(--line); }
.detail-hero__copy { min-width: 0; }
.detail-hero h1 { max-width: 16ch; }
.detail-hero .chip-list { margin-top: 1.25rem; }
.detail-grid { display: grid; grid-template-columns: minmax(0, 1.45fr) minmax(17rem, 0.85fr); gap: 1rem; margin-top: 1rem; align-items: start; }
.detail-column { display: grid; min-width: 0; gap: 1rem; align-content: start; }
.detail-related { margin-top: 1rem; }
.content-section { padding: 1.25rem; border: 1px solid var(--line); border-radius: 1rem; background: var(--surface); }
.content-section h2 { margin-bottom: 1rem; }
.table-scroll { overflow-x: auto; }
table { width: 100%; border-collapse: collapse; text-align: left; }
caption { margin-bottom: 0.75rem; color: var(--muted); font-size: 0.85rem; text-align: left; }
th, td { padding: 0.72rem 0.5rem; border-bottom: 1px solid var(--line); vertical-align: top; }
th { color: var(--muted); font-size: 0.78rem; letter-spacing: 0.04em; text-transform: uppercase; }
td:first-child { color: var(--ink); font-weight: 750; }
.score-value { color: var(--primary); font-weight: 850; }
.score-unavailable { color: var(--muted); }
.video-link { color: var(--primary); font-weight: 800; }
.video-url { display: block; margin-top: 0.6rem; color: var(--muted); font-size: 0.8rem; overflow-wrap: anywhere; }
.summary-group + .summary-group { margin-top: 1.2rem; padding-top: 1.2rem; border-top: 1px solid var(--line); }
.summary-group h3 { margin-bottom: 0.55rem; }
.summary-list { display: grid; gap: 0.45rem; margin: 0; padding-left: 1.2rem; color: var(--muted); line-height: 1.5; }
.summary-list li::marker { color: var(--primary); }
.status-copy { margin: 0; color: var(--muted); line-height: 1.6; }
.similar-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(11rem, 100%), 1fr)); gap: 0.6rem; margin: 1rem 0 0; padding: 0; list-style: none; }
.similar-card { height: 100%; padding: 0.7rem 0.85rem; border: 1px solid var(--line); border-radius: 0.7rem; background: var(--surface); }
.similar-card a { display: block; color: var(--ink); font-weight: 800; text-decoration-color: transparent; }
.similar-card a:hover, .similar-card a:focus-visible { color: var(--primary); text-decoration-color: currentColor; }
.provenance { margin-top: 1rem; border-top: 1px solid var(--line); }
.provenance summary { padding-top: 1rem; color: var(--muted); cursor: pointer; font-size: 0.88rem; font-weight: 750; }
.provenance dl { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 0.55rem 1rem; margin: 1rem 0 0; color: var(--muted); font-size: 0.84rem; }
.provenance dt { color: var(--ink); font-weight: 750; }
.provenance dd { margin: 0; overflow-wrap: anywhere; }
.not-found { max-width: 42rem; }
.not-found .section-label { color: var(--accent); }
.visually-hidden { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
@media (max-width: 46rem) {
  .site-header__inner { align-items: flex-start; flex-direction: column; justify-content: center; padding-block: 0.75rem; }
  .header-note { text-align: left; }
  .catalogue-controls, .detail-grid { grid-template-columns: 1fr; }
  .detail-hero { grid-template-columns: 1fr; }
  .cover-placeholder--large, .cover-image--large { width: 100%; min-height: 8rem; }
  .results-heading { align-items: flex-start; flex-direction: column; }
  .results-heading p { text-align: left; }
}
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after { scroll-behavior: auto !important; transition-duration: 0.01ms !important; animation-duration: 0.01ms !important; animation-iteration-count: 1 !important; }
}
"#;

/// Build the embedded, server-rendered catalogue routes over an injected application read port.
pub fn catalogue_router<P>(catalogue: Arc<Mutex<P>>) -> Router
where
    P: GameCatalogueReadPort + Send + 'static,
    P::Error: Send + 'static,
{
    Router::new()
        .route("/", get(root_redirect))
        .route("/games", get(list_games::<P>))
        .route("/games/{id}/cover", get(show_cover::<P>))
        .route("/games/{id}", get(show_game::<P>))
        .with_state(CatalogueState { catalogue })
        .fallback(missing_page)
}

/// Build the complete HTTP surface, including liveness and durable-store readiness.
pub fn service_router<P, R>(catalogue: Arc<Mutex<P>>, readiness_probe: Arc<R>) -> Router
where
    P: GameCatalogueReadPort + Send + 'static,
    P::Error: Send + 'static,
    R: ServiceReadinessPort + 'static,
    R::Error: Send + 'static,
{
    Router::new()
        .route("/health/live", get(liveness))
        .route("/health/ready", get(readiness::<R>))
        .with_state(ReadinessState {
            readiness: readiness_probe,
        })
        .merge(catalogue_router(catalogue))
}

async fn root_redirect() -> Redirect {
    root_redirect_value()
}

fn root_redirect_value() -> Redirect {
    Redirect::permanent("/games")
}

async fn missing_page() -> Response {
    not_found_response()
}

/// Build the safe HTTP surface used while the configured durable store is unavailable.
pub fn unavailable_service_router<R>(readiness_probe: Arc<R>) -> Router
where
    R: ServiceReadinessPort + 'static,
    R::Error: Send + 'static,
{
    Router::new()
        .route("/", get(root_redirect))
        .route("/health/live", get(liveness))
        .route("/health/ready", get(readiness::<R>))
        .route("/games", get(database_unavailable))
        .route("/games/{id}/cover", get(database_unavailable))
        .route("/games/{id}", get(database_unavailable))
        .with_state(ReadinessState {
            readiness: readiness_probe,
        })
}

struct ReadinessState<R> {
    readiness: Arc<R>,
}

impl<R> Clone for ReadinessState<R> {
    fn clone(&self) -> Self {
        Self {
            readiness: Arc::clone(&self.readiness),
        }
    }
}

async fn liveness() -> Response {
    liveness_response().await
}

/// Return liveness without touching SQLite, source adapters, or worker scheduling.
pub async fn liveness_response() -> Response {
    StatusCode::OK.into_response()
}

async fn readiness<R>(State(state): State<ReadinessState<R>>) -> Response
where
    R: ServiceReadinessPort + 'static,
    R::Error: Send + 'static,
{
    readiness_response(&*state.readiness).await
}

/// Map the configured durable-store readiness port to an intentionally empty HTTP response.
pub async fn readiness_response<R>(readiness: &R) -> Response
where
    R: ServiceReadinessPort,
{
    if readiness.check_readiness().is_ok() {
        StatusCode::OK.into_response()
    } else {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    }
}

async fn database_unavailable() -> Response {
    database_unavailable_response().await
}

/// The fixed database-unavailable response shared by every database-backed route.
async fn database_unavailable_response() -> Response {
    StatusCode::SERVICE_UNAVAILABLE.into_response()
}

struct CatalogueState<P> {
    catalogue: Arc<Mutex<P>>,
}

impl<P> Clone for CatalogueState<P> {
    fn clone(&self) -> Self {
        Self {
            catalogue: Arc::clone(&self.catalogue),
        }
    }
}

async fn list_games<P>(
    State(state): State<CatalogueState<P>>,
    RawQuery(raw_query): RawQuery,
) -> Response
where
    P: GameCatalogueReadPort + Send + 'static,
    P::Error: Send + 'static,
{
    catalogue_response(state.catalogue, raw_query.as_deref()).await
}

/// Render one catalogue HTTP response without opening a listener.
///
/// The Axum `/games` route delegates to this function, which keeps fixture tests fully in-process.
pub async fn catalogue_response<P>(catalogue: Arc<Mutex<P>>, raw_query: Option<&str>) -> Response
where
    P: GameCatalogueReadPort + Send + 'static,
    P::Error: Send + 'static,
{
    let request = CatalogueHttpQuery::from_raw(raw_query);
    let mut catalogue = match catalogue.lock() {
        Ok(catalogue) => catalogue,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let page = match load_catalogue(&mut *catalogue, &request.as_application_query()) {
        Ok(page) => page,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    match CatalogueTemplate::from_page(page, request).render() {
        Ok(rendered) => Html(rendered).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn show_game<P>(
    State(state): State<CatalogueState<P>>,
    Path(raw_source_product_id): Path<String>,
) -> Response
where
    P: GameCatalogueReadPort + Send + 'static,
    P::Error: Send + 'static,
{
    match parse_source_product_id(&raw_source_product_id) {
        Some(source_product_id) => game_detail_response(state.catalogue, source_product_id).await,
        None => not_found_response(),
    }
}

async fn show_cover<P>(
    State(state): State<CatalogueState<P>>,
    Path(raw_source_product_id): Path<String>,
) -> Response
where
    P: GameCatalogueReadPort + Send + 'static,
    P::Error: Send + 'static,
{
    match parse_source_product_id(&raw_source_product_id) {
        Some(source_product_id) => cover_image_response(state.catalogue, source_product_id).await,
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn parse_source_product_id(value: &str) -> Option<u64> {
    value.parse::<u64>().ok()
}

/// Render one stored-game HTTP response without opening a listener.
///
/// The Axum `/games/{id}` route delegates to this function, which keeps fixture tests fully
/// in-process while preserving the route's response behaviour.
pub async fn game_detail_response<P>(catalogue: Arc<Mutex<P>>, source_product_id: u64) -> Response
where
    P: GameCatalogueReadPort + Send + 'static,
    P::Error: Send + 'static,
{
    let source_product_id = match SourceProductId::new(source_product_id) {
        Ok(source_product_id) => source_product_id,
        Err(_) => return not_found_response(),
    };
    let mut catalogue = match catalogue.lock() {
        Ok(catalogue) => catalogue,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let game = match load_catalogue_game(&mut *catalogue, source_product_id) {
        Ok(Some(game)) => game,
        Ok(None) => return not_found_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    match GameDetailTemplate::from_game(game).render() {
        Ok(rendered) => Html(rendered).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Serve one already-persisted local cover with a fixed allowlisted content type.
pub async fn cover_image_response<P>(catalogue: Arc<Mutex<P>>, source_product_id: u64) -> Response
where
    P: GameCatalogueReadPort + Send + 'static,
    P::Error: Send + 'static,
{
    let source_product_id = match SourceProductId::new(source_product_id) {
        Ok(source_product_id) => source_product_id,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let mut catalogue = match catalogue.lock() {
        Ok(catalogue) => catalogue,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    match load_catalogue_cover(&mut *catalogue, source_product_id) {
        Ok(Some(cover)) => (
            StatusCode::OK,
            [(CONTENT_TYPE, cover.content_type().as_str())],
            cover.bytes().to_vec(),
        )
            .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn not_found_response() -> Response {
    match NotFoundTemplate::new().render() {
        Ok(rendered) => (StatusCode::NOT_FOUND, Html(rendered)).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Clone, Debug, Default)]
struct CatalogueHttpQuery {
    title_search: String,
    platform_slug: String,
}

impl CatalogueHttpQuery {
    fn from_raw(raw_query: Option<&str>) -> Self {
        let mut query = Self::default();
        for component in raw_query.unwrap_or_default().split('&') {
            let Some((key, value)) = component.split_once('=') else {
                continue;
            };
            let Some(key) = decode_query_component(key) else {
                continue;
            };
            let Some(value) = decode_query_component(value) else {
                continue;
            };
            match key.as_str() {
                "q" if query.title_search.is_empty() => query.title_search = value,
                "platform" if query.platform_slug.is_empty() => query.platform_slug = value,
                _ => {}
            }
        }
        query
    }

    fn as_application_query(&self) -> CatalogueQuery {
        CatalogueQuery::new(
            (!self.title_search.is_empty()).then(|| self.title_search.clone()),
            (!self.platform_slug.is_empty()).then(|| self.platform_slug.clone()),
        )
    }
}

fn decode_query_component(value: &str) -> Option<String> {
    let mut decoded = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let high = decode_hex(bytes[index + 1])?;
                let low = decode_hex(bytes[index + 2])?;
                decoded.push((high << 4) | low);
                index += 3;
            }
            b'%' => return None,
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).ok()
}

fn decode_hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[derive(Template)]
#[template(
    source = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>GamePulse catalogue</title>
  <style>{{ ui_css|safe }}</style>
</head>
<body>
  <a class="skip-link" href="#main-content">Skip to catalogue</a>
  <header class="site-header">
    <div class="site-header__inner">
      <a class="brand" href="/games"><span class="brand__mark">Game</span>Pulse</a>
      <p class="header-note">Persisted catalogue · no live source required</p>
    </div>
  </header>
  <main id="main-content" class="page-shell">
    <section class="page-intro" aria-labelledby="catalogue-title">
      <p class="section-label">Stored game catalogue</p>
      <h1 id="catalogue-title">Find the signal in your local game data.</h1>
      <p class="lede">Browse persisted games, compare their best stored Metascores, and move into a full record without leaving the local catalogue.</p>
    </section>
    <section class="filter-panel" aria-label="Catalogue controls">
      <form class="catalogue-controls" action="/games" method="get" role="search">
        <div class="field">
          <label for="q">Title search</label>
          <input id="q" name="q" type="search" value="{{ title_search }}" placeholder="Search stored titles" autocomplete="off">
        </div>
        <div class="field">
          <label for="platform">Platform</label>
          <select id="platform" name="platform">
            <option value="">All stored platforms</option>
            {% for platform in platforms %}
            <option value="{{ platform.source_slug }}"{% if platform.selected %} selected{% endif %}>{{ platform.label }}</option>
            {% endfor %}
          </select>
        </div>
        <button class="primary-button" type="submit">Apply filters</button>
      </form>
    </section>
    {% if games.len() == 0 %}
    <section class="empty-state" aria-labelledby="empty-title" role="status">
      <p class="section-label">No matching records</p>
      <h2 id="empty-title">No stored games match this catalogue query.</h2>
      <p>Try a different title or platform. The catalogue only shows data already saved in the local database.</p>
      {% if has_filters %}<a href="/games">Clear catalogue filters</a>{% endif %}
    </section>
    {% else %}
    <section aria-labelledby="results-title">
      <div class="results-heading">
        <h2 id="results-title">{{ games.len() }} stored games</h2>
        <p>{{ score_sort_copy }}</p>
      </div>
      <ol class="game-grid" aria-label="Stored games">
        {% for game in games %}
        <li>
          <article class="game-card">
            <div class="game-card__top">
              {% if game.has_local_cover %}
              <img class="cover-image" src="/games/{{ game.source_product_id }}/cover" alt="Cover for {{ game.title }}" loading="lazy" decoding="async">
              {% else %}
              {% match game.public_cover_url %}
              {% when Some with (cover_url) %}
              <img class="cover-image" src="{{ cover_url }}" alt="Cover for {{ game.title }}" loading="lazy" decoding="async" referrerpolicy="no-referrer">
              {% when None %}
              <div class="cover-placeholder" aria-hidden="true">GP</div>
              {% endmatch %}
              {% endif %}
              <div>
                <h3><a class="game-title" href="/games/{{ game.source_product_id }}">{{ game.title }}</a></h3>
              </div>
              {% match game.highest_metascore %}
              {% when Some with (metascore) %}
              <div class="score-badge"><span class="score-badge__label">{{ score_context_label }}</span><strong>{{ metascore }}</strong></div>
              {% when None %}
              <div class="score-badge score-badge--empty"><span class="score-badge__label">{{ score_context_label }}</span><strong>—</strong></div>
              {% endmatch %}
            </div>
            {% if game.platforms.len() == 0 %}
            <p class="metadata"><strong>Platforms:</strong> No stored platforms</p>
            {% else %}
            <ul class="chip-list" aria-label="{{ game.title }} platforms">
              {% for platform in game.platforms %}<li class="chip chip--platform">{{ platform }}</li>{% endfor %}
            </ul>
            {% endif %}
            {% if game.developers.len() == 0 %}
            <p class="metadata"><strong>Developers:</strong> No stored developers</p>
            {% else %}
            <p class="metadata"><strong>Developed by:</strong> {% for developer in game.developers %}{{ developer }}{% if !loop.last %}, {% endif %}{% endfor %}</p>
            {% endif %}
          </article>
        </li>
        {% endfor %}
      </ol>
    </section>
    {% endif %}
  </main>
</body>
</html>"##,
    ext = "html"
)]
struct CatalogueTemplate {
    ui_css: &'static str,
    title_search: String,
    platforms: Vec<CataloguePlatformView>,
    games: Vec<CatalogueGameCardView>,
    has_filters: bool,
    score_context_label: String,
    score_sort_copy: String,
}

impl CatalogueTemplate {
    fn from_page(page: CataloguePage, query: CatalogueHttpQuery) -> Self {
        let selected_platform = query.platform_slug;
        let selected_platform_label = platform_label(&selected_platform);
        let score_context_label = if selected_platform.is_empty() {
            "Best score".to_owned()
        } else {
            selected_platform_label.clone()
        };
        let score_sort_copy = if selected_platform.is_empty() {
            "Sorted by the best stored Metascore across platforms.".to_owned()
        } else {
            format!(
                "Sorted by the stored {selected_platform_label} Metascore; releases without a score appear last."
            )
        };
        Self {
            ui_css: UI_CSS,
            has_filters: !query.title_search.is_empty() || !selected_platform.is_empty(),
            title_search: query.title_search,
            score_context_label,
            score_sort_copy,
            platforms: page
                .platform_filters()
                .iter()
                .map(|platform| CataloguePlatformView {
                    source_slug: platform.source_slug().to_owned(),
                    label: platform_label(platform.source_slug()),
                    selected: platform
                        .source_slug()
                        .eq_ignore_ascii_case(&selected_platform),
                })
                .collect(),
            games: page
                .games()
                .iter()
                .map(|game| CatalogueGameCardView {
                    source_product_id: game.source_product_id().value(),
                    title: game.title().to_owned(),
                    has_local_cover: game.has_local_cover(),
                    public_cover_url: safe_cover_url(game.public_cover_url()),
                    highest_metascore: game.highest_metascore(),
                    platforms: game
                        .platforms()
                        .iter()
                        .map(|platform| platform_label(platform))
                        .collect(),
                    developers: game.developers().to_vec(),
                })
                .collect(),
        }
    }
}

struct CataloguePlatformView {
    source_slug: String,
    label: String,
    selected: bool,
}

struct CatalogueGameCardView {
    source_product_id: u64,
    title: String,
    has_local_cover: bool,
    public_cover_url: Option<String>,
    highest_metascore: Option<u8>,
    platforms: Vec<String>,
    developers: Vec<String>,
}

#[derive(Template)]
#[template(
    source = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{{ game.title }} · GamePulse</title>
  <style>{{ ui_css|safe }}</style>
</head>
<body>
  <a class="skip-link" href="#main-content">Skip to game details</a>
  <header class="site-header">
    <div class="site-header__inner">
      <a class="brand" href="/games"><span class="brand__mark">Game</span>Pulse</a>
      <p class="header-note">Persisted catalogue · no live source required</p>
    </div>
  </header>
  <main id="main-content" class="page-shell">
    <nav class="breadcrumb" aria-label="Breadcrumb">
      <a href="/games">Catalogue</a><span aria-hidden="true">/</span><span aria-current="page">{{ game.title }}</span>
    </nav>
    <article>
      <header class="detail-hero">
        <div>
          {% if game.has_local_cover %}
          <img class="cover-image cover-image--large" src="/games/{{ game.source_product_id }}/cover" alt="Cover for {{ game.title }}">
          {% else %}
          {% match game.public_cover_url %}
          {% when Some with (cover_url) %}
          <img class="cover-image cover-image--large" src="{{ cover_url }}" alt="Cover for {{ game.title }}" referrerpolicy="no-referrer">
          {% when None %}
          <div class="cover-placeholder cover-placeholder--large" aria-hidden="true">GP</div>
          <p class="cover-status">No stored cover image available.</p>
          {% endmatch %}
          {% endif %}
        </div>
        <div class="detail-hero__copy">
          <p class="section-label">Stored game record</p>
          <h1>{{ game.title }}</h1>
          <p class="lede">{{ game.description }}</p>
          {% if game.developers.len() == 0 %}
          <p class="metadata"><strong>Developers:</strong> No stored developers</p>
          {% else %}
          <ul class="chip-list" aria-label="{{ game.title }} developers">
            {% for developer in game.developers %}<li class="chip">{{ developer }}</li>{% endfor %}
          </ul>
          {% endif %}
        </div>
      </header>
      <div class="detail-grid">
        <div class="detail-column">
        <section class="content-section" aria-labelledby="platform-scores-title">
          <h2 id="platform-scores-title">Platform scores</h2>
          {% if game.platform_scores.len() == 0 %}
          <p class="status-copy">No platform scores stored for this game yet.</p>
          {% else %}
          <div class="table-scroll">
            <table>
              <caption>Stored score comparison by platform</caption>
              <thead><tr><th scope="col">Platform</th><th scope="col">Metascore</th><th scope="col">Userscore</th></tr></thead>
              <tbody>
                {% for platform in game.platform_scores %}
                <tr>
                  <td>{{ platform.label }}</td>
                  <td>{% match platform.metascore %}{% when Some with (metascore) %}<span class="score-value">{{ metascore }}</span>{% when None %}<span class="score-unavailable">Not stored</span>{% endmatch %}</td>
                  <td>{% match platform.userscore %}{% when Some with (userscore) %}<span class="score-value">{{ userscore }}</span>{% when None %}<span class="score-unavailable">Not stored</span>{% endmatch %}</td>
                </tr>
                {% endfor %}
              </tbody>
            </table>
          </div>
          {% endif %}
        </section>
        <section class="content-section" aria-labelledby="users-title">
          <h2 id="users-title">What players said</h2>
          {% match game.user_summary %}
          {% when Some with (summary) %}
            {% match summary.status %}
            {% when ReviewSummaryStatus::Pending %}<p class="status-copy">Summary pending for the current stored review refresh.</p>
            {% when ReviewSummaryStatus::Unavailable %}<p class="status-copy">Unavailable: no stored user excerpts.</p>
            {% when ReviewSummaryStatus::Available %}
            {% if summary.likes.len() > 0 %}<div class="summary-group"><h3>Praise</h3><ul class="summary-list">{% for item in summary.likes %}<li>{{ item }}</li>{% endfor %}</ul></div>{% endif %}
            {% if summary.dislikes.len() > 0 %}<div class="summary-group"><h3>Criticism</h3><ul class="summary-list">{% for item in summary.dislikes %}<li>{{ item }}</li>{% endfor %}</ul></div>{% endif %}
            {% if summary.mixed.len() > 0 %}<div class="summary-group"><h3>Mixed</h3><ul class="summary-list">{% for item in summary.mixed %}<li>{{ item }}</li>{% endfor %}</ul></div>{% endif %}
            {% if summary.likes.len() == 0 && summary.dislikes.len() == 0 && summary.mixed.len() == 0 %}<p class="status-copy">Reviews were stored, but no clear highlights were available.</p>{% endif %}
            {% endmatch %}
          {% when None %}<p class="status-copy">No stored user review summary.</p>
          {% endmatch %}
        </section>
        </div>
        <div class="detail-column">
        <section class="content-section" aria-labelledby="video-title">
          <h2 id="video-title">Stored video</h2>
          {% match game.video %}
          {% when Some with (video) %}
            {% if video.is_safe_href %}
            <a class="video-link" href="{{ video.value }}" rel="noopener noreferrer" target="_blank">Open stored video <span class="visually-hidden">in a new tab</span></a>
            <code class="video-url">{{ video.value }}</code>
            {% else %}
            <code class="video-url">{{ video.value }}</code>
            <p class="status-copy">The stored link is not rendered as a navigable URL.</p>
            {% endif %}
          {% when None %}
          <p class="status-copy">No video link stored for this game.</p>
          {% endmatch %}
        </section>
        <section class="content-section" aria-labelledby="critics-title">
          <h2 id="critics-title">What critics said</h2>
          {% match game.critic_summary %}
          {% when Some with (summary) %}
            {% match summary.status %}
            {% when ReviewSummaryStatus::Pending %}<p class="status-copy">Summary pending for the current stored review refresh.</p>
            {% when ReviewSummaryStatus::Unavailable %}<p class="status-copy">Unavailable: no stored critic excerpts.</p>
            {% when ReviewSummaryStatus::Available %}
            {% if summary.likes.len() > 0 %}<div class="summary-group"><h3>Praise</h3><ul class="summary-list">{% for item in summary.likes %}<li>{{ item }}</li>{% endfor %}</ul></div>{% endif %}
            {% if summary.dislikes.len() > 0 %}<div class="summary-group"><h3>Criticism</h3><ul class="summary-list">{% for item in summary.dislikes %}<li>{{ item }}</li>{% endfor %}</ul></div>{% endif %}
            {% if summary.mixed.len() > 0 %}<div class="summary-group"><h3>Mixed</h3><ul class="summary-list">{% for item in summary.mixed %}<li>{{ item }}</li>{% endfor %}</ul></div>{% endif %}
            {% if summary.likes.len() == 0 && summary.dislikes.len() == 0 && summary.mixed.len() == 0 %}<p class="status-copy">Reviews were stored, but no clear highlights were available.</p>{% endif %}
            {% endmatch %}
          {% when None %}<p class="status-copy">No stored critic review summary.</p>
          {% endmatch %}
        </section>
        </div>
      </div>
        <section class="content-section detail-related" aria-labelledby="similar-title">
          <h2 id="similar-title">Other games in this catalogue</h2>
          <p class="status-copy">Matched on a shared platform or studio, so genres may vary.</p>
          {% if game.similar_games.len() == 0 %}
          <p class="status-copy">Nothing else in the catalogue shares a platform or studio with this game.</p>
          {% else %}
          <ul class="similar-grid">
            {% for similar in game.similar_games %}
            <li class="similar-card"><a href="/games/{{ similar.source_product_id }}">{{ similar.title }}</a></li>
            {% endfor %}
          </ul>
          {% endif %}
        </section>
    </article>
  </main>
</body>
</html>"##,
    ext = "html"
)]
struct GameDetailTemplate {
    ui_css: &'static str,
    game: CatalogueGameDetailView,
}

impl GameDetailTemplate {
    fn from_game(game: CatalogueGameDetail) -> Self {
        Self {
            ui_css: UI_CSS,
            game: CatalogueGameDetailView {
                source_product_id: game.source_product_id().value(),
                title: game.title().to_owned(),
                description: game.description().to_owned(),
                has_local_cover: game.has_local_cover(),
                public_cover_url: safe_cover_url(game.public_cover_url()),
                video: game.video_url().map(|value| CatalogueVideoView {
                    value: value.to_owned(),
                    is_safe_href: is_safe_http_link(value),
                }),
                platform_scores: game
                    .platform_scores()
                    .iter()
                    .map(|platform| CataloguePlatformScoreView {
                        label: platform_label(platform.source_slug()),
                        metascore: platform.metascore(),
                        userscore: visible_userscore(platform.userscore()),
                    })
                    .collect(),
                developers: game.developers().to_vec(),
                critic_summary: game.critic_summary().map(review_summary_view),
                user_summary: game.user_summary().map(review_summary_view),
                similar_games: game
                    .similar_games()
                    .iter()
                    .map(|similar| SimilarGameView {
                        source_product_id: similar.source_product_id().value(),
                        title: similar.title().to_owned(),
                    })
                    .collect(),
            },
        }
    }
}

fn is_safe_http_link(value: &str) -> bool {
    value
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
        || value
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
}

fn safe_cover_url(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| {
            value
                .get(..8)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
        })
        .map(str::to_owned)
}

struct CatalogueGameDetailView {
    source_product_id: u64,
    title: String,
    description: String,
    has_local_cover: bool,
    public_cover_url: Option<String>,
    video: Option<CatalogueVideoView>,
    platform_scores: Vec<CataloguePlatformScoreView>,
    developers: Vec<String>,
    critic_summary: Option<ReviewSummaryView>,
    user_summary: Option<ReviewSummaryView>,
    similar_games: Vec<SimilarGameView>,
}

fn review_summary_view(summary: &CatalogueReviewSummary) -> ReviewSummaryView {
    match summary {
        CatalogueReviewSummary::Pending => ReviewSummaryView {
            status: ReviewSummaryStatus::Pending,
            likes: Vec::new(),
            dislikes: Vec::new(),
            mixed: Vec::new(),
        },
        CatalogueReviewSummary::Unavailable => ReviewSummaryView {
            status: ReviewSummaryStatus::Unavailable,
            likes: Vec::new(),
            dislikes: Vec::new(),
            mixed: Vec::new(),
        },
        CatalogueReviewSummary::Available { likes, dislikes } => {
            let mixed = likes
                .iter()
                .filter(|item| dislikes.contains(item))
                .cloned()
                .collect::<Vec<_>>();
            ReviewSummaryView {
                status: ReviewSummaryStatus::Available,
                likes: likes
                    .iter()
                    .filter(|item| !mixed.contains(item))
                    .cloned()
                    .collect(),
                dislikes: dislikes
                    .iter()
                    .filter(|item| !mixed.contains(item))
                    .cloned()
                    .collect(),
                mixed,
            }
        }
    }
}

#[derive(Clone, Copy)]
enum ReviewSummaryStatus {
    Pending,
    Unavailable,
    Available,
}

struct ReviewSummaryView {
    status: ReviewSummaryStatus,
    likes: Vec<String>,
    dislikes: Vec<String>,
    mixed: Vec<String>,
}

struct CatalogueVideoView {
    value: String,
    is_safe_href: bool,
}

struct CataloguePlatformScoreView {
    label: String,
    metascore: Option<u8>,
    userscore: Option<f64>,
}

fn visible_userscore(score: Option<f64>) -> Option<f64> {
    score.filter(|score| *score > 0.0)
}

fn platform_label(slug: &str) -> String {
    match slug {
        "pc" => "PC".to_owned(),
        "ios-iphoneipad" => "iOS (iPhone / iPad)".to_owned(),
        "nintendo-switch" => "Nintendo Switch".to_owned(),
        "nintendo-switch-2" => "Nintendo Switch 2".to_owned(),
        "playstation-5" => "PlayStation 5".to_owned(),
        "xbox-one" => "Xbox One".to_owned(),
        "xbox-series-x" => "Xbox Series X".to_owned(),
        other => other.replace('-', " "),
    }
}

struct SimilarGameView {
    source_product_id: u64,
    title: String,
}

#[derive(Template)]
#[template(
    source = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Game not found · GamePulse</title>
  <style>{{ ui_css|safe }}</style>
</head>
<body>
  <a class="skip-link" href="#main-content">Skip to not found message</a>
  <header class="site-header">
    <div class="site-header__inner">
      <a class="brand" href="/games"><span class="brand__mark">Game</span>Pulse</a>
      <p class="header-note">Persisted catalogue · no live source required</p>
    </div>
  </header>
  <main id="main-content" class="page-shell">
    <section class="empty-state not-found" aria-labelledby="not-found-title">
      <p class="section-label">Record not found</p>
      <h1 id="not-found-title">This game is not in the stored catalogue.</h1>
      <p>The requested record is unavailable locally. Return to the catalogue to choose another stored game.</p>
      <a href="/games">Back to catalogue</a>
    </section>
  </main>
</body>
</html>"##,
    ext = "html"
)]
struct NotFoundTemplate {
    ui_css: &'static str,
}

impl NotFoundTemplate {
    fn new() -> Self {
        Self { ui_css: UI_CSS }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_helpers_keep_source_keys_internal_and_hide_tbd_zero_scores() {
        assert_eq!(platform_label("pc"), "PC");
        assert_eq!(platform_label("playstation-5"), "PlayStation 5");
        assert_eq!(platform_label("future-platform"), "future platform");
        assert_eq!(visible_userscore(Some(0.0)), None);
        assert_eq!(visible_userscore(Some(8.3)), Some(8.3));
        assert_eq!(visible_userscore(None), None);
    }

    #[test]
    fn mixed_review_items_are_rendered_once_in_their_own_group() {
        let summary = CatalogueReviewSummary::Available {
            likes: vec!["praise".to_owned(), "mixed".to_owned()],
            dislikes: vec!["criticism".to_owned(), "mixed".to_owned()],
        };
        let view = review_summary_view(&summary);
        assert_eq!(view.likes, ["praise"]);
        assert_eq!(view.dislikes, ["criticism"]);
        assert_eq!(view.mixed, ["mixed"]);
    }

    #[test]
    fn route_helpers_redirect_root_and_reject_non_numeric_game_ids() {
        let response = root_redirect_value().into_response();
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(response.headers().get("location").unwrap(), "/games");
        assert_eq!(parse_source_product_id("42"), Some(42));
        assert_eq!(parse_source_product_id("does-not-exist"), None);
    }
}
