# Metacritic Direct-HTTP Canary Contract

- Status: observed M002 source contract
- Evidence date: 2026-08-14
- Evidence method: the completed M002 report records 29 discovery application
  invocations plus one documented live-canary invocation, with no cookies,
  authenticated session, or browser runtime. The executed M002 revision did not
  explicitly disable reqwest protocol-NACK retries, so those records do not
  establish an exact wire-attempt count.

This document records the bounded contract needed by the source-worker canary.
It is not a promise that Metacritic will keep the endpoints stable. Runtime code
must treat every response as untrusted and fail closed on an unexpected status or
malformed required structure.

## Transport

- Base host: `https://backend.metacritic.com`.
- All verified calls are `GET` requests and return `200` with JSON without an
  authenticated session, cookie, user agent, or other required request header.
- The corrected canary sends `Accept: application/json`, disables redirect
  following, bounds response bodies, rejects invalid UTF-8, and explicitly
  disables reqwest retries.
- The completed live-canary report records 20 New Releases items, a numeric
  total, and a continuation link; it did not retain a source payload.
- The public HTML routes remain evidence surfaces rather than the mandatory
  runtime data source: `/game/`, `/browse/game/all/all/all-time/new/`, and
  `/game/{slug}/`. M012 makes one deliberately narrow exception for optional
  cover enrichment, described below.

## Optional public-HTML cover enrichment (M012)

The source adapter may make at most one `GET` request to
`https://www.metacritic.com/game/{slug}/` for a game-ingestion attempt. This
request is never made by catalogue or detail rendering and is never used to
probe, derive, download, proxy, or persist CDN/image bytes.

- The separate HTML client has one in-flight gate, a bounded timeout and body
  cap, redirects disabled, and retries disabled. A competing source attempt
  skips cover enrichment rather than waits or opens another HTML request. The
  optional future starts beside mandatory source work, but is dropped when that
  work settles first; it never consumes the mandatory job lease.
- The response is optional and untrusted. It may yield a cover URL only when
  exactly one effective `meta` declaration in HTML data context names `og:image`
  and its content is non-empty,
  within the configured URL bound, parses as HTTPS, and has exact host
  `www.metacritic.com`. Attribute character references decode once before
  validation. Missing, duplicate, malformed, oversized, non-HTTPS,
  credentialed, non-default-port, or other-host values fail closed to no public
  cover URL.
- HTTP `403` and `429` open the in-process circuit from headers before any body
  validation or read. Challenge-like successfully read HTML also opens it.
  Until process restart, the circuit prevents every further optional HTML
  attempt. Other optional transport, status, content-type, size, body-read,
  decode, parse, or validation failures also store no URL.
- The accepted URL is persisted atomically with the ordinary game snapshot.
  Optional HTML failure never changes mandatory detail, score, review, queue,
  daily-selection, or review-summary outcomes.

The current fixed 20-item discovery selection is not an observable completed
ingestion batch at this boundary. Therefore M012 does not add `runs` or
`run_items` for the proposed 20% safeguard. Revisit batch-level source cover
enrichment disablement only when a completed fixed 20-item batch is observable;
then disable after more than four parse-or-validation failures in that batch.

## Discovery lists

The first daily source is the `New Releases` carousel rendered on `/game/`:

```text
GET /finder/metacritic/web
    ?componentName=new-releases-carousel
    &componentDisplayName=Newly+Released
    &componentType=ProductList
    &sortBy=-releaseDate
    &metaScoreMin=1
    &offset=0
    &limit=20
    &mcoTypeId=13
```

The later `SEE ALL` route is `/browse/game/all/all/all-time/new/`. Its verified
direct list shape is the same finder endpoint with the required query keys
`sortBy=-releaseDate`, `mcoTypeId=13`, `offset`, and `limit=24`; the browser
page maps to the same newest-first list contract. Its next page advances from
`offset=0` to `offset=24`.

`data.items` is an array and `data.totalResults` is numeric. Each observed item
has a numeric `id`, a non-empty `slug`, a title, release date, optional
`criticScoreSummary.score`, and optional `userScore.score`. The stable identity
rule for this milestone is the numeric product `id`; a slug is a transport path
component and must not be used as the daily-deduplication identity.

Continuation is explicit. `links.next.href` carries the next `offset` and
preserves `limit`; the completed M002 report recorded the `offset=20` to
`offset=40` observation. A missing `links.next` is terminal. The corrected
parser rejects a present continuation unless its backend host, endpoint path,
offset, limit, and total-result boundary match the request context.

## Game detail and scores

For a list-item slug, the detail contract is:

```text
GET /games/metacritic/{slug}/web
    ?componentName=product
    &componentDisplayName=Product
    &componentType=Product
```

