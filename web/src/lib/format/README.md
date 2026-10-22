# lib/format

Pure formatters, one module per metric, from `docs/metrics.md` §3 (sizes, counts, durations, relative time via `Intl`). No Svelte, DOM or dependency imports; unit-tested byte-exact against the doc's examples.

- Every formatter is `(value, locale = 'en', …) => string`. Pass the UI locale (W4's `currentLocale()`), and `timeZone` to the date formatters. Timestamps are Unix seconds.
- Value states (§1): `null`/`undefined`/non-finite → `—`; `0` → `0`; `{ value, approx: true }` → `≈ value`; `{ value, sampled: true }` → `≈value` (show the `sampled_note` message once per panel).
- `Intl` instances are cached per locale and options in `shared.ts`.
- `formatBytesParts`, `formatLatencyParts` and `formatSpanParts` return `{ value, unit }` for axis labels; `sizeBinEdges` / `sizeBinLabel` label the log-2 size histogram (§5a).
- Messages stay outside: `formatRate` wraps `{n}/h` itself and `formatRelative` takes the translated `just_now` string in `opts.justNow` until W4 lands.
