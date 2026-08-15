#![forbid(unsafe_code)]

//! Server-rendered HTTP, SSE, and embedded UI adapter.

use std::sync::{Arc, Mutex};

use askama::Template;
use axum::Router;
use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use gamepulse_application::{
    CatalogueGameDetail, CataloguePage, CatalogueQuery, GameCatalogueReadPort, SourceProductId,
    load_catalogue, load_catalogue_game,
};

const EMPTY_PLATFORMS: &str = "No stored platforms";
const EMPTY_DEVELOPERS: &str = "No stored developers";

/// Build the embedded, server-rendered catalogue routes over an injected application read port.
pub fn catalogue_router<P>(catalogue: Arc<Mutex<P>>) -> Router
where
    P: GameCatalogueReadPort + Send + 'static,
    P::Error: Send + 'static,
{
    Router::new()
        .route("/games", get(list_games::<P>))
        .route("/games/{id}", get(show_game::<P>))
        .with_state(CatalogueState { catalogue })
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
    Path(source_product_id): Path<u64>,
) -> Response
where
    P: GameCatalogueReadPort + Send + 'static,
    P::Error: Send + 'static,
{
    game_detail_response(state.catalogue, source_product_id).await
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
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let mut catalogue = match catalogue.lock() {
        Ok(catalogue) => catalogue,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let game = match load_catalogue_game(&mut *catalogue, source_product_id) {
        Ok(Some(game)) => game,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    match GameDetailTemplate::from_game(game).render() {
        Ok(rendered) => Html(rendered).into_response(),
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
    source = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>GamePulse catalogue</title>
</head>
<body>
  <main>
    <h1>GamePulse catalogue</h1>
    <form action="/games" method="get">
      <label for="q">Title search</label>
      <input id="q" name="q" type="search" value="{{ title_search }}">
      <label for="platform">Platform</label>
      <select id="platform" name="platform">
        <option value="">All stored platforms</option>
        {% for platform in platforms %}
        <option value="{{ platform.source_slug }}"{% if platform.selected %} selected{% endif %}>{{ platform.source_slug }}</option>
        {% endfor %}
      </select>
      <button type="submit">Search and sort by rating</button>
    </form>
    {% if games.len() == 0 %}
    <p>No stored games match this catalogue query.</p>
    {% else %}
    <ol>
      {% for game in games %}
      <li>
        <article>
          <h2><a href="/games/{{ game.source_product_id }}">{{ game.title }}</a></h2>
          <p>Metascore: {% match game.highest_metascore %}{% when Some with (metascore) %}{{ metascore }}{% when None %}Not stored{% endmatch %}</p>
          <p>Platforms: {{ game.platforms }}</p>
          <p>Developers: {{ game.developers }}</p>
        </article>
      </li>
      {% endfor %}
    </ol>
    {% endif %}
  </main>
</body>
</html>"#,
    ext = "html"
)]
struct CatalogueTemplate {
    title_search: String,
    platforms: Vec<CataloguePlatformView>,
    games: Vec<CatalogueGameCardView>,
}

impl CatalogueTemplate {
    fn from_page(page: CataloguePage, query: CatalogueHttpQuery) -> Self {
        let selected_platform = query.platform_slug;
        Self {
            title_search: query.title_search,
            platforms: page
                .platform_filters()
                .iter()
                .map(|platform| CataloguePlatformView {
                    source_slug: platform.source_slug().to_owned(),
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
                    highest_metascore: game.highest_metascore(),
                    platforms: display_values(game.platforms(), EMPTY_PLATFORMS),
                    developers: display_values(game.developers(), EMPTY_DEVELOPERS),
                })
                .collect(),
        }
    }
}

struct CataloguePlatformView {
    source_slug: String,
    selected: bool,
}

struct CatalogueGameCardView {
    source_product_id: u64,
    title: String,
    highest_metascore: Option<u8>,
    platforms: String,
    developers: String,
}

#[derive(Template)]
#[template(
    source = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>{{ game.title }} · GamePulse</title>
</head>
<body>
  <main>
    <p><a href="/games">Back to catalogue</a></p>
    <article>
      <h1>{{ game.title }}</h1>
      <dl>
        <dt>Source product ID</dt><dd>{{ game.source_product_id }}</dd>
        <dt>Source slug</dt><dd>{{ game.source_slug }}</dd>
        <dt>Description</dt><dd>{{ game.description }}</dd>
      </dl>
      <section>
        <h2>Stored cover descriptor</h2>
        {% match game.cover %}
        {% when Some with (cover) %}
        <dl>
          <dt>Bucket path</dt><dd>{{ cover.bucket_path }}</dd>
          <dt>Bucket type</dt><dd>{{ cover.bucket_type }}</dd>
          <dt>Filename</dt><dd>{{ cover.filename }}</dd>
          <dt>Kind</dt><dd>{{ cover.kind }}</dd>
        </dl>
        {% when None %}
        <p>No cover descriptor stored.</p>
        {% endmatch %}
      </section>
      <section>
        <h2>Stored video link</h2>
        {% match game.video %}
        {% when Some with (video) %}
          {% if video.is_safe_href %}
          <a href="{{ video.value }}" rel="noreferrer">{{ video.value }}</a>
          {% else %}
          <code>{{ video.value }}</code>
          <p>The stored link is not rendered as a navigable URL.</p>
          {% endif %}
        {% when None %}
        <p>No video link stored.</p>
        {% endmatch %}
      </section>
      <section>
        <h2>Platform scores</h2>
        {% if game.platform_scores.len() == 0 %}
        <p>No platform scores stored.</p>
        {% else %}
        <table>
          <thead><tr><th>Platform</th><th>Metascore</th><th>Userscore</th></tr></thead>
          <tbody>
            {% for platform in game.platform_scores %}
            <tr>
              <td>{{ platform.source_slug }}</td>
              <td>{% match platform.metascore %}{% when Some with (metascore) %}{{ metascore }}{% when None %}Not stored{% endmatch %}</td>
              <td>{% match platform.userscore %}{% when Some with (userscore) %}{{ userscore }}{% when None %}Not stored{% endmatch %}</td>
            </tr>
            {% endfor %}
          </tbody>
        </table>
        {% endif %}
      </section>
      <section>
        <h2>Developers</h2>
        <p>{{ game.developers }}</p>
      </section>
      <section>
        <h2>Similar stored games</h2>
        {% if game.similar_games.len() == 0 %}
        <p>No similar stored games found.</p>
        {% else %}
        <ul>
          {% for similar in game.similar_games %}
          <li><a href="/games/{{ similar.source_product_id }}">{{ similar.title }}</a></li>
          {% endfor %}
        </ul>
        {% endif %}
      </section>
    </article>
  </main>
</body>
</html>"#,
    ext = "html"
)]
struct GameDetailTemplate {
    game: CatalogueGameDetailView,
}

