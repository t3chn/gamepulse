# GamePulse

GamePulse is a Rust take-home project for durable game discovery, review
summarization, and evaluator-visible worker progress.

## Status

The repository has an eight-package Cargo workspace, architecture harness, a
bounded direct-HTTP Metacritic source-contract canary in
`gamepulse-worker-source`, and a deterministic M003 daily-crawl selection seam.
M003 plans New Releases first, then newest-first browse progression, with
numeric-ID daily uniqueness, replay of a partially consumed browse page, and an
atomic application-owned commit boundary. M004 implements the SQLite adapter
that durably commits daily-crawl state and selected candidate slugs through that
boundary. M005 implements the application-owned durable job queue contract and
its SQLite adapter: stable job deduplication, explicit claims and leases,
bounded retries, terminal states, stale-claim rejection, reopen persistence,
execution-attempt history, monotonic clock transitions, and non-reusable claim
tokens. M006 adds the bounded in-process hourly scheduler, durable dispatcher,
and typed handler lifecycle. M007 replaces its source placeholder with an async
discovery handler: the exact durable hourly reference derives a UTC crawl day,
the adapter requests New Releases first and newest-first browse later, and the
existing SQLite daily-crawl commit succeeds before the durable job does.
Fixture-only tests cover those paths; they do not call the public source.
M008 adds the offline game-snapshot foundation and atomic SQLite replacement
write for game, platform-score, developer, cover-descriptor, and video-link
data. M009 derives one durable source-ingestion job per selected candidate and
wires the bounded offline scheduler-to-snapshot vertical with fixture-only
coverage. M010 adds the embedded, server-rendered catalogue at `/games` and
`/games/{id}`: it reads only persisted SQLite snapshots, supports
case-insensitive title search, platform filtering, rating sorting, and
SQLite-only similar-game links. M011 adds the fully offline review-to-summary vertical: each
stored refresh keeps bounded critic and user review inputs separate, atomically schedules one
fingerprint-fenced local summary job per kind, and renders persisted likes/dislikes or an explicit
unavailable state on the detail page. The fallback is deterministic and local; it does not use a
provider. M013 adds local delivery readiness: non-dependent liveness, SQLite/schema readiness,
an explicit offline source-work switch for smoke evidence, and a non-root container definition.
M014 adds local-only, privacy-bounded tracing in the sole binary: explicit deterministic human
or structured JSON output, lifecycle/HTTP/queue/source-summary categories, and a direct
source-disabled binary smoke. No telemetry backend is configured.
Runs/run_items, SSE, media, external LLM/provider integration, and an actual deployment remain
unimplemented.

## Baseline architecture

- one virtual Cargo workspace with seven library crates and one binary crate;
- compiler-visible domain, application, storage, web, and worker ownership;
- one deployable binary despite the internal crate boundaries;
- one long-running Tokio process;
- SQLite application storage and durable job queue;
- separate `source`, `media`, and `llm` worker lanes;
- Axum plus Askama server-rendered UI with embedded assets;
- mandatory Metacritic and review summaries before optional YouTube work.

Read the canonical documents before implementation:

- [requirements](docs/requirements.md);
- [architecture spine](ARCHITECTURE.md);
- [workspace decision](docs/decisions/0001-adopt-multi-crate-workspace.md);
- [agent rules](AGENTS.md);
- [AI correspondence policy](docs/ai/README.md).

## Local build and run

The project pins Rust through `mise`.

```bash
mise install
mise run architecture
mise run ci
```

The binary requires these explicit environment variables:

- `GAMEPULSE_DATABASE_PATH`: an absolute writable SQLite file path on persistent local
  storage; it is created and migrated at process startup. Do not use an
  in-memory database for delivery.
- `GAMEPULSE_HTTP_ADDRESS`: an IP socket address and port, such as
  `127.0.0.1:3000` or `0.0.0.0:3000`. Host names are rejected so startup never
  performs address resolution.
- `GAMEPULSE_LOG_FORMAT`: required local log format, exactly `human` for
  time-free, ANSI-free inspection output or `json` for one structured event per
  line. A missing, non-Unicode, or unsupported value stops startup without
  echoing the supplied value.

`GAMEPULSE_SOURCE_WORK_ENABLED` is optional and defaults to `true`. Set it to
`false` only for a local offline smoke or UI inspection: it prevents the source
lane from scheduling or claiming work, but does not alter stored data. Page
requests themselves never fetch catalogue data.