The observed `data.item` shape supplies numeric `id`, `slug`, title,
description, image descriptors (`bucketPath`, `bucketType`, `filename`,
`typeName`), genres, platform descriptors, production companies, and an
optional Metacritic-hosted `video` with `embedUrl` and `manifestUrl`. The
cover candidate is an image whose `typeName` is `cardImage`; M002 keeps its
descriptor and does not fabricate a rendered CDN URL.
Platform entries use a numeric `id`, `slug`, release date, and optional
`criticScoreSummary.score`. Developer names are the `production.companies`
entries whose `typeName` is `Developer`.

An absent `video` remains structurally valid source data. M035 applies the
separate assignment eligibility rule at mandatory source-ingestion time: it
rejects such a detail before persistence under the existing aggregate-only
`other_mandatory_stage` category. This does not change direct-HTTP parsing or
expose source values through diagnostics.

The endpoint does not attach every platform's Userscore to its platform array.
Fetch it separately for each platform slug:

```text
GET /reviews/metacritic/user/games/{slug}/platform/{platform-slug}/stats/web
    ?componentName=user-score-summary
    &componentDisplayName=User+Score+Summary
    &componentType=MetaScoreSummary
```

This response has `data.item.score` and `data.item.reviewCount`; the observed
`links.self.href` reflects the requested platform. Missing or null score data
degrades to an explicit unavailable score. A non-numeric or out-of-range score
is rejected.

## Distinct review inputs

Critic and user reviews are distinct paths and must remain distinct source kinds:

```text
GET /reviews/metacritic/critic/games/{slug}/web?offset={offset}&limit={limit}&sort=date&...
GET /reviews/metacritic/user/games/{slug}/web?offset={offset}&limit={limit}&orderBy=score&orderType=desc&...
```

Both observed response shapes contain `data.items`, numeric `data.totalResults`,
and continuation links. Review records expose scores, authors, dates, URLs, and
quote fields; a critic record cannot be assumed to have a stable review ID.
M002 records only structural availability and never writes review text to
fixtures, logs, or tracked evidence.

M011's offline fixture vertical requests only `offset=0` with `limit=20` for
each kind and never follows a review continuation. It maps bounded synthetic
excerpt fields as untrusted input; critic and user data must not be combined.

The critic endpoint has one first-page compatibility rule for the observed
backend clamp. When that exact M011 critic request returns fewer than 20 parsed
items, its continuation is accepted only when the continuation remains on the
exact critic backend path and establishes one effective page size: it has one
positive `limit` smaller than 20, its `offset` equals the requested offset plus
that limit, and that limit equals the parsed-item count. The normal host,
scheme, duplicate-query-key, arithmetic-overflow, and `totalResults` boundary
checks still apply. Thus an `offset=0&limit=20` request with ten parsed items
and `totalResults=12` can accept only `offset=10&limit=10`. This exception does
not apply to finder/list continuations, user reviews, later critic pages, or a
review continuation that M011 would follow.

## M016 safe terminal diagnostics

When a mandatory game-ingestion attempt fails, its durable handler failure and
the binary-owned source event carry only one fixed category. A review-page
parser rejection of a continuation is `review_continuation_link`; every other
mandatory-path failure is `other_mandatory_stage`. The categories contain no
source-derived material and are the only source-ingestion failure values that
the observability boundary may emit.

M016 changes no continuation acceptance rule. M017 adds one separately
authorized review-only terminal normalization: a present `links.next` object
whose `href` field is absent is terminal only when the requested offset plus
the parsed review-item count equals `data.totalResults` exactly, using checked
arithmetic. M034R extends that same normalization to an `href: null` field
after the bounded live diagnostic observed that structural shape. A
non-exhausted review placeholder remains invalid. A missing `links.next`
retains its established terminal meaning, while explicit `links.next: null`
remains invalid. Finder/list continuations do not receive the normalization.
The M015 critic first-page effective-page-size exception remains narrow, and
every non-empty review or listing continuation still requires the existing
exact host, path, duplicate-query-key, positive-limit, progression, overflow,
and total-boundary validation.

## M028 aggregate-only diagnostic canary

M028 adds a repository-owned integration-test diagnostic, not a runtime path
or a second production binary. Its unignored fixture command uses only local
fixtures and makes zero network requests. It drives the committed listing and
review parsers through the same process-local request budget and aggregate
reporting path that the two ignored live modes use.

