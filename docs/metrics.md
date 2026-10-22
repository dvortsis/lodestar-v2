# Metric contracts

What every number in the UI means, how it's formatted, and which JSON field
carries it. Architecture §10.12 (data display contracts) and §10.9 (Insights)
are the source of truth; this file makes them precise enough for the
formatters (W8, `web/src/lib/format/`), the API DTOs (B1) and the
analytics-validation matrix (§10.17). If this file and the architecture
document disagree, the architecture document wins and this file is fixed.

**Rules that apply everywhere:**
- **One formatter per metric** (§10.12). Facets, rows, the drawer, tooltips,
  tables, chart axes, KPIs, CSV labels and the digest all call the same
  function. No component formats a number itself.
- **The client never computes counts, cohorts or percentages** (build plan).
  Every rate, share, conversion and percentile in this file is computed by the
  server and arrives in the response. Formatters only present.
- **Every displayed count, KPI and chart container carries a `data-testid`**
  (§10.12 validation hooks).
- **Locale** is the UI locale (`en`, `zh`); every `Intl` call takes it.
  **Timezone** is the user's setting (default: the browser's), passed as
  `timeZone` to every `Intl.DateTimeFormat`; Insights shows which one is in use.

---

## 1. Value states: null, zero, estimated, sampled

| State | Meaning | Display | Formatter input |
|---|---|---|---|
| **Null** | Unknown: never measured (never scraped, not classified yet, no data for the period) | `—` (U+2014 em dash), with an accessible name from the metric ("Never scraped", "Not measured") | `null` |
| **Zero** | Measured and really 0 | `0` (formatted like any other value) | `0` |
| **Estimated** | The value itself is an estimate (DHT estimate, harvest uniqueness, histogram percentiles, `≈` totals) | `≈` + U+00A0 + value, e.g. `≈ 2.4M` | `{ value, approx: true }` |
| **Sampled** | Computed on a uniform sample (facets above 1M matches) | `≈` + value, plus a note once per panel: "Counts are estimated from a sample" (glossary) | `{ value, approx: true }`; the panel reads `sampled` from the response |
| **Gap** | A time bucket with no data (crawler paused, history ring gap) | A **gap** in lines; an empty bin in bars; `—` in tables. **Never drawn as 0.** | `null` in the series |
| **Incomplete** | A discovery cohort that isn't final (§10.9.3) | Hatched mark + the word "incomplete"; excluded from the yield KPI | `final: false` on the cohort |

- `≈` is never added by the client on its own judgement: it appears only when
  the field's `*_approx` / `sampled` / `approx` flag is set, or for metrics this
  file marks **always ≈**.
- A null is never coerced to 0 for sorting or summing in the client. The
  server sorts (never-scraped torrents sort last under `seeders` descending,
  §8).

## 2. Time bases

Every chart subtitle names its time base (§10.9.3).

| Base | Definition | Used by |
|---|---|---|
| **Event time** | The UTC hour (or day) the event happened: a torrent stored, indexed, a search answered | Trend, "new searchable", ingest rate, Serve, stage-detail counters |
| **Discovery cohort** | The UTC hour a hash was **first harvested**. Later stages count progress *of that cohort* | Funnel, loss reasons, end-to-end yield, time to searchable |
| **Snapshot** | The value now (at the last rollup flush or index commit) | Searchable torrents, queue depths, sizes, health |
| **Per-torrent timestamp** | One torrent's own times (`discovered_at`, `last_seen_at`, `last_scrape_at`, `metadata_at`) | Rows, drawer, facets |

Hours and days are UTC buckets on the server. Day buckets for display are
**UTC days** (rollups are compacted to UTC days); the axis labels say "UTC"
when the user's timezone isn't UTC. All API timestamps are **Unix seconds
(integer, UTC)**.

## 3. Formatters

All live in `web/src/lib/format/`. Each is a pure function
`(value, locale, opts?) => string`, plus a `…Parts` variant where a component
needs the unit separately (axis labels). A number and a unit symbol the
formatter adds itself (bytes, `/h`) are joined with U+00A0 (no-break space);
units from `Intl` keep the `Intl` spacing.

**Every example below is the exact output of the stated options in Node 22
(ICU as shipped) for `en` and `zh`.** W8's unit tests are written from these
rows; if an `Intl` upgrade changes an output, the test and this table change
together. `timeZone` in the examples is `UTC`; the date is
2026-10-22 14:05 UTC (a Thursday).