```bash
mkdir -p var
export GAMEPULSE_DATABASE_PATH="$PWD/var/gamepulse.sqlite3"
export GAMEPULSE_HTTP_ADDRESS="127.0.0.1:3000"
export GAMEPULSE_LOG_FORMAT="human"
export GAMEPULSE_SOURCE_WORK_ENABLED="false"
cargo run --locked -p gamepulse
```

### Deterministic local demo

```bash
mise run demo
```

The command checks that `127.0.0.1:3000` is free, builds the existing release
binary offline, seeds deterministic local SQLite fixture data, and starts the
source-disabled UI at [http://127.0.0.1:3000/games](http://127.0.0.1:3000/games).
It uses embedded assets and loopback requests only. Press `Ctrl-C` to stop the
server; the bounded temporary fixture directory is removed on shutdown. If the
port is occupied or loopback binding is unavailable, the command exits without
leaving fixture data behind.

Logging is local-only. It records fixed lifecycle, HTTP method/normalized route
class/process-local request-ID/status/elapsed-time, scheduler, durable-job,
source-stage, optional-cover-category, and review-summary-category fields. It
does not record HTTP bodies, query strings, title searches, review text, URLs,
headers, cookies, credentials, database paths, local paths, or raw errors.
Only the six binary-owned `gamepulse::*` logging targets are admitted; warnings
and errors from dependencies are filtered before either format is rendered.
`GAMEPULSE_SOURCE_WORK_ENABLED=false` is the offline smoke setting: source
clients and source handlers are not composed, so the process makes no source
request. A source-disabled smoke uses a temporary SQLite file outside this
repository, checks `/health/live`, `/health/ready`, and `/games`, gracefully
stops the process, inspects its safe startup/request/shutdown logs, and removes
the temporary data afterwards.

Once the process is listening, `GET /health/live` returns `200 OK` without
opening SQLite or a network connection. `GET /health/ready` returns `200 OK`
only when the configured SQLite file can be reopened read-only and has the
required migrated schema version and structure; otherwise it returns `503 Service
Unavailable` with an empty body. If SQLite cannot initialize, liveness still returns `200` while
readiness and catalogue routes return `503`; neither endpoint schedules jobs or
starts source work.

## Container delivery boundary

`Dockerfile` is a multi-stage, `--locked` build of the existing `gamepulse`
binary. The runtime image uses a non-root user and declares
`/var/lib/gamepulse` as the persistent SQLite mount point; the image contains
no SQLite database. A local image build and run, when separately authorized,
use the same explicit variables:

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

SQLite has one durable writer and GamePulse supports exactly one replica with
one persistent volume claim. Do not scale this image horizontally or add a
database/queue service. Exact deployment namespace, host/TLS route, immutable
image digest, PVC name/class, and production source-work authorization are
handoff TODOs; they are intentionally not inferred here.

The source lane and the opt-in public canary are separate. Enabling ordinary
source work may make Metacritic requests through the normal runtime. The canary
below remains the only deliberate contract probe and is never part of health,
readiness, or an offline smoke.

### Mandatory and optional status

| Area | Status |
| --- | --- |
| Stored catalogue, detail, mandatory review summaries | Implemented with deterministic local coverage |
| Local delivery readiness and container definition | Implemented locally; deployment handoff remains TODO |
| Metacritic live canary | Explicit opt-in only |
| Runs/run_items, SSE, manual trigger, YouTube/media, external LLM | Not implemented |

`mise run ci` checks formatting, Clippy with warnings denied, and all current
tests. The architecture task verifies the exact declared internal Cargo graph
and the eight-target production shape (seven normal libraries plus the sole
binary) against metadata-shaped sabotage rules. Coverage is deferred; targeted
mutation testing begins when meaningful critical behavior exists.

### M038 one-shot evaluator acceptance

The explicit acceptance command is separate from ordinary service startup. It
expects a fresh absolute database path and never deletes a caller file or its
SQLite sidecars. Create and remove the temporary directory yourself:

```bash
acceptance_dir="$(mktemp -d /tmp/gamepulse-acceptance.XXXXXX)"
cargo run --locked -p gamepulse -- acceptance-once \
  --database "$acceptance_dir/gamepulse.sqlite3" \
  --deadline-seconds 180
rm -rf -- "$acceptance_dir"
```

The command defaults to the current mandatory target of 20 only when `--target`
is omitted. `--target 20` is accepted for an explicit template; a missing or
non-numeric explicit value, and any target other than 20, are rejected before
SQLite opens because the domain's atomic daily-selection invariant is exactly
20. It performs exactly one hourly-discovery enqueue, then drains only the mandatory source-ingestion
and local review-summary jobs derived in that fresh database. It starts no
listener, daemon, or repeat scheduler, and it does not retry a failed job.
The deadline is hard and required.

Its stdout is exactly one compact `gamepulse.acceptance.v1` aggregate JSON
report. It includes the terminal outcome, target, selected, attempted,
persisted, complete-video, summary-readiness, safe fixed failure-category
counts, and runtime milliseconds. It intentionally omits a request count:
the normal source port does not expose an exact wire-attempt count. The report
never includes titles, source IDs, review text, payloads, credentials, URLs,
or local paths. Fail-closed operational reports exit `3`; invalid command
arguments exit `2`, and an internal `runtime_failure` report exits `1`.

### M028 opt-in source diagnostic

The repository-owned diagnostic is an integration-test tool, not a second
production binary and not part of ordinary source work. Its fixture mode runs
only local fixtures through the same request budget, parser, and aggregate
reporting path; it makes zero external requests:

```bash
bash scripts/diagnostic_canary.sh fixture
```

The two live modes are ignored by normal tests and require separate explicit
owner authorization before the environment opt-in is set. They use anonymous,
direct HTTPS only, disable retries and redirects, reject a non-JSON or
oversized response, bypass cookies/authentication/browser/proxy state, and
print a structured aggregate report only. They never log or persist payloads,
identities, URLs, headers, or response bodies.

For either live wrapper mode, the wrapper explicitly runs its designated
ignored test entrypoint. Without the exact opt-in it reaches the pre-request
`blocked_environment` path instead: no wire attempt is made, the validated
zero-count aggregate remains on stdout, and the wrapper exits `3`.

Finder mode permits one New Releases request:

```bash
GAMEPULSE_M028_LIVE_DIAGNOSTIC=1 bash scripts/diagnostic_canary.sh finder
```

Review-continuation mode permits at most finder, critic-first-page, and
user-first-page requests for one ephemeral finder candidate. It stops on the
first failure and never follows a continuation or candidate fallback:

```bash
GAMEPULSE_M028_LIVE_DIAGNOSTIC=1 bash scripts/diagnostic_canary.sh review-continuation
```

Terminal verdicts are `fixture_validated`, `contract_ready`, `access_denied`,
`rate_limited`, `source_rejected`, `no_candidate`,
`request_budget_exhausted`, and `blocked_environment`. Only the first two are
positive structural evidence; the others are fail-closed diagnostic results.
`blocked_environment` is the only zero-count report: its exchanges are empty
and it means that the live diagnostic could not safely create or validate its
environment, client, transport, or first request before any wire attempt. It
authorizes no automatic retry. Full behavior and boundaries are recorded in
[`docs/source-contracts/metacritic-direct-http.md`](docs/source-contracts/metacritic-direct-http.md).

M031 makes the output a single validated `gamepulse.diagnostic.v1` aggregate
report. The wrapper accepts only the exact controlled Cargo transcript with one
report: no extra fields, duplicate JSON, noise, reordered or repeated framing,
or inconsistent count, ceiling, exchange order, parser, category, or verdict
combination. Positive reports exit `0`; every schema-valid fail-closed verdict
is still printed as evidence and exits `3`; this includes a valid
zero-count `blocked_environment` report. Invalid output or an internal
wrapper failure prints only `diagnostic command failed` to stderr and exits
`1`. Invalid mode exits `2` with no report. `request_count` is the exact
budget-reserved diagnostic attempt count, is zero only for
`blocked_environment`, and is not a reconstruction of a historical wire count.

## Repository boundary

This repository must remain self-contained for evaluation. Private application
records, recruiter correspondence, salary discussion, credentials, and hidden
agent runtime data do not belong here.

No open-source license is granted by this repository. The code is prepared for
candidate evaluation unless a separate written license is added later.

## Solution cost

- Coverage TODO: the verified totals below cover accepted M003 and M004 only.
  M005-M013, shared pre-instrumentation work, and cost-instrumentation setup
  have not yet been reconciled into a final estimate; README formatting remains
  outside that coverage.
- API-equivalent estimate (not an invoice): $44.95953076.
- Effective tokens: 2,684,859.
- Cache savings: $267.87101184.
- Pricing profile: as of 2026-08-14; [official OpenAI API pricing](https://developers.openai.com/api/docs/pricing).