The opt-in finder mode permits one New Releases GET. The opt-in
review-continuation mode permits no more than three GETs total: finder, critic
first page, and user first page for the first finder candidate held only in
process memory. Both modes are unavailable unless a later owner explicitly
authorizes the run and sets the documented opt-in. A non-200 response,
non-JSON content type, invalid UTF-8, body-cap failure, malformed JSON,
parser rejection, absent candidate, or exhausted budget stops the sequence;
there are no retries, redirects, fallback candidates, second sequences, or
continuation requests.

Each live request is constructed and rechecked against the exact HTTPS backend
host, expected path, complete query contract, default port, and absent user
info or fragment before it is sent. The isolated client sets only
`Accept: application/json`, disables retries, redirects, and proxy use, has a
bounded timeout and body, and does not enable cookies, authentication, browser
state, HTML, media, CDN, YouTube, or LLM traffic.

Its sole output is the M031 serialized aggregate report described below. It
has no raw status, payload, review text, source identity, URL, header, cookie,
credential, response body, or local path.

## M031 validated aggregate report and process exit contract

Every normal terminal diagnostic result is one minified JSON object conforming
to `gamepulse.diagnostic.v1`. The wrapper accepts exactly one such object and
prints that unchanged object as its complete stdout. It rejects any missing,
malformed, duplicate, noisy, privacy-unsafe, or semantically inconsistent
output. The fixture command remains local-only; it is the only command used by
the repository's deterministic diagnostic tests.

The v1 top-level object has exactly these fields and no others:

| Field | Contract |
| --- | --- |
| `schema_version` | Exact string `gamepulse.diagnostic.v1`. |
| `mode` | `fixture`, `finder`, or `review_continuation`; it must exactly match the wrapper mode. |
| `request_count` | Exact count of budget-reserved diagnostic request attempts. It equals the number of exchange records and cannot exceed `request_ceiling`. It is zero only for `blocked_environment`; every other verdict has a positive count. It does not retroactively assert an exact wire count. |
| `request_ceiling` | Exact integer `1` for finder and `3` for fixture or review-continuation. |
| `terminal_verdict` | One allowed verdict listed below. |
| `exchanges` | An ordered array with exactly `request_count` aggregate-only records. |

Every exchange has exactly the fixed request kind (`finder`, then
`critic_review`, then `user_review` as far as the count reaches), fixed status
category, content-type/UTF-8/JSON booleans, non-negative integer item count,
numeric-total boolean, continuation and `href` presence kinds, all seven
boolean link checks, parser outcome, and fixed safe category. Unknown fields at
any level are invalid; this intentionally excludes source-derived and
path-bearing data.

The following is the complete exchange truth table. Any combination not shown
is invalid. `all false` means every link-check boolean is `false`; `all true`
means every link-check boolean is `true`; `diagnostic evidence` means typed
boolean evidence which cannot make the parser accepted.

| Parser outcome | Status | Structural booleans | Continuation / href | Link checks | Safe category | Request kind |
| --- | --- | --- | --- | --- | --- | --- |
| `accepted` | `ok` | all four are `true` | `missing` / `not_applicable` | all false | `other_mandatory_stage` | any |
| `accepted` | `ok` | all four are `true` | `object` / `missing` | all false | `other_mandatory_stage` | review only |
| `accepted` | `ok` | all four are `true` | `object` / `string` | all true | `other_mandatory_stage` | any |
| `rejected` | non-`ok`, never `not_attempted` | UTF-8, JSON, and numeric-total are `false`; expected-content-type is boolean evidence | `not_checked` / `not_applicable` | all false | `other_mandatory_stage` | any |
| `rejected` | `ok` | JSON and numeric-total are `false`; expected-content-type and UTF-8 are boolean evidence; item count is zero | `not_checked` / `not_applicable` | all false | `other_mandatory_stage` | any |
| `rejected` | `ok` | expected-content-type, UTF-8, and JSON are `true`; numeric-total is boolean evidence | `missing`, `null`, or `other` / `not_applicable` | all false | `other_mandatory_stage` | any |
| `rejected` | `ok` | expected-content-type, UTF-8, and JSON are `true`; numeric-total is boolean evidence | `object` / `missing`, `null`, or `other` | all false | `other_mandatory_stage` | any |
| `rejected` | `ok` | expected-content-type, UTF-8, JSON, and numeric-total are `true` | `object` / any href kind | diagnostic evidence | `review_continuation_link` | review only |
| `rejected` | `ok` | expected-content-type, UTF-8, and JSON are `true`; numeric-total is boolean evidence | `object` / `string` | diagnostic evidence | `other_mandatory_stage` | any |

In particular, `accepted` with `not_checked`, a non-review accepted
`object`/`missing` continuation, a rejected `not_attempted` exchange, and a
finder `review_continuation_link` category are impossible. The producer and
wrapper enforce this same table.

