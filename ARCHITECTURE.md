# GamePulse Architecture

- Status: adopted for workspace initialization
- Scope: the GamePulse Rust workspace and its runtime
- Last verified: 2026-08-14
- Requirements: [`docs/requirements.md`](docs/requirements.md)

This file records only durable, non-obvious boundaries that contributors or
agents could otherwise implement incompatibly. Code owns discoverable details
such as complete type layouts and internal helper structure.

## Design paradigm

GamePulse is a multi-crate modular monolith. A virtual Cargo workspace provides
compiler-visible ownership boundaries while one binary still runs the web
server, durable scheduler, queue dispatcher, and three logical worker lanes in
one Tokio process. SQLite on persistent storage owns both application state and
durable job execution state.

```mermaid
flowchart LR
    BIN["gamepulse binary<br/>composition root"]
    WEB["gamepulse-web"]
    STORAGE["gamepulse-storage-sqlite"]
    SOURCE["gamepulse-worker-source"]
    MEDIA["gamepulse-worker-media"]
    LLM["gamepulse-worker-llm"]
    APP["gamepulse-application<br/>use cases and ports"]
    DOMAIN["gamepulse-domain<br/>pure policy"]

    BIN --> WEB
    BIN --> STORAGE
    BIN --> SOURCE
    BIN --> MEDIA
    BIN --> LLM
    WEB --> APP
    STORAGE --> APP
    SOURCE --> APP
    MEDIA --> APP
    LLM --> APP
    APP --> DOMAIN

    STORAGE --> DB[("SQLite")]
    SOURCE --> META["Metacritic"]
    MEDIA --> YOUTUBE["YouTube and transcripts"]
    LLM --> MODEL["LLM provider"]
```

## Architecture invariants

### AD-1 — Separate compile-time ownership without splitting deployment

- **Prevents:** infrastructure and process topology growing before the workload
  justifies them.
- **Rule:** the baseline is one virtual Cargo workspace with eight packages,
  seven library crates, one binary crate, one long-running replica, and one
  multithreaded Tokio process. Multiple crates enforce dependency ownership;
  they do not imply multiple services or deployable artifacts.

### AD-2 — Keep delivery adapters thin

- **Prevents:** application behavior being duplicated across HTTP handlers,
  scheduler callbacks, and worker loops.
- **Rule:** `crates/gamepulse/src/main.rs` wires components. Web routes,
  scheduler ticks, and job handlers invoke narrow application use cases. They
  do not own workflow policy.

### AD-3 — Preserve one-way dependencies

- **Prevents:** domain and orchestration code depending on Axum, SQLx, source
  parsing, or a concrete AI provider.
- **Rule:** `gamepulse-application` depends only on `gamepulse-domain` among
  workspace crates. Storage, web, and each worker depend on application and
  domain. The binary composition root depends on every library crate. Reverse
  edges, outer-adapter edges, and worker-to-worker edges are forbidden.

```text
gamepulse binary --------------------------> all library crates
storage / web / each worker --------------> application and domain
application ------------------------------> domain
domain -----------------------------------> no workspace crate
```

### AD-4 — Keep durable work in SQLite

- **Prevents:** queued work disappearing on restart or becoming split between
  in-memory and durable sources of truth.
- **Rule:** SQLite owns jobs, leases, retries, deduplication, and terminal
  execution history. In-memory channels may wake claim loops or broadcast UI
  invalidations only. Execution is at-least-once; writes are idempotent.

### AD-5 — Separate worker lanes by external bottleneck

- **Prevents:** source politeness, media/transcript limits, and LLM budget being
  coupled into one concurrency and retry policy.
- **Rule:** `source`, `media`, and `llm` remain separate crates and logical lanes
  with independent semaphores, timeouts, rate limits, retry ceilings, and
  priorities. Media obtains transcripts but never performs LLM summarization
  synchronously or depends on the LLM worker crate.

### AD-6 — Separate business progress from execution attempts

- **Prevents:** prunable job history becoming the UI source of truth or optional
  YouTube work holding a mandatory run open.
- **Rule:** `runs` and `run_items` own batch and mandatory item progress;
  `summaries` own visible freshness; `jobs` own retryable attempts. A run is
  active only while mandatory discovery, ingestion, critic summary, or user
  summary work is non-terminal. Optional media work never holds it open.

### AD-7 — Hide Metacritic behind a verified source port

- **Prevents:** undocumented endpoint shape, page HTML, or unstable slug rules
  leaking into application logic.
- **Rule:** the first canary verifies list modes, pagination, stable identity,
  details, scores, trailer, genre, and both review kinds. Runtime uses direct
  HTTP when proven. Browser inspection is development evidence, not a baseline
  runtime dependency.

### AD-8 — Keep the UI server-rendered and embedded

