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
- The public HTML routes remain evidence surfaces rather than the runtime data
  source: `/game/`, `/browse/game/all/all/all-time/new/`, and
  `/game/{slug}/`.

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
