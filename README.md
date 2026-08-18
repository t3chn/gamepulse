# GamePulse

GamePulse is a take-home project that collects newly released games from
Metacritic, stores their details and review summaries, and presents the result
in a searchable web catalogue.

- **Live service:** [gamepulse.10g.dev/games](https://gamepulse.10g.dev/games)
- **Repository:** [github.com/t3chn/gamepulse](https://github.com/t3chn/gamepulse)
- **AI-assisted development record:** [`docs/ai/`](docs/ai/)

## What to try

Open the [live catalogue](https://gamepulse.10g.dev/games) and:

1. browse the 20 most recently processed games;
2. search by title;
3. filter by platform;
4. verify that games are ordered by Metascore;
5. open a game to see its description, developers, platform-specific
   Metascore and Userscore, stored video link, separate critic and user review
   summaries, and links to similar stored games.

The public instance uses real Metacritic data. The most recent bounded
acceptance run selected, processed, and stored exactly 20 games in 43.439
seconds. Both review summaries were ready for all 20 games. Metacritic supplied
a video link for 4 of them; an omitted source value is shown as unavailable.

## Assignment coverage

### Mandatory part

| Requirement | Implementation |
| --- | --- |
| Run every hour | A durable hourly scheduler starts immediately and then ticks once per hour. Hour identities prevent duplicate scheduling. |
| Process 20 games not processed today | Each durable run targets exactly 20 successful unique games. Daily state and source identities survive restart. |
| Start each day with New Releases | The first daily selection uses New Releases. Later selections use the newest-first SEE ALL feed and persisted pagination. A new UTC day resets the sequence. |
| Insert or update games | SQLite upserts the current game snapshot and refreshes source-derived review data. |
| Title, cover, platforms, scores, developer, description, video | Stored and rendered on the catalogue/detail pages. Missing source fields remain explicitly unavailable. |
| Separate critic and user summaries | Reviews are fetched and stored separately. A local deterministic summarizer produces independent likes/dislikes summaries. |
| Catalogue and game page | Implemented as server-rendered pages with assets embedded in the binary. |
| Search, platform filter, rating sort | Implemented from persisted SQLite data; page requests do not call Metacritic. |
| Similar games | Deterministically selected from stored games by shared platforms and developers; links open their detail pages. |
| Repository and live service | Linked at the top of this document. |
| AI correspondence | Sanitized project-facing prompts and responses retained by the development workflow are under [`docs/ai/`](docs/ai/). See its index for the exact coverage boundary. |

### Optional parts

The YouTube letsplay/transcript enrichment and realtime worker dashboard with a
manual-run button are **not implemented**. They were intentionally left out to
keep the take-home focused on a complete, demonstrable mandatory path.

## Important evaluation notes

This is an evaluation build, not a production-ready service.

- Metacritic is an external public source with no stability guarantee for the
  observed endpoints. Parsing is isolated behind a source adapter and covered
  by captured contract fixtures, but the upstream contract can still change.
- Review summaries are deterministic and local. The `llm` lane is a clean
  provider boundary, but no paid model or secret is required to evaluate the
  project.
- SQLite is both the application database and durable job queue. The deployed
  shape is deliberately one process, one replica, and one persistent volume.
  Horizontal scaling would require a different persistence/queue boundary.
- Missing optional source values do not displace a newer valid game. This is
  why the live set can contain fewer than 20 video links while still containing
  exactly the 20 selected games.
- The service uses direct HTTP requests rather than browser automation. The
  current source contract is documented in
  [`docs/source-contracts/metacritic-direct-http.md`](docs/source-contracts/metacritic-direct-http.md).

For the detailed architecture rationale and verification map, see
[`AGENTS.md`](AGENTS.md), [`ARCHITECTURE.md`](ARCHITECTURE.md), and
[`docs/requirements.md`](docs/requirements.md).

## Architecture at a glance

GamePulse is a modular Rust monolith:

- one Cargo workspace with explicit domain, application, storage, web, and
  worker boundaries;
- one deployable Tokio binary;
- SQLite-backed application state, crawl progression, runs, queue jobs,
  leases, retries, and summaries;
- separate logical source, media, and LLM worker lanes;
- Axum and Askama server-rendered pages with embedded CSS;
- no separate frontend build or runtime service.

The worker lanes are logical components inside one process, not independent
services. Durable work lives in SQLite, while in-memory notifications only
reduce wake-up latency.

## Run locally

The project pins its Rust toolchain through [mise](https://mise.jdx.dev/).

```bash
mise install
mise run architecture
mise run ci
```

For a deterministic UI demo that makes no external requests:

```bash
mise run demo
```

Then open [127.0.0.1:3000/games](http://127.0.0.1:3000/games). The demo uses a
temporary seeded database and removes it on shutdown.

To run the actual service:

```bash
mkdir -p var
export GAMEPULSE_DATABASE_PATH="$PWD/var/gamepulse.sqlite3"
export GAMEPULSE_HTTP_ADDRESS="127.0.0.1:3000"
export GAMEPULSE_LOG_FORMAT="human"
export GAMEPULSE_SOURCE_WORK_ENABLED="true"
cargo run --locked -p gamepulse
```

Set `GAMEPULSE_SOURCE_WORK_ENABLED=false` for an offline UI/smoke run. The
catalogue always reads only persisted data.

Health endpoints:

- `GET /health/live` checks only that the process is alive;
- `GET /health/ready` verifies that the configured SQLite database and required
  schema are available.

## One-shot acceptance check

The evaluator command runs one real mandatory cycle against a fresh SQLite
database without starting the web server or hourly daemon. Its built-in help is
available offline:

```bash
cargo run --locked --offline -p gamepulse -- acceptance-once --help
```

The safe template below creates a fresh database path, runs the bounded cycle,
and removes only the temporary directory it created:

```bash
(
  acceptance_dir="$(mktemp -d /tmp/gamepulse-acceptance.XXXXXX)" || exit 1
  case "$acceptance_dir" in
    /tmp/gamepulse-acceptance.*) ;;
    *) printf '%s\n' 'acceptance temporary directory is invalid' >&2; exit 2 ;;
  esac
  database_path="$acceptance_dir/gamepulse.sqlite3"
  cargo run --locked --offline -p gamepulse -- acceptance-once \
    --database "$database_path" \
    --target 20 \
    --deadline-seconds 180
  command_status=$?
  rm -rf -- "$acceptance_dir"
  exit "$command_status"
)
```

It prints one privacy-safe `gamepulse.acceptance.v1` JSON report. Success means
exactly 20 games were persisted and both critic and user summaries became
ready. This command contacts the live public source, so it should be used as a
bounded evaluator check rather than as a unit test.

## Verification

```bash
mise run architecture  # validates the adopted Cargo ownership graph
mise run ci            # formatting, Clippy, and all offline tests
mise run mutation      # critical policy and persistence mutation checks
```

Most source behavior is tested offline using captured fixtures. Live source
diagnostics are explicit opt-in tools and are documented in
[`docs/source-contracts/metacritic-direct-http.md`](docs/source-contracts/metacritic-direct-http.md).

## Container

The `Dockerfile` builds the same binary with `--locked` dependencies. The
runtime image runs as a non-root user and expects SQLite on persistent storage:

```bash
docker build --tag gamepulse:local .
docker run --rm -p 3000:3000 \
  -e GAMEPULSE_HTTP_ADDRESS=0.0.0.0:3000 \
  -e GAMEPULSE_DATABASE_PATH=/var/lib/gamepulse/gamepulse.sqlite3 \
  -e GAMEPULSE_LOG_FORMAT=human \
  -e GAMEPULSE_SOURCE_WORK_ENABLED=true \
  -v gamepulse-data:/var/lib/gamepulse \
  gamepulse:local
```

The deployed image is `ghcr.io/t3chn/gamepulse:952e7fa` with digest
`sha256:4bc97cc98cc3cc57c588f440adfe25e1ace7cd855a68b646c65d2bb0f5ed8df4`.

## Repository boundary

This repository is self-contained for candidate evaluation. It intentionally
contains no credentials, recruiter correspondence, compensation details, or
private control metadata. No open-source license is granted; the code is
provided for evaluation unless a separate license is added.