| Formatter | Options | en | zh |
|---|---|---|---|
| `formatCount` | `Intl.NumberFormat(locale, { maximumFractionDigits: 0 })`. Exact integers with grouping. | `0`, `1,284`, `84,210` | `0`, `1,284`, `84,210` |
| `formatCompact` | `Intl.NumberFormat(locale, { notation: 'compact', compactDisplay: 'short', maximumFractionDigits: 1 })`. Only where this file says so (KPIs, stage strip, status pill, headline stats, axis ticks); tables and tooltips use `formatCount`. zh compacts only from 万 (10⁴). | `950`, `1.2K`, `84.2K`, `2.4M`, `12.4M` | `950`, `1200`, `8.4万`, `240万`, `1240万` |
| `formatBytes` | **IEC, base 1024**: B, KiB, MiB, GiB, TiB, PiB. Unit = the largest whose value is ≥ 1. Digits `d` = 0 for B; otherwise 2 if the value is < 10, 1 if < 100, else 0. Number via `Intl.NumberFormat(locale, { minimumFractionDigits: d, maximumFractionDigits: d })` (fixed digits, so tabular columns align), then U+00A0 and the unit symbol. If rounding reaches 1024, step up a unit. Unit symbols are the same in every locale. Never SI (`kB`, `GB`). | `512 B`, `4.37 GiB`, `38.2 MiB`, `912 KiB`, `1.00 MiB` (never `1024 KiB`) | same as en |
| `formatPercent` | Input is a **ratio 0–1** from the server. `Intl.NumberFormat(locale, { style: 'percent', minimumFractionDigits: f, maximumFractionDigits: f })`, `f = 1` for rates (conversion, yield, zero-result rate, reply ratio, coverage), `f = 0` for distribution shares (category mix, loss-reason share). Edges: a non-zero value that would round to 0 shows `<` + the smallest step (`<0.1%`, `<1%`); a value < 1 that would round to 100% shows `>99.9%` / `>99%`. | `37.4%`, `82%`, `0.0%`, `<0.1%`, `>99.9%` | same as en |
| `formatShare` | Composition only. Input is the server's **whole percent** (integer 0–100; any non-zero share is ≥ 1 and only a complete one is 100, `classify::Composition::pct_bytes`). `Intl.NumberFormat(locale, { style: 'percent', maximumFractionDigits: 0 }).format(pct / 100)`; the division is a unit change, not a computation. The dominant-share label is the message `dominant_share` ("{share} {group}"). | `82%`, `1%`, `100%`; label `82% video` | `82%`; label TBD |
| `formatRate` | `formatCompact(value)` inside the message `rate_per_hour` = `{n}/h` (en; zh TBD). Per-day uses `rate_per_day` = `{n}/d`. | `1.2K/h`, `0/h` | TBD |
| `formatDelta` | The metric's own number options plus `signDisplay: 'exceptZero'`, then every U+002D hyphen-minus in the result is replaced with U+2212 (minus). A value that rounds to zero prints unsigned. Always paired with an up/down icon and the period text ("vs previous 7 days"); never colour alone. | `+1,240`, `−3`, `0`; percent (f = 1): `−3.2%`, `0.0%` | same as en |
| `formatLatency` | Input milliseconds. Below 1,000 ms: `Intl.NumberFormat(locale, { style: 'unit', unit: 'millisecond', unitDisplay: 'short', maximumFractionDigits: v < 1 ? 1 : 0 })`. From 1,000 ms: unit `second`, `maximumFractionDigits: 1`. Percentiles from histograms are **always ≈**. | `0.8 ms`, `12 ms`, `1.4 sec` | `0.8毫秒`, `12毫秒`, `1.4秒` |
| `formatSpan` | Durations (time to searchable, queue age, index lag, days until full). Input seconds. Unit: `second` below 60 s, `minute` below 3,600 s, `hour` below 172,800 s (48 h), else `day`. `Intl.NumberFormat(locale, { style: 'unit', unit, unitDisplay: 'short', maximumFractionDigits: v < 10 ? 1 : 0 })` where `v` is the value in that unit. | `45 sec`, `12 min`, `5.2 hr`, `3.1 days` | `45秒`, `12分钟`, `5.2小时`, `3.1天` |
| `formatRelative` | Rows only. `Intl.RelativeTimeFormat(locale, { numeric: 'auto', style: 'long' })`; unit = the largest of second/minute/hour/day/week/month/year with \|Δ\| ≥ 1, value truncated toward zero. \|Δ\| < 45 s → message `just_now`. Re-rendered at most once a minute. Always paired with `formatDateTime` in a tooltip (the RelativeTime primitive, §10.11). | `3 hours ago`, `yesterday`, `in 5 minutes` | `3小时前`, `昨天`, `5分钟后` |
| `formatDateTime` | Drawer, tables, tooltips. `Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'short', hourCycle: 'h23', timeZone })`. | `Oct 22, 2026, 14:05` | `2026年10月22日 14:05` |
| `formatDateFull` | Chart tooltip headers (§10.9.6). `Intl.DateTimeFormat(locale, { weekday: 'short', year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit', hourCycle: 'h23', timeZoneName: 'short', timeZone })`. Day and week buckets drop `hour`, `minute` and `timeZoneName`. | `Thu, Oct 22, 2026, 14:05 UTC`; day: `Thu, Oct 22, 2026` | `2026年10月22日周四 UTC 14:05`; day: `2026年10月22日周四` |
| `formatDate` | Axis ticks for day buckets, date-only facts. `Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeZone })`. | `Oct 22, 2026` | `2026年10月22日` |
| `formatScore` | 0–100 integer scores (junk). Same options as `formatCount`. | `42` | `42` |

