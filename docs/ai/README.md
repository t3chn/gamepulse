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
