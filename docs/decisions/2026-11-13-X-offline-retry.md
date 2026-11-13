# X-offline-retry decisions (#842)

Date: 2026-11-13Z. Slice: X-offline-retry. Refines §10.14 (states); no owner number changes.

- **Detection:** `+error.svelte` runs when a load fails with a 5xx. Kit gives the same status whether the server failed or nothing answered, so the screen probes the server: only a request that fails outright, times out (5 s), or gets a 502/503/504 counts as "can't reach". No `hooks.client.ts` change was needed.
- **Probe target:** `/_app/version.json`, not `/healthz`. Lodestar always serves it with a 200 and `no-cache`, so only a reverse proxy (SWAG, NPM) in front of a stopped server can turn it into a 502/503/504. `/healthz` answers 503 while a source plugin is stopped, which would read a reachable server as offline (runner's call).
- **When it runs:** an effect keyed on the page address and status, because Kit reuses the root error node from one error to the next. A 404 followed by an outage probes again, and a stale offline state doesn't carry into a later 404. Nothing shows while the first probe is out. An `online` event probes at once.
- **Retry:** backoff 1 s, 2 s, 4 s … capped at 30 s, plus Try again (probes at once). Only the waiting line is announced to screen readers.
- **Recovery:** one `location.reload()` at the same URL, only after the probe has answered. A client-side retry (`goto`) lands on the same error, because a route chunk that failed to import stays failed for the page's lifetime (seen in the e2e spec). It can't loop: no reload happens while the server is down.
- **Countdown wording:** `Intl.NumberFormat` with `style: 'unit'`, `unit: 'second'`, `unitDisplay: 'short'` ("2 sec", "2秒"), which meets the `formatSpan` contract (`docs/metrics.md`) without importing the formatter into the error node (runner's call, §2.26).
- **Copy:** this state uses the shell's existing name, "Can’t reach Lodestar", so one condition has one name (runner's call, §2.26; the runner flags it to the owner).
- **Files:** `reach.ts` (a static import, since a lazy one fails when the server is gone) and its tests are new, outside the brief's owns; the runner approved them (2026-11-13Z).
