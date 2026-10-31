# S-locale-boot: language, time zone and first day of week at start-up

Refinement of §10.12 / §10.10.8 (Language & region); no owner decision changes.

- **Boot.** `+layout.ts` calls `bootRegion()` (`lib/i18n/boot.ts`) without awaiting it. It applies a
  per-browser hint from `localStorage["lodestar.region"]` (language, time zone, first day: non-secret,
  per viewer) before the first paint, then fetches `GET /settings/preferences` and applies and re-caches
  the saved values. The server stays authoritative. A failed, slow, offline or malformed answer keeps the
  hint and never throws or blocks. Order: saved → `navigator.language` → en; "Automatic" (empty) keeps
  following the browser. The fetch is a plain `fetch` so the API client stays out of the first load; a 401
  is handled by the shell's own requests.
- **Time zone.** `displayTimeZone()` (saved zone if the browser knows it, else the browser's) is the default
  for `formatDate`, `formatDateTime` and `formatDateFull`; an explicit `timeZone` option still wins.
  Charts and `RelativeTime` go through those, so they follow the setting. Insights passes the server's
  `range.tz` explicitly and `'UTC'` for UTC-labelled ranges; those stay as they are.
- **First day of week.** `firstDayOfWeek()` (`lib/i18n/week.ts`, kept out of the boot bundle) returns
  0/1/6 from the setting, or the language's own (`Intl.Locale` week info), else Monday. No week-based UI
  exists yet (no date picker or custom-range calendar in Insights), so it is exposed only.
- **Live update.** `LanguageForm` calls `followSaved()` with all three saved values, so changes apply
  at once and refresh the hint.
- **Not changed.** Other tabs' changes are picked up on the next load, not live.
- **Size.** Search initial JS 42.8 → 43.3 kB gzipped (limit 45 kB, unchanged).
