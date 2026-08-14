# Adopt a Multi-Crate Workspace

- **Status:** adopted
- **Date:** 2026-08-14
- **Decision owner:** candidate

## Decision question

How should GamePulse enforce clean dependency direction between domain,
application, storage, delivery, and three independent worker lanes while still
shipping one lightweight binary?

## Constraints and verified facts

- The take-home must remain easy to build and run as one binary and one process.
- Source, media, and LLM work have different bottlenecks and must not call each
  other directly.
- Application policy must not depend on Axum, SQLx, source parsing, transcript
  acquisition, or a concrete model provider.
- Rust module visibility cannot reliably forbid every sibling-to-sibling edge.
- Source-text architecture scanners can miss qualified paths and macro-expanded
  dependencies, so a green text scan would overstate conformance.
- The repository contains only a compileable shell, making package-topology
  revision cheap and reversible now.

## Options

### 1. Keep one package with internal modules

This minimizes manifests, but dependency direction remains primarily a review
responsibility. Rejected because compiler-enforced clean architecture is now a
primary project constraint.

### 2. Use a coarse workspace with one combined adapter or worker crate

This protects domain and application but leaves worker-to-worker ownership
inside a shared crate. Rejected because source, media, and LLM lanes have
explicitly different policy and failure boundaries.

### 3. Use an eight-package workspace with one binary

Selected. Separate domain, application, SQLite storage, web delivery, and each
worker lane. Keep the deployable composition root as the only binary.

## Decision

Adopt these workspace packages:

- `gamepulse-domain`;
- `gamepulse-application`;
- `gamepulse-storage-sqlite`;
- `gamepulse-worker-source`;
- `gamepulse-worker-media`;
- `gamepulse-worker-llm`;
- `gamepulse-web`;
- `gamepulse` as the only binary and composition root.

The complete internal Cargo edge set is allowlisted. Application depends on
domain. Each outer crate depends on application and domain. The binary depends
on every library crate. No worker-to-worker, adapter-to-adapter, or reverse
layer edge is allowed.

An architecture fitness test reads live `cargo metadata`, normalizes workspace
member identities, manifest dependency paths, and production targets, and
compares them with this exact contract. It checks every declared internal
dependency, including optional, build, and development dependencies, rather
than only feature-resolved edges. The production target set is exactly seven
named normal libraries (`kind = lib`, `crate_types = lib`) and the sole
`gamepulse` binary (`kind = bin`, `crate_types = bin`). Metadata-shaped
negative fixtures prove rejection of a worker-to-worker edge, a second binary,
a missing package, an extra ninth package, an extra library target, and a
retyped library target. The test does not parse Rust source and does not claim
complete architecture conformance.

## Rejected objections

- **Eight manifests add navigation overhead.** Accepted because the crates map
  to durable ownership and failure boundaries rather than arbitrary type groups.
- **Multiple crates imply multiple services.** Rejected: all libraries link into
  the single `gamepulse` binary and run in one process.
- **A source scanner would be smaller.** Rejected because smaller tooling is not
  useful when its semantic claim is unsound.

## Verification and stop condition

The initialization milestone stops when all eight packages compile, Cargo
metadata matches the edge allowlist, sabotage cases pass, workspace CI passes,
and the binary smoke test prints `GamePulse`. No product behavior belongs in
this milestone.

## Rollback

Move library modules back into the binary package, remove the virtual workspace
and Cargo graph test, and restore the previous single-package architecture. This
is cheap only while substantial product behavior has not accumulated.

## Revisit condition

Revisit after three product milestones if a crate has no durable owner boundary,
or immediately if an accepted use case requires a cycle or a new internal edge.
Any merge, split, or edge change updates this decision and sabotage cases before
implementation.
