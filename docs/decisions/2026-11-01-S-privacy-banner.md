# S-privacy-banner decisions

Completes the #277 item that X-web-polish-1101c (#331) couldn't build: the Overview says when identifying data is being recorded (§10.10.7 Privacy, §10.10.12).

- **Carrier: `GET /insights/overview`, new field `privacy`.** The Overview already loads it on first paint, refetches it on every `insights` event (every 30 s) and after each crawler action, so the banner adds no request. The Overview now also refetches it (quietly) on a `settings` event for `logging` or `insights`, so a change made in another tab shows without a reload. The shell's own first-paint requests (`/auth`, then `/stats` once the chrome loads) were not used: `/stats` is polled by the status pill on every page, so a field there would spread privacy state wider than the Overview needs. Runner decision (§2.26) to carry it on a response the app already loads.
- **Shape:** `privacy: { identifying_until: i64 | null, query_logging: bool }`. `identifying_until` is null while nothing is on, -1 when anything is on until turned off, else the latest deadline (Unix seconds) across the identifying data kinds and the two opt-ins (peer activity, filter change details). That's the same span `/logs`' banner shows. Booleans and a time only: never which kinds, a path, a query or a name.
- **Built at each request** by `settings::privacy::summary` (lodestar), from the saved `logging` and `insights` sections through `Applied::compute`, so a deadline that has passed reads as off even before the runtime's switch re-applies. It's set in the handler only, after the rollups' cache, so it's never cached and never reaches the digest, the CSV export or the `insights` event. `SettingsApi::privacy` has a default (all off) for builds and tests without settings.
- **Auth:** unchanged. `/insights/overview` is session-only (`auth::protected`). Signed in (cookie or a trusted proxy): the summary. No session or a forged cookie: the same 401 as before, with no privacy field. API keys and feed tokens can't reach the route (no key scope opts in). A session could already read both settings in full (`GET /settings/logging`, `/settings/insights`, `/logs/catalog`), so it learns nothing new.
- **Web:** `features/shell/privacy/PrivacyBanner.svelte` loads (`{#await import()}`) only while the flag says something is on. Wording and time left are reused from Settings → Logging and `/logs` (`state.ts` `timeLeft`), with the banner's own message keys for the title and Turn off now (`shell_privacy_*`, en and zh). Sharing those message modules split chunks in Settings → Logging and `/logs` (+0.15 KB each). Turn off now runs `/logs`' `turnOffAll` (identifying kinds back to Private, both opt-ins 0), loaded on the press. The query-logging banner links to Settings → Insights & digest; it has no Turn off now (the brief's choice). Neither banner can be dismissed. Time left is on the server clock (offset from `updated_at`); at expiry the banner goes and the overview is read again.
- **Surfaces (§10.10.12):** Settings → Logging and `/logs` already had the identifying-kinds banner; the Overview was the one missing.

## Sizes (gzip, before → after)

| Entry | Before | After |
| --- | --- | --- |
| Every existing `size-limit` entry | — | unchanged (byte-identical report) |
| Overview chunk (`Overview.svelte`, no entry of its own) | OVERVIEW_BEFORE | OVERVIEW_AFTER |
| Privacy banner (on demand), new | — | 3.01 KB (limit 3.31 KB) |

The banner entry includes Settings → Logging's `rules.ts` chunk (0.95 KB): `state.ts` lives in that chunk, and reaching `state.ts` alone split it out, which pushed "Settings logging parts" over its limit. With the flag off, `.size-limit.mjs` asserts from the chunk graph (`modulesIn`) that the Overview's chunks carry no `features/shell/privacy/` module.
