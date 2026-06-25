# rate-limiter

A multi-file feature large enough to exceed `DIRECT_DIFF_BUDGET`, so it should
take the **map-reduce** path (summarize each file, consolidate, then title).
Adds a new token-bucket `RateLimiter` module, registers it in `lib.rs`, and wires
it into `Client::send` so requests acquire a token before being sent.

## Should
- Lead with the new rate limiter / token-bucket capability.
- Optionally note it is integrated into the client's request path.

## Should NOT
- Mention retry/backoff as if it were new (it already existed; only an import and
  a single `acquire()` call were added near it).
- Hallucinate config options or limiter features not in the diff (e.g. per-route
  limits, sliding windows).
