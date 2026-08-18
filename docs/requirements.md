# GamePulse Requirements

Status: captured from the received take-home assignment on 2026-08-14

## Mandatory processing

The service runs once per hour and visits the Metacritic Games section. Each
durable run owns a target of 20 successful, eligible, unique games and advances
one stable candidate at a time until that target succeeds. A candidate missing
the mandatory video link is a terminal candidate rejection: it is not stored as
a complete game, consumes no target quota, and is never retried within the run
or after restart. The same run continues to later unique candidates. If bounded
newest-first discovery is exhausted or the durable run deadline passes before
20 successes, the run fails closed and is never reported as successful.

Daily source sequence:

1. A new day starts its run at the Games page New Releases section.
2. Later run cycles use SEE ALL sorted by newest and may advance through browse
   pages, with at most eight durable SEE ALL pages across restart.
3. Each new day starts the sequence again from New Releases.

Transient source failures use durable, deterministic retry eligibility and
source-lane pacing. A process restart must not make a retry or another
source-lane claim eligible earlier than its persisted time.

Required game information:

- title;
- cover image;
- every available platform with Metascore and Userscore;
- developer;
- description;
- video link.

The service reads reviews and creates two separate short summaries:

- what critics like and dislike;
- what users like and dislike.

Review-derived information may be refreshed when a game is processed again.

## Web interface

The service provides:

- a list of loaded games with compact information;
- a complete game detail page;
- platform filtering;
- title search;
- rating sorting;
- similar games selected only from games already stored in the database;
- navigation from a similar-game result to its detail page.

## Optional enrichment 1

For each game:

1. find YouTube letsplays;
2. choose the most popular eligible letsplay;
3. convert the blogger speech to text;
4. summarize the transcript;
5. attach the conclusion and video link to the game.

## Optional enrichment 2

The web interface may expose:

- realtime worker status and processed-record counters;
- a manual processing trigger.

## Delivery requirements

The result includes:

- a repository link;
- a live service link;
- complete visible project AI prompts and responses, with raw JSONL accepted as
  an optional transport format.

## Local delivery readiness

The single binary exposes a dependency-free liveness endpoint and a separate
readiness endpoint. Liveness must not contact a source or require SQLite.
Readiness may inspect only the configured SQLite database and its required
migrations; it must not schedule durable jobs or begin source work. A failed
readiness check returns a non-success response without disclosing an
operational path or database error. An unavailable SQLite database must not
prevent liveness from starting; catalogue delivery and worker execution remain
unavailable until readiness succeeds.

The delivery container runs the existing sole binary as a non-root user. Its
SQLite file is supplied through persistent storage outside the image. SQLite
supports exactly one application replica; a multi-replica shape is out of
scope until the architecture revisit condition is met.

## Adopted interpretations

These are architecture decisions rather than quoted assignment facts:

- the mandatory Metacritic trailer/video link is separate from the optional
  YouTube letsplay link;
- daily uniqueness is based on a verified stable source identity, not a page
  cursor;
- rating sort uses the selected platform Metascore, or the maximum Metascore
  when no platform filter is active;
- similar-game scoring is deterministic and degrades safely when optional genre
  data is missing;
- optional YouTube work never holds a mandatory run open.

## Material unknowns

- YouTube and transcript provider;
- LLM provider and budget;
- assignment deadline;
- deployment target and persistent-storage behavior;
- accepted repository visibility and AI transcript format.

The bounded current Metacritic direct-HTTP contract is recorded in
[`source-contracts/metacritic-direct-http.md`](source-contracts/metacritic-direct-http.md).
It remains a monitored public-source dependency rather than a permanent API
guarantee.

## Evaluator acceptance cycle

The sole binary provides one explicit opt-in, one-shot evaluator acceptance
command. It accepts a fresh caller-selected SQLite path, defaults to the
mandatory 20-game target, performs one persistence cycle through the ordinary
application ports and worker lanes, and exits with one aggregate-safe machine
report. It never starts the HTTP server, daemon, or hourly loop.

The command schedules discovery once only. It does not create a second cycle,
retry a failed job, or wait for optional work. It waits only for the mandatory
source-ingestion and review-summary jobs created by its fresh durable run,
subject to an explicit hard deadline. Success requires one succeeded run with
exactly the requested current mandatory target of stored records with video
links and both persisted review summaries ready. The command neither removes nor overwrites a caller database;
operators provide a fresh temporary path and remove it themselves after
inspection.
