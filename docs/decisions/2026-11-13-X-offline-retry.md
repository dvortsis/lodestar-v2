# X-offline-retry: how the error screen tells "no answer" from a server error

Date: 2026-11-13Z. Slice: X-offline-retry. Refines §10.14 (states); no owner number changes.

- **Detection:** `+error.svelte` runs when a load fails with a 5xx. Kit gives the same status whether the server failed or nothing answered, so the screen asks `/healthz`: only a request that fails outright counts as "couldn't reach Lodestar". Any HTTP answer, even a 500, keeps the existing error view. No `hooks.client.ts` change was needed.
- **Retry:** backoff 1 s, 2 s, 4 s … capped at 30 s, plus Try again (probes at once).
- **Recovery:** one `location.reload()` at the same URL, only after `/healthz` has answered. A client-side retry (`goto`) lands on the same error, because a route chunk that failed to import stays failed for the page's lifetime (seen in the e2e spec). It can't loop: no reload happens while the server is down.
- **Wording:** "Couldn’t reach Lodestar" (the brief's), separate from the existing in-shell "Can’t reach Lodestar" state, which is unchanged.
