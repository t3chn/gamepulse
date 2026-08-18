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
3. Download or proxy image bytes. Rejected because caching, storage,
   attribution, and operational ownership are disproportionate for the
   take-home.

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

The derived URL is persisted with the snapshot. Optional HTML enrichment may
replace it with another validated first-party URL, but a missing HTML result
must not erase it. Web reads never derive URLs.

## Rollback

Remove descriptor derivation and retain the stored descriptor and existing HTML
enrichment. Existing public cover URLs remain ordinary nullable snapshot data.

## Revisit condition

Revisit if Metacritic changes the descriptor shape or the derived path stops
returning an image for more than four games in one completed 20-game run.
