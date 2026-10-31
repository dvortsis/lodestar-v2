# S-locale-boot: language, time zone and first day of week at start-up

Refinement of §10.12 / §10.10.8 (Language & region); no owner decision changes.

- **Boot.** `+layout.ts` applies the cached hint (`localStorage["lodestar.region"]`: language, time zone,
  first day; non-secret, per viewer) before the first paint, the only synchronous work. The account's saved
  values are fetched by a lazily imported `lib/i18n/boot.ts` on the first pointer or key press, not during
  the load: Lighthouse's `total-byte-weight` on /search sits 0.3% under its 100 KiB assertion, so a boot-time
  request (answered with the SPA page by the preview server) and the code behind it don't fit. The server
  stays authoritative: its answer is applied and re-cached. A failed, offline, malformed or older-than-newest
  answer keeps what is applied and never throws. Order: saved → `navigator.language` → en; "Automatic"
  (empty) follows the browser. A 401 (login page) is not retried; the session module calls `refetchRegion()` when sign-in is confirmed, so the account's values apply without a reload. A browser with no
  hint therefore shows the browser's language until the first interaction. Plain `fetch`, so the API client
  stays out of the first load.
- **Time zone.** `displayTimeZone()` (the saved zone, else the browser's; a zone the browser doesn't know falls back
  inside `dateFormat`) is the default for `formatDate`, `formatDateTime` and `formatDateFull`; it lives in
  `locale.svelte.ts`, and `format/shared.ts` reads it through a `globalThis.lodestarZone` hook, because any new
  import in `shared.ts` re-split chunks and pushed the Alerts route 560 B over its limit; an explicit `timeZone` option still wins.
  Charts and `RelativeTime` go through those, so they follow the setting. Insights passes the server's
  `range.tz` explicitly and `'UTC'` for UTC-labelled ranges; those stay as they are.
- **Worker safety.** `lib/format` is imported by the file-tree web worker, so it may not import `$lib/i18n` or any `.svelte` module: an earlier head did, the worker failed to load and the tree fell back to the main thread (`detail.routes` "build in the worker"). `format/worker-safe.test.ts` guards it.
- **Passive viewers.** The layout also wakes the fetch after 30 s, so a wall display that nobody touches still syncs.
- **First day of week.** `firstDayOfWeek()` (`lib/i18n/week.svelte.ts`, kept out of the boot bundle) returns
  0/1/6 from the setting, or the language's own (`Intl.Locale` week info), else Monday. Automatic uses `navigator.language` (its region
  gives en-GB a Monday). No week-based UI can use it yet: the Insights custom range is a native
  `<input type="date">`, whose week start follows the browser and can't be set, so it is exposed only.
- **Live update.** `LanguageForm` calls `followSaved()` with all three saved values, so changes apply
  at once and refresh the hint.
- **Signed out at load.** The interaction fetch gets a 401 and keeps the hint. `session.svelte.ts` (one line outside the brief's Own list, runner-approved) calls `refetchRegion()` when the session becomes signed in. A response older than a value applied meanwhile is dropped.
- **Not changed.** Other tabs' changes are picked up on the next load, not live. The hint is per browser and
  isn't cleared on logout; the account's values replace it as soon as the fetch answers.
- **Size.** Search initial JS 42.8 → 43.04 kB gzipped (limit 45 kB, unchanged); the cost is the hint parse and the zone state in the locale chunk, and the formatters' own code stays in their lazy chunks. Lighthouse `total-byte-weight` on /search: 98,758 B locally, where the previous head measured 102,891 B (CI's figure), against 102,400.
- **"Automatic" and the OS zone.** The browser's zone is re-read whenever the settings are applied (boot, sign-in, saving), not on every format call.
