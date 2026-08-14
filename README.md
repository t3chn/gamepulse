# GamePulse

GamePulse is a Rust take-home project for durable game discovery, review
summarization, and evaluator-visible worker progress.

## Status

The repository has an eight-package Cargo workspace, architecture harness, a
bounded direct-HTTP Metacritic source-contract canary in
`gamepulse-worker-source`, and a deterministic M003 daily-crawl selection seam.
M003 plans New Releases first, then newest-first browse progression, with
numeric-ID daily uniqueness, replay of a partially consumed browse page, and an
atomic application-owned commit boundary.
Scheduling, durable ingestion and persistence, summaries, and the web UI are
not implemented yet.

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

## Development

The project pins Rust through `mise`.

```bash
mise install
mise run architecture
mise run ci
cargo run --locked -p gamepulse
```

`mise run ci` checks formatting, Clippy with warnings denied, and all current
tests. The architecture task verifies the exact declared internal Cargo graph
and the eight-target production shape (seven normal libraries plus the sole
binary) against metadata-shaped sabotage rules. Coverage is deferred; targeted
mutation testing begins when meaningful critical behavior exists.

### Opt-in public source canary

The live canary is ignored by normal tests. It performs exactly one anonymous
public request to the verified New Releases finder endpoint and prints only
structural counts:

```bash
METACRITIC_LIVE_CANARY=1 cargo test --locked -p gamepulse-worker-source \
  --test live_canary live_new_releases_contract_canary -- --ignored --exact --nocapture
```

Run it deliberately and only within the request ceiling documented in
[`docs/source-contracts/metacritic-direct-http.md`](docs/source-contracts/metacritic-direct-http.md).

## Repository boundary

This repository must remain self-contained for evaluation. Private application
records, recruiter correspondence, salary discussion, credentials, and hidden
agent runtime data do not belong here.

No open-source license is granted by this repository. The code is prepared for
candidate evaluation unless a separate written license is added later.
