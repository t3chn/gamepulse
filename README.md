# GamePulse

GamePulse is a Rust take-home project for durable game discovery, review
summarization, and evaluator-visible worker progress.

## Status

The repository is initialized as an eight-package Cargo workspace with an
adopted architecture and local verification harness. Product behavior is not
implemented yet. The first product milestone is the bounded Metacritic source
canary described in `ARCHITECTURE.md`.

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

## Repository boundary

This repository must remain self-contained for evaluation. Private application
records, recruiter correspondence, salary discussion, credentials, and hidden
agent runtime data do not belong here.

No open-source license is granted by this repository. The code is prepared for
candidate evaluation unless a separate written license is added later.