The architecture's example strings ("1.2k/h", "Seen 3 h ago") are
illustrative; the outputs above are the contract.

---

## 4. Torrent metrics (rows, drawer, compare, facets)

| Metric | Definition | Formatter | Null / zero / ≈ | Time base |
|---|---|---|---|---|
| **Size** | Total payload bytes (`torrents.size`), padding files included as stored | `formatBytes` | Never null. 0 is shown as `0 B` (padding-only torrents are junk-flagged) | — |
| **Files** | Number of files in the info dict (`file_count`) | `formatCount` | Never null | — |
| **Seeders** | **Max across sources within the freshness window** (§10.12): the highest seeders count among the sources that answered within the window. *Pending in M4: the store keeps what the scheduler gives (PR #32), and the scheduler (slice 4) fixes the window's length and whether the BEP 33 estimate counts as a source.* | `formatCount` | **Null = never scraped → `—`** (and "Never scraped" in the drawer). 0 = scraped, nobody seeding → `0`. ≈ only if the server sets `seeders_approx` (the value came from the DHT estimate alone). | Per-torrent (`last_scrape_at`) |
| **Leechers** | As seeders, for leechers | `formatCount` | As seeders | Per-torrent |
| **Completed** | Max `completed` (downloaded) across reporting trackers; a coarse age hint (§6) | `formatCount` | Null = never scraped | Per-torrent |
| **Trackers reporting** | n of m: n = trackers whose answer had **s + l + c > 0** (opentracker rule, §15); **m = trackers that answered in the last answered round** (`SwarmUpdate.trackers_checked`, PR #32). Trackers that didn't answer are in neither number. | `formatCount` for each; row `4 / 20`; drawer "Reported by 4 of 20 trackers checked" | Null = never scraped → `—`. `0 / 20` is a real zero. A round where no tracker answered changes nothing, so n and m keep the last answered round's values. | Per-torrent |
| **DHT estimate** | BEP 33 bloom-filter estimate of seeds and peers from up to 8 closest responders | `formatCount` | **Always ≈** ("≈ 1,225 seeds"; labelled "estimate"). Null = no estimate (no DHT answer, or no responder stores the hash: "unknown", not "dead") | Per-torrent |
| **Scrape age** | `now − last_scrape_at` (time of the last *answered* round) | `formatRelative` (row focus/hover), `formatDateTime` (drawer) | Null = never scraped | Per-torrent |
| **Swarm trend** | Least-squares slope of the last 7 daily seeders samples, seeders/day; needs ≥ 3 samples (PR #32) | `formatDelta` + "/day" (message) | Null = fewer than 3 samples → "Not enough history" | Per-torrent (daily ring) |
| **Swarm history** | 14 daily + 26 weekly (seeders, leechers) samples, newest first | Axis: `formatCount`, `formatDate`; tooltip `formatDateFull` | Gap samples are `null` → line gaps | Per-torrent, UTC day/week |
| **Suspect swarm** | One tracker reports a swarm the DHT and other trackers don't see (fake-seeding signature, §8) | Label "Suspect seeders" + icon | Boolean | Per-torrent |
| **Discovered** | `discovered_at`: when *we* first saw the key (§6) | `formatRelative` in rows, `formatDateTime` in the drawer | Never null. If the `backfill` flag is set, add the marker "First seen during backfill" | Per-torrent |
| **Last seen** | `last_seen_at`: latest DHT sighting; SQLite is at most 1 h behind the hot column (PR #32) | `formatRelative` / `formatDateTime` | Never null for stored torrents | Per-torrent |
| **Metadata fetched** | `metadata_at` | `formatDateTime` | Never null | Per-torrent |
| **Piece size** | `piece_len` bytes | `formatBytes` | Never null | — |
| **Junk score** | 0–100, sum of reason weights, capped (§6, M3) | `formatScore` + reasons as text | Null = not classified yet → `—` | — |
| **Composition share** | Share of the torrent's **non-padding** bytes (and, separately, non-padding files) per category group (§10.18 slots), computed server-side as a whole percent: any non-zero share is ≥ 1, only a complete one is 100 (`classify/src/composition.rs`). Padding files are in no group's share. | `formatShare`; dominant share as "82% video" | A group with no files is omitted. A padding-only torrent has an empty composition. | — |
| **Parser confidence** | 0–1 from the release parser / resolver | `formatPercent` (f = 1) | Null = not parsed | — |

## 5. Search metrics

| Metric | Definition | Formatter | Null / zero / ≈ | Time base |
|---|---|---|---|---|
| **Result count** | Matches for the AST | `formatCount` ("1,284 results"); `formatCompact` when approximate ("≈ 2.4M results") | ≈ when `total_approx`. 0 → zero-result state | Snapshot (last commit) |
| **Facet count** | Disjunctive: each facet ignores its own selection (§10.4) | `formatCount`; ≈ + note when sampled | Sampled above 1M matches | Snapshot |
| **Hidden count** | Junk hidden ("N hidden · Show") and filter-hidden ("N hidden by filters · Show") | `formatCount` | 0 → the control isn't shown | Snapshot |
| **Per-clause recovery count** | Results if a clause were removed ("Removing `trackers:>=3` gives 1,240") | `formatCount` | ≈ under the same rule as result count | Snapshot |
| **Took** | Server time for the request | `formatLatency` | — | — |
| **Headline stats** | Total searchable torrents, + new in the last hour (empty state) | `formatCompact`, `formatCount` | — | Snapshot / event time |

## 5a. Histograms (§10.12)

- **Bins are explicit and come from the server** (`{lo, hi, count}`); the client never re-bins. Bins are half-open `[lo, hi)`; the last bin is closed.
- **Size** uses **log bins** with labelled edges. Edges are powers of 2 in bytes, labelled with `formatBytes` (`1.00 GiB`, `4.00 GiB`); the bin label is "lo – hi" with an en dash.
- **Discovered** uses time bins (hour, day or week by range), labelled with `formatDate` / `formatDateFull`; the backfill window is marked.
- **Year** uses one bin per year.
- **Zero baselines**; **empty bins are shown as empty** (present with `count: 0`), never dropped or interpolated.
- Sampled histograms (above 1M matches) carry the panel's `≈` note.
- The exact bin edges for size and the time-bin rule are fixed in B6 and recorded here then.

## 6. Insights metrics (§10.9)

### 6.1 Stage strip (§10.9.1)

| Stage | Headline number | Formatter | ≈ | Time base |
|---|---|---|---|---|
| Discover | Unique hashes harvested in range | `formatCompact` | **Always ≈** ("unique (≈)": the seen filter rotates) | Event time |
| Fetch | Metadata retrieved in range | `formatCompact` | Exact | Event time |
| Understand | Stored and classified in range | `formatCompact` | Exact | Event time |
| Index | Searchable torrents | `formatCompact` | Exact | Snapshot |
| Serve | Searches answered in range | `formatCompact` | Exact | Event time |

The text-equivalent ordered list uses `formatCount`.

### 6.2 Overview KPIs (§10.9.2, level 1)

| KPI | Definition | Formatter | Null / ≈ | Time base |
|---|---|---|---|---|
| **Searchable torrents** | Tantivy document count at the last commit (reconciled, §10.17); Δ vs the previous period's end | `formatCompact` (KPI), `formatCount` (tooltip/table); Δ `formatDelta` | Exact. Imported torrents included and shown as a separate "Imported" total | Snapshot |
| **New searchable in range** | Torrents that first became searchable in the range (first `index_watermark_hour` covering each id) | `formatCompact` / `formatCount` | Exact; 0 is real | Event time |
| **End-to-end yield** | Searchable ÷ harvested over **final** cohorts in range | `formatPercent` (f = 1) | Null when no cohort in range is final → `—` + "No completed cohorts yet". Excludes imports | Discovery cohort |
| **Time to searchable** | First harvest → searchable, p50 and p95, over final cohorts | `formatSpan` | **Always ≈** (log-bucket histograms, ≤ 10% resolution). Null as yield | Discovery cohort |
| **Health** | Running / Paused / Degraded + reason | Text + icon (glossary) | — | Snapshot |

### 6.3 Movement (level 2)

| Metric | Definition | Formatter | Notes | Time base |
|---|---|---|---|---|
| **Newly searchable** | Torrents that became searchable per hour (ranges ≤ 7 d) or per UTC day | `formatCount`; axis `formatCompact` | Gaps where no rollup exists (paused) | Event time |
| **Unique hashes harvested** | Separate aligned line; no dual axis | `formatCount`; axis `formatCompact` | Always ≈ | Event time |
| **Ingest rate** | **New stored torrents per hour**, not harvested hashes (§10.12); status pill "Crawling · 1.2K/h" | `formatRate` | Over the last full hour | Event time |
| **Annotations** | restart, upgrade, intensity/config change, import, reindex, network change, paused | Text label + `formatDateFull` | — | Event time |

### 6.4 Funnel and loss reasons (level 3)

| Metric | Definition | Formatter | Notes | Time base |
|---|---|---|---|---|
| **Stage count** | Hashes of the cohorts in range that reached each stage: Harvested → Peer found → Metadata fetched → Stored → Searchable | `formatCount` | Never increases from one stage to the next (reconciled). Harvested is exact *per cohort* (cohorts count first harvests) | Discovery cohort |
| **Stage conversion** | count(stage n) ÷ count(stage n−1), server-computed | `formatPercent` (f = 1) | Null if the previous stage is 0 | Discovery cohort |
| **Loss reason share** | Share of a stage's losses per reason; 100% stacked bar | `formatPercent` (f = 0), counts `formatCount` | API names, derived from the `Loss` enum in `crates/sources/src/funnel.rs` (which stores numeric codes 1–10 only; the names match the Prometheus labels in `crates/lodestar/src/metrics.rs`): `no_peers`, `timed_out`, `refused`, `encryption_failed`, `oversize`, `invalid`, `blocked`, `evicted`, `gave_up`, `unreachable` (code 10, §16 M2) | Discovery cohort |
| **In progress** | Harvested − Σ terminal outcomes, per cohort | `formatCount` | Shown only on incomplete cohorts | Discovery cohort |

Category filters apply only from Understand onward; with the content filter
active, earlier stages are greyed with the reason (§10.9.3).

### 6.5 Segments (level 4)

All sorted horizontal bars; values `formatCount`, shares `formatPercent`
(f = 0), yields `formatPercent` (f = 1).

| Segment | Metric | Time base |
|---|---|---|
| Category group / content type | Newly searchable by group / type | Event time |
| Harvest strategy (BEP 51 / passive / active lookup) | Yield (searchable ÷ harvested, final cohorts) | Discovery cohort |
| IP family (v4 / v6) | Yield | Discovery cohort |
| Ambiguous names resolved, by method (files / folder / structure / plugin) | Count | Event time |

### 6.6 Exceptions (level 5)

Each item is text + icon + a fix action; numbers inside use the formatter of
the metric they quote (e.g. "Index lag 42 s" → `formatSpan`; "Disk full in
11 days" → `formatSpan`; yield vs baseline → `formatPercent` f = 1).

### 6.7 Serve (level 6)

| Metric | Definition | Formatter | ≈ | Time base |
|---|---|---|---|---|
| Searches per hour | Search requests answered | `formatRate` | Exact | Event time |
| Zero-result rate | Searches with total = 0 ÷ searches | `formatPercent` (f = 1) | Exact; null when 0 searches | Event time |
| Search latency p95 | Server-measured, from log buckets | `formatLatency` | Always ≈ | Event time |
| Alerts fired | Saved-search alerts that fired | `formatCount` | Exact | Event time |
| Sends to download clients | Successful sends | `formatCount` | Exact | Event time |

### 6.8 Stage detail pages (§10.9.5)

| Metric | Formatter | Notes |
|---|---|---|
| DHT nodes, routing-table size | `formatCount` | Snapshot |
| Reply ratio (NAT-saturation signal) | `formatPercent` (f = 1) | Event time |
| BEP 51 sample yield | `formatPercent` (f = 1) | Event time |
| Keyspace coverage | `formatPercent` (f = 1) | Snapshot |
| Seen-filter load factor | `formatPercent` (f = 1) | Snapshot |
| Queue depth / queue age | `formatCount` / `formatSpan` | Snapshot |
| Fetch outcome breakdown, TCP vs uTP, MSE vs plaintext | `formatCount`, `formatPercent` (f = 1) | Event time |
| Fetch latency histogram | `formatLatency`; bins labelled with edges | Always ≈ percentiles |
| Per-IP limiter rejections, ephemeral-port pressure | `formatCount`, `formatPercent` | Event time |
| Parser confidence distribution, junk score distribution | `formatPercent` / `formatScore` bins | Snapshot |
| Blocklist drops | `formatCount` (counts only) | Event time |
| DB / index / file-index size vs §3 budget, projection | `formatBytes`; projection `formatSpan` ("days until full") | Snapshot |
| NRT commit lag | `formatSpan` (or `formatLatency` < 1 s) | Snapshot |
| Segment count, merges | `formatCount` | Snapshot |
| Scrape coverage (share of torrents with a swarm row) | `formatPercent` (f = 1) | Snapshot |
| Scrape freshness (age of scrape data, p50) | `formatSpan` | Snapshot |
| Per-tracker table: state (ok / backoff / down / banned), reporting hashes, requests by outcome | Text + icon; `formatCount` | Snapshot / event time |
| Cancel rate, facet sampling rate | `formatPercent` (f = 1) | Event time |

---

## 7. UI field contract

JSON field names the API exposes, for B1's DTOs. Types are JSON types;
**nullable** means the field is always present and may be `null` (never
omitted). Times are Unix seconds. Status: **main** = served on `main` today;
**#32** = backed by the store in open PR #32 (M4 slices 1–3), API exposure is
M4 slice 6, still pending; **planned** = named here and in the build plan
(§1 "M4 contract"), no code yet.

### 7.1 Torrent summary (search hits and detail)

| Field | Type | Nullable | Metric | Status |
|---|---|---|---|---|
| `id` | integer | no | — | main |
| `hash` | string (40 hex) | no | — | main |
| `hash_v2` | string (64 hex) | yes | — | main |
| `name` | string | no | Raw name (decoded) | main |
| `title` | string | yes | Resolved title; null = not resolved | main on search hits (B5a); planned on detail |
| `resolved_via` | string | yes: null = the name was informative (no resolution) | "resolved via …": `files`, `folder`, `structure` (`classify::ResolvedVia::as_str`); `plugin` arrives with §18 | main on search hits (B5a); planned on detail |
| `size` | integer (bytes) | no | Size | main |
| `files` | integer | no | Files | main |
| `piece_len` | integer (bytes) | no | Piece size | main |
| `discovered_at` | integer | no | Discovered | main |
| `metadata_at` | integer | no | Metadata fetched | main |
| `last_seen_at` | integer | no | Last seen | planned (store: main; written through by #32) |
| `flags` | string[] | no | `private`, `v2`, `hybrid`, `padding`, `symlinks`, `imported`, `backfill`, `files_partial`, `single_file`, `warnings` | main |
| `content_type` | string | yes | `movie`, `tv`, `music`, `audiobook`, `ebook`, `comic`, `software`, `game`, `images`, `video`, `other`, `adult`; null = not classified | main on search hits (B5a); planned on detail |
| `adult_kind` | string | yes | `scene`, `movie`, `photos`, `comics`, `game`, `vr`, `audio`; non-null only when `content_type = "adult"` | main on search hits (B5a); planned on detail |
| `composition` | `{group, files, bytes, pct_bytes, pct_files}[]` | no (may be empty) | Composition; `group` ∈ `video`, `audio`, `software`, `documents`, `images`, `archives`, `other`; `files`/`bytes` exclude padding; `pct_*` are whole percents (0–100) of non-padding bytes/files, rounded as `Composition::pct_bytes`; groups in the bar's order (§10.18), groups with no files left out | main on search hits (B5a); planned on detail |
| `junk_score` | integer 0–100 | yes: null = not classified | Junk score | main on search hits (B5a); planned on detail |
| `junk_reasons` | string[] | no | Stable reason keys (§6 M3) | main on search hits (B5a); planned on detail |
| `hidden_by_filter` | boolean | no | Hidden by a §20 filter rule | planned |
| `why` | string[] | no | Filter reason keys; empty unless hidden | planned |
| `seeders` | integer | **yes: null = never scraped** | Seeders | #32 |
| `leechers` | integer | yes (as seeders) | Leechers | #32 |
| `completed` | integer | yes (as seeders) | Completed | #32 |
| `seeders_approx` | boolean | no | True when the DHT estimate alone supplied the counts; always false if slice 4 keeps the DHT out of the max | planned, **needs M4 sign-off** |
| `trackers_reporting` | integer | yes (as seeders) | Trackers reporting (n) | #32 |
| `trackers_checked` | integer | yes (as seeders) | Trackers checked (m): trackers that answered in the last answered round | #32 |
| `last_scrape_at` | integer | yes (as seeders) | Scrape age | #32 |
| `swarm_suspect` | boolean | no | Suspect swarm (`swarm_flags::SUSPECT`) | #32 (flag in the store; API name planned) |
| `score` | number | no | Relevance (hits only) | main |
| `matched_files` | integer[] | omitted unless the query has a file clause | Matched-file positions | main |

"Never scraped" means: no `swarm` row, or a row with `trackers_checked = 0`
and no DHT estimate (PR #32). The six nullable swarm fields (`seeders`,
`leechers`, `completed`, `trackers_reporting`, `trackers_checked`,
`last_scrape_at`) are then all `null` together.

### 7.2 Detail-only (swarm section)

| Field | Type | Nullable | Metric | Status |
|---|---|---|---|---|
| `dht_seeds` | integer | yes: null = no estimate | DHT estimate (always ≈) | #32 |
| `dht_peers` | integer | yes (as `dht_seeds`) | DHT estimate | #32 |
| `swarm_trend` | number (seeders/day) | yes: null = < 3 daily samples | Swarm trend | #32 (`history::trend`; API name planned) |
| `swarm_trackers` | `{url, seeders, leechers, completed}[]` | no (may be empty) | Per-tracker table; reporting trackers only, from the last round | #32 (`Store::swarm_trackers`) |
| `swarm_history` | `{daily: Sample[14], weekly: Sample[26]}` | yes: null = no ring kept | Swarm history; newest first | #32 (`Store::history`) |
| `Sample` | `{at: integer, seeders: integer\|null, leechers: integer\|null}` | — | `at` = start of the UTC day or week; null counts = gap (`0xFFFF`). Days are Unix epoch days (UTC); a week is epoch-day / 7, so **weeks start on Thursday 00:00 UTC**. Counts saturate at **65,534**: show 65,534 as `≥ 65,534` | #32 |
| `file_list`, `file_list_total`, `magnet` | as today | no | — | main |

### 7.3 Search response

| Field | Type | Nullable | Metric | Status |
|---|---|---|---|---|
| `total` | integer | no | Result count | main |
| `total_approx` | boolean | no | ≈ on the result count: set only when `total` was estimated (above 100,000 matches, architecture §16 "B5a"); `exact=1` asks for the exact count | main (B5a) |
| `took_ms` | number | no | Took | main |
| `hidden_junk` | integer | no | Hidden count (junk) | planned (needs the default junk threshold, §16 "B5a") |
| `hidden_count` | integer | no | Hidden count (filters); distinct from the per-torrent `hidden_by_filter` | planned |
| `next_cursor` | string | yes: null = last page | — (the next page, from the same snapshot) | main (B5a) |
| `canonical`, `stemmed`, `offset`, `limit`, `hits` | as today | no | — | main |

### 7.4 Facets response (B6)

`GET /facets` (the search's `q`, `sort`, `hidden`, optional `lease` = a
search's `next_cursor`, `limit.<facet>=` 1–100) or `POST` with the AST.
Counts are over exactly what `/search` returns for the same parameters.

| Field | Type | Nullable | Notes |
|---|---|---|---|
| `total`, `total_approx` | integer, boolean | no | The search's result count, as `/search` reports it |
| `sampled` | boolean | no | True when any group was computed on a sample (a pass above 1M matches) |
| `sample_rate` | number 0–1 | yes: null when not sampled | Share of the documents a sampled pass visited, for the "≈" note (`sampled_note`) |
| `facets.<name>` | `{approx: boolean, distinct: integer, other: integer, values: {value: string, count: integer, selected: boolean, excluded: boolean}[]}` | no | Term facets; **disjunctive** (each ignores the query's own clauses on it, architecture §16 "B6"). `values`: the top 8 (or `limit.<name>`) by count, ties by value, then any selected or excluded value outside them (count may be 0). `other`: the unlisted values' counts summed. `approx`: sampled, show "≈". Flags (`private`, `v2`, `ambiguous`, `imported`, `backfill`) list `yes` and `no`. Names: `type`, `ext`, `res`, `category`, `lang`, `group`, `codec`, `audio`, `hdr`, `private`, `v2`, `has`, `ambiguous`, `imported`, `backfill`, and `adult` only when it has values |
| `histograms.<name>` | `{approx: boolean, unit: string, missing: integer, bins: {lo: integer, hi: integer\|null, count: integer, backfill: boolean}[]}` | no | Explicit bins, `lo` inclusive, `hi` exclusive (null: open top bin); empty bins present with `count: 0`. Not disjunctive: bins + `missing` sum to the result count. `size`: bytes, `[0, 1 MiB)`, then powers of two to `[512 GiB, 1 TiB)`, then `[1 TiB, ∞)`. `discovered`: UTC seconds; `unit` is `hour`, `day`, `week` (Unix epoch weeks, from Thursday 00:00 UTC, as §7.2), `month` (UTC calendar months) or `year` (UTC calendar years), chosen from the matches' range (< 48 h, < 90 days, < 104 weeks, < 10 years, else years). When that range is more than four times as wide as its 1st to 99th percentile (stray dates: a source's `created_at` of 0, a year 9999), the bins cover the percentile range widened by half on each side and the matches outside that are counted in one edge bin each, `[earliest, start)` and `[end, null)`, so edge bins can be irregular. At most 130 bins (the earliest years go in the lower edge bin past that). `year`: the release's first year, `[y, y + 1)`; `missing` = no year parsed |
| `backfill_before` | integer (Unix seconds) | yes: null = unknown | The crawler's first run; a `discovered` bin with `backfill: true` ends by then (§10.4 "Backfill" marker). Labels use the user's timezone; edges stay UTC (§2) |
| `buckets.seeders`, `buckets.trackers` | term facet (as `facets.<name>`), values `≥1`, `≥5`, `≥50`, `never_scraped` | yes: **null until M4's swarm fields are indexed** | Never zeros while unavailable: show "—" |
| `unavailable` | string[] | no | Groups this index can't serve: fields an older index lacks during a rebuild ("index rebuilding; … not yet available"), and `seeders`/`trackers` until M4 |
| `canonical`, `took_ms` | as `/search` | no | — |

### 7.5 Insights (planned; names fixed here)

| Field | Type | Nullable | Metric |
|---|---|---|---|
| `range` | `{from: integer, to: integer, tz: string}` | no | Context bar |
| `updated_at` | integer | no | "Last updated" |
| `stages.{discover,fetch,understand,index,serve}` | integer | no | Stage strip (`discover` always ≈) |
| `kpi.searchable` | integer | no | Searchable torrents |
| `kpi.searchable_delta` | integer | yes: null without Compare | Δ |
| `kpi.imported` | integer | no | Imported total |
| `kpi.new_searchable` | integer | no | New searchable in range |
| `kpi.yield` | number 0–1 | yes: null = no final cohort | End-to-end yield |
| `kpi.time_to_searchable_p50_s`, `kpi.time_to_searchable_p95_s` | integer (seconds) | yes (as yield) | Time to searchable (≈) |
| `kpi.health` | `{state: "running"\|"paused"\|"degraded", reason: string\|null}` | no | Health |
| `trend[]` | `{at: integer, searchable: integer\|null, harvested: integer\|null}` | no | Movement; null = gap |
| `annotations[]` | `{at: integer, kind: string, text: string}` | no | Annotations |
| `funnel[]` | `{cohort: integer, final: boolean, stages: {harvested, peer_found, fetched, stored, searchable: integer}, losses: {<reason>: integer}, in_progress: integer}` | no | Funnel by cohort |
| `serve` | `{searches: integer, zero_result_rate: number\|null, latency_p95_ms: number\|null, alerts_fired: integer, sends: integer}` | no | Serve |
| `ingest_per_hour` | integer | no | Ingest rate |
| `stale` | `{since: integer, reason: string}` | yes: null = fresh | Stale banner: > 2 min since the last rollup flush (§10.9.6) |
| `segments` | object | no | Level 4; shape sketched below, **fields fixed in B11a/B11b** |
| `exceptions[]` | `{kind: string, severity: "warning"\|"error", value: number\|null, unit: string\|null, since: integer, fix: {kind: "settings"\|"stage", target: string}}` | no (may be empty) | Level 5; `kind` ∈ `yield_below_baseline`, `nat_saturation`, `ephemeral_ports`, `port_unreachable`, `scrape_bans`, `index_lag`, `parser_confidence`, `disk_projection`, `cohort_stalled`, `network_path`; **names fixed in B11a/B11b** |

Comparison-period fields mirror the current ones under `previous` (same
shape), present only when `compare=1`.

**`segments` sketch (deferred to B11a/B11b):**

```
segments: {
  by_group:        {key: string, searchable: integer}[],   // event time
  by_content_type: {key: string, searchable: integer}[],   // event time
  by_strategy:     {key: "bep51"|"passive"|"active", harvested: integer, searchable: integer, yield: number|null}[],  // cohort, final only
  by_ip_family:    {key: "v4"|"v6", harvested: integer, searchable: integer, yield: number|null}[],          // cohort, final only
  resolved_by:     {key: "files"|"folder"|"structure"|"plugin", count: integer}[]                         // event time
}
```

Rows are sorted by the server (descending value, then key).
