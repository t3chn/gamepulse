# ADR 0002: Process-local failure-category observations

## Context

The one-shot evaluator report needs aggregate visibility into a small set of
failure causes, while durable job errors are already part of the queue
compatibility contract. Source-omitted video is not a failure; the historical
fixed observation category remains only for compatibility with existing report
and database shapes.

## Options

- Change durable error values or add SQLite state.
- Infer new categories from persisted error strings.
- Carry a fixed observation category in the in-memory handler failure and
  aggregate it through the runtime report.

## Decision

Use the third option. The observation is one of four fixed categories, is not
serialized or persisted, and is accumulated only for the current one-shot
report. Durable messages, queue settlement, retries, and schema remain
unchanged.

## Rollback

Remove the observation field, runtime aggregate channel, report field, tests,
and this ADR. Existing durable failure messages and queue behavior remain the
rollback reference.

## Revisit trigger

Revisit when evaluator observations must survive process restart or be joined
across more than one process; that would require an explicit durable ownership
decision rather than extending this process-local path.
