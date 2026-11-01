# S-privacy-banner decisions

Completes the #277 item that X-web-polish-1101c (#331) couldn't build: the Overview says when identifying data is being recorded (§10.10.7 Privacy, §10.10.12).

- **Carrier: `GET /insights/overview`, new field `privacy`.** The Overview already loads it on first paint, refetches it on every `insights` event (every 30 s while subscribed) and after each crawler action, so the banner adds no request.
  - The shell's own first-paint requests weren't used. `/auth` is public. `/stats` is polled by the status pill on every page, which would spread privacy state wider than the Overview needs.
  - Runner decision (§2.26): carry it on a response the app already loads.
- **Shape:** `privacy: { identifying_until: i64 | null, query_logging: bool } | null`.
  - The field is **null while nothing is on**, so the Overview's eager check is one truthiness test.
  - `identifying_until` is null when only query logging is on, -1 when anything is on until turned off, and otherwise the latest deadline (Unix seconds).
  - The deadline covers the identifying data kinds and the two opt-ins (peer activity, filter change details): the same span `/logs`' banner shows.
  - Booleans and a time only: never which kinds, a path, a query or a name.
- **Built at each request** by `settings::privacy::summary` (lodestar), from the saved `logging` and `insights` sections through `Applied::compute`, on the clock of the response's `updated_at`.
  - A deadline that has passed reads as off even before the runtime's switch re-applies.
  - It's set in the handler only, after the rollups' cache. So it's never cached, and the digest, the CSV export and the `insights` event never carry it.
  - `Registry::privacy` reads both sections through `entry()`, which panics if one is missing rather than reporting "off".
  - `SettingsApi::privacy` defaults to all off for builds and tests without settings.
- **Updates without a reload:** within 30 s of a change in another tab, through the `insights` event's refetch. A separate `settings` subscription would have been instant, but it cost 32 B in the Overview's first view (see Sizes), so it's left out. In this tab, Turn off now and the countdown's expiry read the overview again at once.
- **Auth:** unchanged. `/insights/overview` is session-only (`auth::protected`).
  - Signed in (cookie or a trusted proxy): the summary.
  - No session or a forged cookie: the same 401 as before, with no privacy field.
  - API keys and feed tokens can't reach the route (no key scope opts in).
  - A session could already read both settings in full (`GET /settings/logging`, `/settings/insights`, `/logs/catalog`), so it learns nothing new.
- **Web:** `features/shell/privacy/PrivacyBanner.svelte` mounts through the Overview's existing `Lazy` and a dependency-free `door.ts`, only while `privacy` is set. A failed load shows `Lazy`'s existing error with Reload.
  - **Wording:** the time left reuses Settings → Logging's `timeLeft`. The text has its own keys (`shell_privacy_*`, en and zh) with the same wording. Sharing Settings → Logging's and `/logs`' message modules split their chunks (+0.15 KB each), so they aren't shared.
  - **Turn off now** runs `/logs`' `turnOffAll` (identifying kinds back to Private, both opt-ins to 0), loaded on the press.
  - **Query logging:** its banner links to Settings → Insights & digest.
  - Neither banner can be dismissed. Time left is on the server clock (an offset from `updated_at`). At expiry the banner goes and the overview is read again.
- **Surfaces (§10.10.12):** Settings → Logging and `/logs` already had the identifying-kinds banner. The Overview was the one missing.

## Sizes (gzip, before → after)

| Entry | Before | After |
| --- | --- | --- |
| Every existing `size-limit` entry | | unchanged (byte-identical report) |
| Overview chunk (`Overview.svelte`, no `size-limit` entry) | 14,123 B | 14,203 B (+80 B) |
| Privacy banner (on demand), new | | 3.37 KB (limit 3.71 KB, measured + 10%) |

- **The Overview's +0.08 KB is over the brief's +0.05 KB.** That's the if-block, the `Lazy` mount with its props, and two hashed preload paths (the door and the preload helper). It's the floor found for a conditional lazy mount in this chunk:
  - importing the banner directly: +0.11 KB;
  - with `{#await}`: +0.25 KB;
  - with a `settings` subscription: +0.03 KB more.
  The runner decides (§2.26).
- **The banner entry (the door, the banner and their own modules) includes Settings → Logging's `rules.ts` chunk (0.95 KB).** `state.ts` lives in that chunk. Reaching `state.ts` alone split it out, which pushed "Settings logging parts" over its limit.
- **Chunk-graph guard:** `.size-limit.mjs` asserts from the chunk graph (`modulesIn`) that the Overview's chunks carry no `features/shell/privacy/` module.