- **Prevents:** a separate frontend toolchain and deployment artifact growing
  before client-side complexity exists.
- **Rule:** Axum and Askama own semantic server-rendered pages. Templates,
  migrations, CSS, and minimal JavaScript are embedded in the binary. SSE is an
  invalidation channel over durable database snapshots. WASM is not baseline.

### AD-9 — Keep optional enrichment subordinate to mandatory behavior

- **Prevents:** YouTube quota, missing transcripts, or optional LLM work causing
  mandatory ingestion to fail or starve.
- **Rule:** mandatory review summaries have higher priority than optional media
  work. A missing transcript is a terminal optional outcome. Optional failures
  remain visible but cannot fail mandatory game processing.

### AD-10 — Preserve the evaluation and secret boundary

- **Prevents:** credentials, hidden system data, or private career context
  leaking through the repository or AI transcript.
- **Rule:** secrets are environment-only. Export complete visible project
  prompts and responses, but never credentials, hidden system instructions,
  private chain-of-thought, or unrelated HR context.

## Workspace ownership

| Crate | Owns | May depend on |
| --- | --- | --- |
| `gamepulse-domain` | Entities, value objects, state transitions, pure policy | No workspace crate |
| `gamepulse-application` | Use cases, inbound and outbound ports, orchestration policy | Domain |
| `gamepulse-storage-sqlite` | SQLite repositories, migrations, durable `JobStore` | Application, domain |
| `gamepulse-worker-source` | Scheduler, Metacritic client, source-lane handlers | Application, domain |
| `gamepulse-worker-media` | YouTube search, transcript acquisition, media-lane handlers | Application, domain |
| `gamepulse-worker-llm` | Model client, prompt execution, LLM-lane handlers | Application, domain |
| `gamepulse-web` | Axum routes, Askama templates, SSE, embedded assets | Application, domain |
| `gamepulse` | Configuration and composition root; the only binary | Every library crate |

Worker and delivery crates communicate through application-owned ports and
durable state. They never call each other directly. Concrete storage is injected
by the binary rather than imported by workers or web.

## Durable state ownership

| State | Owner | Purpose |
| --- | --- | --- |
| Games and platform scores | `games`, `game_platform_scores` | Current source data |
| Review source snapshots | `review_snapshots` | Summary inputs and hashes |
| Summary freshness and output | `summaries` | UI-visible LLM state |
| Daily crawl progression | `crawl_days` | New Releases then browse ordering |
| Batch progress | `runs`, `run_items` | Mandatory business lifecycle |
| Execution attempts | `jobs` | Claims, leases, retries, errors |
| Optional media state | `youtube_enrichments` | Video and transcript lifecycle |

## Architecture fitness

The executable architecture gate makes only bounded claims that Cargo can
support reliably:

- the workspace contains exactly the eight adopted packages;
- production targets exactly match seven named normal libraries (`kind = lib`,
  `crate_types = lib`) plus the sole `gamepulse` binary (`kind = bin`,
  `crate_types = bin`); no extra production binary or library target is allowed;
- the complete declared internal Cargo dependency graph, including optional and
  non-normal dependency kinds, matches the adopted allowlist;
- metadata-shaped sabotage fixtures reject a forbidden worker-to-worker edge,
  a second binary, a missing member, an extra ninth member, an extra library
  target, and a retyped library target.

`mise run architecture` runs this gate against live `cargo metadata`. The test
normalizes workspace package identities, manifest dependency paths, and target
metadata; it does not parse Rust source text or use feature-resolved graph
nodes as its dependency source.
Every new architecture rule must state its exact claim and add positive and
negative sabotage evidence before becoming a gate.

Compiler visibility, focused behavior tests, integration tests, mutation tests,
and independent review cover other invariants. A green Cargo graph does not
prove complete architecture conformance and must not be reported as such.

## Current conformance

The repository currently contains the eight-package workspace harness, one
compileable binary shell, and a sabotage-tested Cargo graph gate. No source,
storage, queue, worker, LLM, media, web, or deployment behavior is implemented.
Passing scaffold CI proves only the bounded workspace claims above, not product
behavior or complete architecture conformance.

## Revisit conditions

Revisit crate ownership, process topology, or queue storage only when evidence
shows one of:

- a crate has no durable ownership boundary after three implementation
  milestones;
- a required use case cannot cross the adopted ports without cyclic ownership;
- multiple replicas are required;
- web and workers need independent deployment or scaling;
- SQLite contention persists after WAL, short transactions, indexes, and
  bounded concurrency;
- accepted local transcription needs material CPU, GPU, or memory isolation;
- a source requires a heavyweight browser runtime;
- queue latency violates the evaluator-visible service contract.

Any accepted crate merge, split, new internal edge, or runtime-topology change
updates this spine, the decision record, and architecture sabotage cases before
implementation.