impl GameDetailTemplate {
    fn from_game(game: CatalogueGameDetail) -> Self {
        Self {
            game: CatalogueGameDetailView {
                source_product_id: game.source_product_id().value(),
                source_slug: game.source_slug().to_owned(),
                title: game.title().to_owned(),
                description: game.description().to_owned(),
                cover: game.cover().map(|cover| CatalogueCoverView {
                    bucket_path: cover.bucket_path().to_owned(),
                    bucket_type: cover.bucket_type().to_owned(),
                    filename: cover.filename().to_owned(),
                    kind: cover.kind().to_owned(),
                }),
                video: game.video_url().map(|value| CatalogueVideoView {
                    value: value.to_owned(),
                    is_safe_href: is_safe_http_link(value),
                }),
                platform_scores: game
                    .platform_scores()
                    .iter()
                    .map(|platform| CataloguePlatformScoreView {
                        source_slug: platform.source_slug().to_owned(),
                        metascore: platform.metascore(),
                        userscore: platform.userscore(),
                    })
                    .collect(),
                developers: display_values(game.developers(), EMPTY_DEVELOPERS),
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

fn display_values(values: &[String], missing: &str) -> String {
    if values.is_empty() {
        missing.to_owned()
    } else {
        values.join(", ")
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

struct CatalogueGameDetailView {
    source_product_id: u64,
    source_slug: String,
    title: String,
    description: String,
    cover: Option<CatalogueCoverView>,
    video: Option<CatalogueVideoView>,
    platform_scores: Vec<CataloguePlatformScoreView>,
    developers: String,
    similar_games: Vec<SimilarGameView>,
}

struct CatalogueCoverView {
    bucket_path: String,
    bucket_type: String,
    filename: String,
    kind: String,
}

struct CatalogueVideoView {
    value: String,
    is_safe_href: bool,
}

struct CataloguePlatformScoreView {
    source_slug: String,
    metascore: Option<u8>,
    userscore: Option<f64>,
}

struct SimilarGameView {
    source_product_id: u64,
    title: String,
}