The semantic terminal rules are also part of the schema: positive
`fixture_validated` is exactly the three local fixture exchanges, while
positive `contract_ready` is exactly one accepted finder exchange or three
accepted review-continuation exchanges, in each case with a non-empty finder
aggregate. `access_denied` and `rate_limited` end at their first rejected
exchange, respectively with `forbidden` and `rate_limited` status.
`source_rejected` ends at its first rejected exchange with only `ok` or
`other` status. `no_candidate` is one accepted empty finder exchange; and
`request_budget_exhausted` has a full ceiling-sized accepted prefix.
`blocked_environment` is the sole zero-attempt terminal verdict: it has exactly
`request_count: 0`, an empty `exchanges` array, and the exact mode ceiling. It
means that the live diagnostic could not create or validate its isolated
environment, client, transport, or first request before any wire attempt. It
contains no status, content, parser, source, or exchange evidence and never
authorizes an automatic retry. After a counted first attempt, transport, status,
body, and parser failures remain ordinary nonzero aggregate outcomes and must
not become `blocked_environment`.
`not_attempted` is never emitted in a report. This makes a fail-closed result
parseable evidence without making it positive evidence.

Wrapper exit codes are deliberately separate from the underlying test process:

| Exit | Meaning | stdout |
| --- | --- | --- |
| `0` | Valid `fixture_validated` or `contract_ready` report. | The one validated report. |
| `3` | Valid fail-closed `access_denied`, `rate_limited`, `source_rejected`, `no_candidate`, `request_budget_exhausted`, or `blocked_environment` report. | The one validated report. |
| `1` | Internal/validation failure, including non-zero underlying process status, unavailable validator, invalid report, duplicate JSON, or noise. | Empty; stderr is exactly `diagnostic command failed`. |
| `2` | Invalid wrapper mode. | No report. |

The wrapper accepts one complete, exact transcript only: one leading empty
line, `running 1 test`, one single-line JSON report, `.`, the Cargo success
summary, and one trailing empty line.
The report is the only JSON object. Reordered, repeated, missing, or additional
framing; source noise; duplicate JSON keys; and every bad report shape are
invalid. Wrapper setup, redirection, and cleanup errors use the same
report-free safe failure path; cleanup is quiet and best-effort.

## M033 pre-request blocked-environment reporting

The two opt-in live test entrypoints never panic or expose configuration details
when the opt-in environment is absent or invalid, isolated client/transport
creation fails, or the first request cannot be constructed and validated. They
emit exactly one schema-valid `blocked_environment` aggregate instead. The
wrapper passes the test-harness `--ignored` flag only for its two live modes,
so those designated entrypoints actually run; fixture mode is unchanged. It
validates and leaves the aggregate unchanged on stdout, then exits `3`. A
missing, duplicate, malformed, noisy, privacy-unsafe, schema-invalid,
or semantically impossible report — including a zero-count report with any
other verdict or exchanges — remains report-free, fixed-safe-stderr-only exit
`1` behavior. Build or test-harness failure before the live entrypoint likewise
has no trusted report.

`blocked_environment` is evidence of a local diagnostic precondition failure,
not source evidence and not retry authority. Operators must investigate or
explicitly rerun under a separate owner authorization; the wrapper and
diagnostic perform no automatic retry.

M030 evidence is preserved without reinterpretation: exactly one
review-continuation command invocation occurred, with no retry; its process
exit status was `0`; no valid aggregate report supplied a numeric request count
and trustworthy parser/structural fields; the exact wire count remains UNKNOWN
within `0..3`; no narrow source/parser mismatch was proven; and repository and
temporary-state cleanup remained clean. M031 does not recreate or fill in that
missing live evidence.

The direct M032 fallback evidence is also preserved without reinterpretation:
the review-continuation command was invoked exactly once, exited `1`, produced
no stdout, and emitted only the fixed safe wrapper stderr. Its exact wire count
remains UNKNOWN within `0..3`; no parser/source mismatch is proven; repository
and temporary cleanup remained clean. M033 does not recreate, refine, or
fabricate any part of that evidence.

## Remaining risks

- The backend is a public site implementation, not a versioned public API.
- Image descriptors require a later, separately verified rendering URL policy;
  M002 preserves the source descriptors rather than fabricating a CDN URL.
- Some games can have no score summary, no video, no developer entry, or no
  continuation. The parser represents those as explicit optional data; later
  ingestion policy must decide whether a required assignment field makes an
  item ineligible.
- The contract has no published rate-limit policy. A scheduler must add its own
  concurrency and retry policy in its dedicated milestone.
- Public HTML is subject to bot protection and has no established stability or
  availability guarantee. M012 treats it as a fail-closed optional enrichment,
  not as a required cover-image contract.
