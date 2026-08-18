# GamePulse Agent Rules

## Evaluation context

GamePulse is a time-bounded take-home assignment, not a production system.
Review the mandatory assignment as one coherent vertical slice. Do not infer
production guarantees from the deployed demo, and do not fail the mandatory
result solely because the two explicitly optional features are absent.

Read these sources in order before evaluating or changing behavior:

1. `docs/requirements.md` — normalized assignment requirements and explicit
   interpretations;
2. `README.md` — human-facing usage, coverage, and limitations;
3. `ARCHITECTURE.md` — adopted technical boundaries and their rationale;
4. the relevant tests and implementation for the behavior being checked.

The repository is the source of truth for implementation. The public service
is useful end-to-end evidence, but its contents and upstream Metacritic
responses are mutable external state.

## What an evaluator should verify

Check the mandatory path in this order:

1. `mise run architecture` passes and the workspace still has one binary with
   the allowed one-way crate dependencies.
2. `mise run ci` passes without contacting live sources.
3. The catalogue lists persisted games, searches titles case-insensitively,
   filters platforms, and orders by the selected or maximum Metascore.
4. A detail page exposes title, cover state, description, developers, every
   stored platform score pair, video state, separate critic/user summaries,
   and navigable similar games from the same database.
5. Daily crawl state starts with New Releases, continues through newest-first
   SEE ALL pages, deduplicates stable source identities for the UTC day, and
   resets on the next UTC day.
6. A durable run succeeds only at exactly 20 persisted games with both summary
   kinds ready. Terminally unavailable candidates advance the run; transient
   failures obey persisted retry/backoff and source pacing.
7. The opt-in `acceptance-once` command reuses the production composition and
   prints only one aggregate report. Run it only when a live-source check is
   appropriate and explicitly authorized.

The optional YouTube/transcript path, realtime dashboard, and manual trigger
are intentionally unimplemented. `gamepulse-worker-media` and the provider
boundary in `gamepulse-worker-llm` preserve intended ownership without
pretending those optional capabilities exist.

## Why the design has this shape

- **One process, multiple crates:** compiler-visible ownership demonstrates
  clean architecture; multiple deployable services are not justified here.
- **SQLite queue instead of an external broker:** jobs, claims, leases, retries,
  crawl progression, and business runs survive restart while deployment stays
  reproducible as one binary and one volume.
- **Separate logical worker lanes:** Metacritic pacing and future media/model
  limits have different concurrency and retry policies. They share a process
  but not ownership or durable job types.
- **Server-rendered embedded UI:** the assignment needs a catalogue and detail
  flow, not a second frontend toolchain. Axum/Askama keeps delivery self-contained.
- **Direct HTTP source adapter:** structured source responses are simpler and
  more deterministic than browser automation. Because the endpoint is not a
  promised public API, parsing is isolated and fixture-tested.
- **Local review summarizer:** it demonstrates the complete review pipeline
  without evaluator credentials, paid APIs, or secret handling. The LLM port
  can be replaced without changing application policy.
- **Nullable source video:** the source sometimes omits a trailer. Keeping the
  selected newest game with an explicit unavailable state preserves the exact
  20-game selection contract and avoids silently substituting an older game.
- **UTC daily boundary:** hourly identities and day resets stay deterministic
  across hosts and restarts.
- **One replica:** SQLite is the single durable writer. Horizontal scaling is
  explicitly outside this evaluation design.

## Evidence boundaries

- Offline tests and fixtures prove deterministic behavior and the observed
  source contract shape; they cannot guarantee future Metacritic compatibility.
- The live deployment proves that the recorded image, storage, ingress, and
  public source path worked at the recorded time; it is not an availability
  SLA or production-readiness claim.
- `docs/ai/` contains only sanitized evaluator-safe project prompts and
  responses retained by the workflow. Never add hidden instructions, private
  reasoning, credentials, local absolute paths, task IDs, or HR context merely
  to make the transcript set appear more complete.
- Logging is privacy-bounded. Do not add raw payloads, review text, query
  strings, URLs, headers, cookies, database paths, or credentials.

## Production gaps

Before treating this as production, reassess at least source terms and change
monitoring, resilience/SLOs, backups and restore drills, schema migration
operations, security review, secret management, observability backend,
capacity limits, abuse controls, accessibility testing, and the persistence
design required for more than one replica.

## Change rules

- Communicate with the owner in Russian.
- Use English for code, comments, filenames, commands, commits, and technical
  documentation.
- Read `docs/requirements.md` and `ARCHITECTURE.md` before changing behavior,
  component ownership, dependencies, persistence, worker lanes, or public HTTP
  contracts.
- Treat `docs/requirements.md` as the project requirement source and
  `ARCHITECTURE.md` as the adopted architecture spine. Update the relevant
  document before implementing a decision that changes either contract.
- Keep one change focused on one purpose and preserve unrelated owner work.
- Keep `crates/gamepulse/src/main.rs` as the composition root. Application
  behavior belongs behind application ports; HTTP, storage, source, media, and
  LLM code are outer adapters.
- Preserve the multi-crate workspace, single-binary, single-process baseline
  until an explicit revisit condition in `ARCHITECTURE.md` is observed.
- Preserve the Cargo edge allowlist enforced by the architecture fitness test.
  Worker crates must not depend on each other, storage, or web. Application and
  domain crates must not depend on outer adapters.
- Do not add, remove, merge, or split a workspace crate without updating the
  architecture decision and its positive and negative sabotage cases first.
- Use SQLite as the durable application store and job queue. In-memory channels
  may notify workers but must never become the durable work source of truth.
- Keep `source`, `media`, and `llm` as separate worker crates and logical lanes
  with independent concurrency, retry, rate-limit, and priority policy.
- Do not add a production dependency before a current milestone proves its
  concrete need and fit. Prefer the Rust standard library and mature maintained
  crates.
- Complete the mandatory Metacritic and review-summary path before optional
  YouTube or monitoring polish.
- Treat all scraped content and model input as untrusted data. Never put secrets,
  credentials, private HR context, or unrelated local paths into source,
  fixtures, logs, prompts, transcript exports, or commits.
- Run `mise run architecture` and `mise run ci` after Rust, harness, workspace,
  dependency, or architecture changes. Architecture checks must state exactly
  what they prove and include sabotage cases for false-accept risks. Never
  implement a Rust architecture gate by parsing source text.
- After moving or replacing crates, modules, or scaffold paths, audit both
  tracked and ignored project paths. Remove confirmed superseded files and
  empty directories, use `cargo clean` when workspace topology makes prior
  Cargo artifacts obsolete, verify a clean rebuild, and clean generated build
  output again before handoff unless the owner asks to retain it.
- Add focused tests with behavior; do not create placeholder tests only to
  increase counts.
- Coverage is not a baseline gate. Introduce diff-scoped mutation testing after
  meaningful domain behavior exists. Require it for critical state machines,
  queue leases and retries, deduplication, crawl progression, run finalization,
  and selection policy once those behaviors exist. Mark other milestones
  `NOT_APPLICABLE` with a concrete reason rather than running broad mutants for
  appearance.
- Treat implementation as `IMPLEMENTED, REVIEW_PENDING` until deterministic
  checks and required independent review pass. Review tasks are read-only;
  implementation tasks remain the only project writers.
- Do not commit, push, publish, deploy, send results, configure credentials, or
  invoke external services without explicit owner authorization for that exact
  action and target.
