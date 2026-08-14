# M002 Sanitized Evidence Receipt

- Scope: completed M002 public-source investigation and one opt-in live canary
- Evidence date: 2026-08-14
- Source of this receipt: the completed visible M002 implementation report and
  its compact local command results

## Observed

- The report records 29 anonymous discovery application invocations and one
  deliberate live-canary application invocation.
- The live canary reported a New Releases response with 20 listed items, a
  numeric total, and a continuation link.
- The recorded contract covers the two list modes, offset-based continuation,
  numeric product identity plus slug, product detail fields, separately fetched
  platform Userscore, and distinct critic and user review paths.
- No raw third-party payload, review quote, cookie, authenticated session, or
  browser runtime was retained in the project evidence.

## Not established by the completed run

- The executed canary revision disabled redirects but did not explicitly disable
  reqwest protocol-NACK retries. Therefore 30 is an application-invocation
  count, not independently proven wire-attempt count.
- No versioned public API, published rate limit, permanent schema guarantee, or
  rendered cover-image CDN URL is established.

The correction pass makes retry policy and structural validation deterministic
without issuing a new source request.
