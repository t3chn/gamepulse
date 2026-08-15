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
