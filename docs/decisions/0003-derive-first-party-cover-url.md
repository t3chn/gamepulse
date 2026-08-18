# ADR 0003: Derive validated first-party cover URLs

## Context

The assignment requires a cover image. The optional public-page `og:image`
enrichment produced no public cover URLs for the live 20-game catalogue, while
the structured product response retained complete Metacritic image descriptors.
A bounded canary confirmed that the observed descriptor maps to a first-party
`www.metacritic.com/a/img/catalog/provider/...` JPEG response.

## Options

1. Keep only HTML enrichment. Rejected because the demonstrated catalogue has
   no covers and therefore misses a mandatory field.
2. Derive the observed first-party URL from a strictly validated descriptor.
   Selected because it uses already fetched structured data, needs no new
   runtime request, and is easy to disable.
3. Download one bounded asset through an explicit operator command. Selected
   after the deployed database showed that a persisted URL alone leaves old
   snapshots without renderable covers and leaks the upstream URL into HTML.

## Decision

The source adapter may derive a public cover URL only when all of these hold:

- bucket type is exactly `catalog`;
- image kind is exactly `cardImage`;
- bucket path begins with `/provider/`;
- bucket path ends with the separately supplied filename;
- path and filename contain no traversal, query, fragment, backslash, or
  separator ambiguity;
- the resulting URL passes the existing exact HTTPS `www.metacritic.com`
  validation.

The derived URL remains source-adapter data. Optional HTML enrichment may
replace it with another validated first-party URL, but a missing HTML result
must not erase it. Web reads never derive URLs or fetch upstream images.

`cover-backfill` is the sole acquisition path for existing records. It accepts
an existing absolute SQLite path and a hard limit of 1–20, resolves only the
validated descriptor shape, rejects percent-encoded or ambiguous path segments,
makes no redirect or retry, accepts only JPEG/PNG/WebP with a matching file
signature and a 2 MiB cap, and writes bytes to `game_cover_assets`. The
application-owned coordinator, rather than the binary entrypoint, owns
candidate selection, source outcomes, conditional persistence, reporting, and
exit policy. Each asset is bound to a versioned descriptor fingerprint; a
snapshot replacement invalidates mismatched bytes and a stale fetch cannot
replace the current descriptor. It never deletes game data or starts the
service. The server-rendered pages refer only to `/games/{id}/cover`; that
route serves the persisted bytes and allowlisted content type without exposing
a source URL or descriptor in HTML. Repeat only while the preceding aggregate
report proves a stored asset; stop at zero progress, no candidates, or failure.

The versioned aggregate report records an `unavailable` total and only bounded
`unavailable_reasons`: descriptor rejection, unexpected HTTP status class,
unsupported or missing content type, signature mismatch, and invalid body. The
reason-counter sum is the top-level unavailable total. It contains no status
code, source identity, descriptor, URL, header, body, or per-item record. These
counters are diagnostic evidence, not permission to retry, widen the source
boundary, or make another run.

## Rollback

Stop running `cover-backfill`. Existing local assets remain durable and can be
removed only through a separately authorized data migration.

## Revisit condition

Revisit if Metacritic changes the descriptor shape or the derived path stops
returning an image for more than four games in one completed 20-game run.
