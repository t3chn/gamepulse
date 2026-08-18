# Project AI Correspondence

This directory is reserved for evaluator-facing records of AI-assisted project
work.

## Inclusion contract

Include complete visible project-specific prompts and responses that influence
requirements, architecture, implementation, testing, review, or delivery.
Preserve chronology and identify the originating tool or model when known.

## Exclusion contract

Never include:

- credentials, API keys, tokens, cookies, or private environment values;
- hidden system or developer instructions;
- private chain-of-thought or internal reasoning records;
- unrelated HR, recruiter, salary, or personal context;
- unrelated tool output or absolute local paths that do not help evaluation.

Do not label a filtered export as raw JSONL. Use Markdown or a clearly labeled
sanitized export when raw runtime records contain excluded material.

The technical planning that predates this repository is disclosed in
[`prehistory.md`](prehistory.md).

## Included sanitized exports

The [`transcripts`](transcripts/) directory contains complete visible prompts
and responses recovered from retained native evaluator-facing project tasks.
It covers:

- M004-M007 and M009-M015, including replacement tasks M012a and M014b;
- M019-M033;
- M034R-M059, including replacement tasks M038R, M046R, and M051R.

The exact task roles available for each milestone are represented by the file
names. Some later milestones were verification or diagnostic slices rather
than implementation/review pairs.

Each file is labeled as sanitized rather than raw. Local roots, runtime task
identifiers, private routing metadata, hidden instructions, and unrelated
application context are excluded. Private control-plane conversations are not
evaluator-facing project correspondence and are not exported.

## Known gaps

No transcript is published for M008, M016-M018, or M034:

- M016 and M017 used internal delegated workers rather than native
  evaluator-facing project tasks, so they cannot be represented as equivalent
  project-task correspondence;
- M018 contained only private control work;
- M034 was a recorded route failure and was replaced by the native M034R
  project tasks;
- no retained native evaluator-facing source was available for M008.

These gaps are disclosed rather than reconstructed from private control
conversation or presented as correspondence that did not occur in a native
project task.
