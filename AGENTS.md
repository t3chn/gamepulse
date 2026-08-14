# GamePulse Agent Rules

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
