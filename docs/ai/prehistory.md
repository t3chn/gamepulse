# Technical Planning Prehistory

Architecture planning began in a private application-management task before
this standalone repository existed. That outer task also contains unrelated HR
and personal context, so its raw runtime transcript is not part of this
repository.

All technical decisions that govern implementation are reproduced in
`ARCHITECTURE.md` and `docs/requirements.md`. Before repository initialization,
one isolated `claude-fable-5` architecture review returned `REVISE`. The final
architecture integrated its material findings, including:

- separation of the mandatory Metacritic trailer from optional YouTube media;
- explicit New Releases versus later browse sequencing;
- mandatory run completion independent from optional work;
- durable `run_item` and job state machines;
- separate `source`, `media`, and `llm` logical worker lanes;
- bounded pagination, SQLite claim discipline, summary freshness ownership,
  protected manual runs, and SSE recovery.

From repository initialization onward, project-specific visible AI prompts and
responses should be exported under the inclusion contract in `README.md`.

After initialization, the owner raised compiler-enforced clean architecture to
a primary project constraint and replaced the original single-package choice
with an eight-package Cargo workspace. The adopted change is recorded in
[`0001-adopt-multi-crate-workspace.md`](../decisions/0001-adopt-multi-crate-workspace.md).
The earlier Fable review predates this package-topology revision and must not be
reported as validation of the new Cargo graph.
