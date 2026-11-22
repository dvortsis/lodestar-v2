# lib/format

Pure formatters, one module per metric, from `docs/metrics.md` §3 (sizes, counts, durations, relative time via `Intl`). No Svelte, DOM or dependency imports; unit-tested byte-exact against the doc's examples.

- Every formatter is `(value, locale = 'en', …) => string`. Pass the UI locale (W4's `currentLocale()`), and `timeZone` to the date formatters. Timestamps are Unix seconds.
- Value states (§1): `null`/`undefined`/non-finite → `—`; `0` → `0`; `{ value, approx: true }` → `≈ value` (also for sampled values; the panel shows the `sampled_note` message once).
- `Intl` instances are cached per locale and options in `shared.ts`.
- `formatBytesParts`, `formatLatencyParts` and `formatSpanParts` return `{ value, unit }` for axis labels; `sizeBinEdges` / `sizeBinLabel` label the log-2 size histogram (§5a).
- Messages stay outside: `formatRate` wraps `{n}/h` itself and `formatRelative` takes the moment's word in `opts.justNow`. The UI (`RelativeTime`) passes the catalogue's `common_just_now`; a direct caller that passes none gets Intl's own word ("now", "现在"), never built-in English.
