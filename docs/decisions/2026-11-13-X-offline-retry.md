# X-offline-retry: how the error screen tells "no answer" from a server error

Date: 2026-11-13Z. Slice: X-offline-retry. Refines §10.14 (states); no owner number changes.

- **Detection:** `+error.svelte` runs when a load fails with a 5xx. Kit gives the same status whether the server failed or nothing answered, so the screen asks `/healthz`: only a request that fails outright counts as "couldn't reach Lodestar". Any HTTP answer, even a 500, keeps the existing error view. No `hooks.client.ts` change was needed.
- **Retry:** backoff 1 s, 2 s, 4 s … capped at 30 s, plus Try again (probes at once).
- **Recovery:** one `location.reload()` at the same URL, only after `/healthz` has answered. A client-side retry (`goto`) lands on the same error, because a route chunk that failed to import stays failed for the page's lifetime (seen in the e2e spec). It can't loop: no reload happens while the server is down.
- **Wording:** "Couldn’t reach Lodestar" (the brief's), separate from the existing in-shell "Can’t reach Lodestar" state, which is unchanged.
- **What counts as an answer:** any HTTP status except 502, 503 and 504. Those three come from a reverse proxy (SWAG, NPM) in front of a stopped server, so they count as no answer. Each probe times out after 5 s. This differs from the restart flow, which needs a 2xx, because a probe here must not mistake a real server 500 for "gone".
- **Seconds:** the retry line says "Trying again in 2 s", worded in the message, not through `formatSpan`: the error node ships with every route and can't carry the formatter.
- **Files:** `reach.ts` (static, since a lazy import fails when the server is gone) and its tests are new, outside the brief's owns list; the runner approved them (2026-11-13Z).
