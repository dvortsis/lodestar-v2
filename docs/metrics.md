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
| **Sampled** | Computed on a uniform sample (facets above 1M matches, or above 300k when the index has a segment of more than 300k torrents) | `≈` + value, plus a note once per panel: "Counts are estimated from a sample" (glossary) | `{ value, approx: true }`; the panel reads `sampled` from the response |
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
| **Seeders** | **Max across sources within the freshness window** (§10.12): the highest seeders count among the sources that answered within the window. The window is the **last answered round**: one scrape round asks every tracker at once. The BEP 33 DHT estimate is **not** a source in the max. It stands in only when no tracker reports (s + l + c > 0), and then `seeders_approx` is true. | `formatCount` | **Null = never scraped → `—`** (and "Never scraped" in the drawer). 0 = scraped, nobody seeding → `0`. ≈ only if the server sets `seeders_approx` (the value came from the DHT estimate alone). | Per-torrent (`last_scrape_at`) |
| **Leechers** | As seeders, for leechers | `formatCount` | As seeders | Per-torrent |
| **Completed** | Max `completed` (downloaded) across reporting trackers; a coarse age hint (§6) | `formatCount` | Null = never scraped | Per-torrent |
| **Trackers reporting** | n of m: n = trackers whose answer had **s + l + c > 0** (opentracker rule, §15), less a suspect outlier; **m = trackers that answered in the last answered round** (`SwarmUpdate.trackers_checked`). Trackers that didn't answer are in neither number. | `formatCount` for each; row `4 / 20`; drawer "Reported by 4 of 20 trackers checked" | Null = never scraped → `—`. `0 / 20` is a real zero. A round where nothing answered (no tracker, no DHT) changes nothing, so n and m keep the last answered round's values. A round only the DHT answered gives `0 / 0` with approximate counts. | Per-torrent |
| **DHT estimate** | BEP 33 bloom-filter estimate of seeds and peers from up to 8 closest responders | `formatCount` | **Always ≈** ("≈ 1,225 seeds"; labelled "estimate"). Null = no estimate (no DHT answer, or no responder stores the hash: "unknown", not "dead") | Per-torrent |
| **Scrape age** | `now − last_scrape_at` (time of the last *answered* round) | `formatRelative` (row focus/hover), `formatDateTime` (drawer) | Null = never scraped | Per-torrent |
| **Swarm trend** | Least-squares slope of the last 7 daily seeders samples, seeders/day; needs ≥ 3 samples | `formatDelta` + "/day" (message) | Null = fewer than 3 samples → "Not enough history" | Per-torrent (daily ring) |
| **Swarm history** | 14 daily + 26 weekly (seeders, leechers) samples, newest first | Axis: `formatCount`, `formatDate`; tooltip `formatDateFull` | Gap samples are `null` → line gaps | Per-torrent, UTC day/week |
| **Suspect swarm** | One tracker reports a swarm the DHT and other trackers don't see (fake-seeding signature, §8) | Label "Suspect seeders" + icon | Boolean | Per-torrent |
| **Discovered** | `discovered_at`: when *we* first saw the key (§6) | `formatRelative` in rows, `formatDateTime` in the drawer | Never null. If the `backfill` flag is set, add the marker "First seen during backfill" | Per-torrent |
| **Last seen** | `last_seen_at`: latest DHT sighting; SQLite is at most 1 h behind the hot column; the API serves the later of the two | `formatRelative` / `formatDateTime` | Never null for stored torrents | Per-torrent |
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
| Notifications delivered (`notifications_delivered`) | Notifications a channel accepted, counted once per notification, after any retries | `formatCount` | Exact | Event time |
| Notifications failed (`notification_failures`) | Notifications that failed, after any retries | `formatCount` | Exact | Event time |

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
**M4** = served since M4's second PR (slices 4–6; the store landed with
#32); **planned** = named here and in the build plan
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
| `last_seen_at` | integer | no | Last seen | M4 |
| `flags` | string[] | no | `private`, `v2`, `hybrid`, `padding`, `symlinks`, `imported`, `backfill`, `files_partial`, `single_file`, `warnings` | main |
| `content_type` | string | yes | `movie`, `tv`, `music`, `audiobook`, `ebook`, `comic`, `software`, `game`, `images`, `video`, `other`, `adult`; null = not classified | main on search hits (B5a); planned on detail |
| `adult_kind` | string | yes | `scene`, `movie`, `photos`, `comics`, `game`, `vr`, `audio`; non-null only when `content_type = "adult"` | main on search hits (B5a); planned on detail |
| `composition` | `{group, files, bytes, pct_bytes, pct_files}[]` | no (may be empty) | Composition; `group` ∈ `video`, `audio`, `software`, `documents`, `images`, `archives`, `other`; `files`/`bytes` exclude padding; `pct_*` are whole percents (0–100) of non-padding bytes/files, rounded as `Composition::pct_bytes`; groups in the bar's order (§10.18), groups with no files left out | main on search hits (B5a); planned on detail |
| `junk_score` | integer 0–100 | yes: null = not classified | Junk score | main on search hits (B5a); planned on detail |
| `junk_reasons` | string[] | no | Stable reason keys (§6 M3) | main on search hits (B5a); planned on detail |
| `hidden_by_filter` | boolean | no | Hidden by a §20 filter rule | planned |
| `why` | string[] | no | Filter reason keys; empty unless hidden | planned |
| `seeders` | integer | **yes: null = never scraped** | Seeders | M4 |
| `leechers` | integer | yes (as seeders) | Leechers | M4 |
| `completed` | integer | yes (as seeders) | Completed | M4 |
| `seeders_approx` | boolean | no | True when no tracker reported and the DHT estimate supplied the counts (signed off in M4: the DHT stands in, it's never part of the max) | M4 |
| `trackers_reporting` | integer | yes (as seeders) | Trackers reporting (n); a suspect outlier isn't counted, and with only it and the DHT the DHT stands in (`seeders_approx`) | M4 |
| `trackers_checked` | integer | yes (as seeders) | Trackers checked (m): trackers that answered in the last answered round | M4 |
| `last_scrape_at` | integer | yes (as seeders) | Scrape age | M4 |
| `swarm_suspect` | boolean | no | Suspect swarm (`swarm_flags::SUSPECT`) | M4 |
| `score` | number | no | Relevance (hits only) | main |
| `matched_files` | integer[] | omitted unless the query has a file clause | Matched-file positions | main |

"Never scraped" means: no `swarm` row, or a row with `trackers_checked = 0`
and no DHT estimate. The six nullable swarm fields (`seeders`,
`leechers`, `completed`, `trackers_reporting`, `trackers_checked`,
`last_scrape_at`) are then all `null` together.

### 7.2 Detail-only (swarm section)

| Field | Type | Nullable | Metric | Status |
|---|---|---|---|---|
| `dht_seeds` | integer | yes: null = no estimate | DHT estimate (always ≈) | M4 |
| `dht_peers` | integer | yes (as `dht_seeds`) | DHT estimate | M4 |
| `swarm_trend` | number (seeders/day) | yes: null = < 3 daily samples | Swarm trend | M4 |
| `swarm_trackers` | `{url, seeders, leechers, completed, at}[]`; `url` never null | no (may be empty) | Per-tracker table; reporting trackers only, as of the last answered round. `at` is when that tracker answered | M4 |
| `swarm_history` | `{daily: Sample[14], weekly: Sample[26]}` | yes: null = no ring kept | Swarm history; newest first | M4 |
| `Sample` | `{at: integer, seeders: integer\|null, leechers: integer\|null}` | — | `at` = start of the UTC day or week; null counts = gap (`0xFFFF`). Days are Unix epoch days (UTC); a week is epoch-day / 7, so **weeks start on Thursday 00:00 UTC**. Counts saturate at **65,534**: show 65,534 as `≥ 65,534` | M4 |
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

### 7.5 Insights (B11a)

`GET /insights/overview?range=24h|7d|30d|all|custom&from=&to=&compare=0|1`
(default `7d`) and `GET /insights/events?from=&to=`. All **main** (B11a).
Ranges are normalized to whole UTC hours ending with the current one, or
to whole UTC days beyond 7 days or when the range reaches a compacted day
(B11b, §7.7); `custom` takes Unix seconds and stops at the current hour. A
bad `range`, `from`, `to` or `compare` is a `400` with `field`. Every
number comes from the rollups (`rollup_hour`, `cohort_hour`, their daily
twins `rollup_day` and `cohort_day`, `events`) or a snapshot the process
keeps; none from `torrents` or the index.

| Field | Type | Nullable | Metric | Time base |
|---|---|---|---|---|
| `range` | `{bucket: "hour"\|"day", from: integer, to: integer, tz: "UTC"}` | no | Context bar; `bucket` is the trend's and funnel's: `day` past 7 days, and for any range reaching a compacted day (B11b); with Compare, both periods share it | — |
| `updated_at` | integer | no | "Last updated" | — |
| `stages.discover` | integer | no | Stage strip: first harvests in range (cohort harvested counts). Always ≈ | Event time (= cohort hour) |
| `stages.fetch` | integer | **yes: null when the range starts before fetches were counted by event time** (`insights_state.funnel_events_since_hour`, the first whole hour after the V11 upgrade): unknown, never 0 | Stage strip: metadata fetched in range (`metric::FUNNEL_EVENTS`, dim `Fetched`; B11b) | Event time (the writer's flush, within 5 s) |
| `stages.understand` | integer | no | Stage strip: newly stored (`metric::STORED`) | Event time |
| `stages.index` | integer | yes: null when search is off | Stage strip (= `kpi.searchable`) | Snapshot |
| `stages.serve` | integer | **yes: null until #48's search counters are read** | Stage strip | Event time |
| `kpi.searchable` | integer | yes: null when search is off | Searchable torrents (`/stats`'s `searchable_torrents`) | Snapshot |
| `kpi.searchable_delta` | integer | yes: null without Compare, for `all`, with search off, or with no snapshot from the previous period's end | Δ: `kpi.searchable` less `previous.kpi.searchable` (removals included) | Snapshot |
| `kpi.imported` | integer | yes: null until the recorder's first read at start | Imported total: searchable torrents that were imported (`insights_state.imported`, kept exact by the store since B11b) | Snapshot |
| `kpi.new_searchable` | integer | no | New searchable in range, imports included; 0 is real | Event time |
| `kpi.yield` | number 0–1 | yes: null = no final cohort | End-to-end yield over final cohort hours; imports excluded | Discovery cohort |
| `kpi.new_searchable_delta` | integer | yes: null without Compare (and for `all`) | Δ: `kpi.new_searchable` less `previous.kpi.new_searchable` (B11b) | Event time |
| `kpi.yield_delta` | number −1–1 | yes: null without Compare or when either yield is null | Δ in ratio points: `kpi.yield` less `previous.kpi.yield` (B11b) | Discovery cohort |
| `kpi.time_to_searchable_p50_delta_s`, `kpi.time_to_searchable_p95_delta_s` | integer (seconds) | yes: as `yield_delta` | Δ of each percentile (B11b). Always ≈ | Discovery cohort |
| `kpi.time_to_searchable_p50_s`, `kpi.time_to_searchable_p95_s` | integer (seconds) | yes (as yield) | Time to searchable over final cohort hours: the midpoint of the log bucket (edges ×1.1, within 5%) holding the rank. Always ≈ | Discovery cohort |
| `kpi.health` | `{state: "running"\|"paused"\|"degraded", reason: "paused"\|"writer_stalled"\|"index_lag"\|null}` | no | Health: paused first, then stale, then index lag > 30 s | Snapshot |
| `trend[]` | `{at: integer, searchable: integer\|null, harvested: integer\|null}` | no | Movement, one point per bucket from `range.from`. A count is **null where nothing was measured** (no coverage row: the crawler paused, or the server not running) and 0 where it was measured and was 0 | Event time |
| `annotations[]` | `{at: integer, kind: string, text: string}` | no | At most the oldest 1,000 (`/insights/events` sets `truncated` when there were more). `kind` ∈ `restart`, `stopped`, `paused`, `resumed`, `intensity`; other event codes are skipped. `text` is the event's detail (the version, the new intensity), possibly empty | Event time |
| `funnel[]` | `{cohort: integer, final: boolean, stages: {harvested, peer_found, fetched, stored, searchable: integer}, conversion: {peer_found, fetched, stored, searchable: number\|null}, losses: {<reason>: integer}, in_progress: integer}` | no | Funnel by cohort (hours up to 7 days, UTC days beyond; a day is final when all its hours are), cohorts with any count only. `losses` lists every reason (§6.4), 0 where none. `conversion` is each stage ÷ the one before, null when that is 0. `in_progress` = harvested − stored − lost, at least 0 | Discovery cohort |
| `funnel_total` | `{cohorts, final_cohorts, in_progress: integer, stages, conversion, losses, loss_share: {<reason>: number\|null}}` | no | The funnel over the whole range; `loss_share` is each reason's share of all losses, null when nothing was lost | Discovery cohort |
| `serve` | `{searches: integer\|null, zero_result_rate: number\|null, latency_p95_ms: number\|null, alerts_fired: integer\|null, sends: integer\|null}` | no | Serve. **Every field null until its counter exists:** searches and latency with #48, alerts with B9, sends with the integrations' counters | Event time |
| `ingest_per_hour` | integer | **yes: null when the last full hour wasn't measured and stored nothing** | Ingest rate: newly stored in the last full UTC hour | Event time |
| `stale` | `{since: integer, reason: "paused"\|"writer_stalled"}` | yes: null = fresh | Stale banner: > 120 s since the writer's last flush (`since`; the pipeline's start when there was none) | — |
| `segments` | object, below | no | Level 4 | — |
| `exceptions[]` | `{kind: string, severity: "warning"\|"error", value: number\|null, unit: string\|null, since: integer, fix: {kind: "settings"\|"stage", target: string}}` | no (may be empty) | Level 5, whatever the range. `kind` ∈ `index_lag` (> 30 s; an error past 600 s; `unit` `s`; fix: stage `index`) and `cohort_stalled` (hashes in progress, the crawler running ≥ 3,000 s in each of the last 2 full hours, and nothing stored in them; `unit` `torrents`; fix: stage `fetch`). `yield_below_baseline` (B11b): end-to-end yield of the newest 24 cohort hours final by age (72–96 h ago) below 75% of the 7 days of cohort hours before them (an error below 50%), when the recent hours harvested ≥ 500 keys and the baseline ≥ 3,500; `value` the recent yield, `unit` `ratio`, `since` the recent hours' start; fix: stage `fetch`. Still waiting for a counter: `nat_saturation` (a DHT reply-ratio history), `ephemeral_ports` and `port_unreachable` (socket counters), `scrape_bans` (#48's `SCRAPE_REQUESTS`), `parser_confidence` (a confidence rollup), `disk_projection` (free space), `network_path` (a FUSE/network-path probe) | Snapshot |
| `previous` | `{range, stages: {discover, fetch, serve, understand}, kpi: {searchable, new_searchable, yield, time_to_searchable_p50_s, time_to_searchable_p95_s}, trend[], serve, segments}` | **absent** without `compare=1` and for `all` | The period of equal length just before, for Compare. `previous.kpi.searchable` is the last hourly snapshot of the searchable count (`metric::SEARCHABLE`) in the day before the range starts (a compacted day's is its last hour's), null when there's none. `previous.stages.fetch` is null as `stages.fetch`. `previous.segments` is the previous period's `segments`, same shape and Adult rule (B11b) | As the current fields |

**Finality:** a cohort hour is final once it has ended with nothing in
progress (harvested = stored + lost) and everything stored searchable, or
once more than the 72 h retry horizon has passed since it ended.

**`segments`** (rows sorted by value, descending, then key; **Adult left
out entirely while "Show adult content" is off**):

```
segments: {
  by_content_type: {key: string, searchable: integer}[],  // event time; key = content type, or "unclassified"
  by_group:        {key: string, searchable: integer}[],  // event time; video (movie, tv, video), audio (music,
                                                          // audiobook), documents (ebook, comic), software
                                                          // (software, game), images, other, adult, unclassified
  by_source:       {key: string, harvested: integer, searchable: integer, yield: number|null}[]
                                                          // discovery cohort, final cohort hours; key = plugin name
}
```

Still waiting: `by_strategy` and `by_ip_family` here (the cohort table
has no strategy or address-family dimension yet; Discover's
`by_strategy` is volume by event time) and `resolved_by` (needs a
resolution counter).

**`GET /insights/events`:** `{annotations: Annotation[], paused: {from:
integer, to: integer|null}[], truncated: boolean}`. `paused` lists the
paused spans overlapping the range (one starting before it begins at its
pause; `to` null while still paused), so the client draws gaps.
`annotations` is the same list as the overview's, the oldest 1,000
(`truncated` when there were more).

### 7.6 Stats, crawler and network check (B14)

`GET /stats` feeds the global bar and the status pill; `GET /crawler` the
bar's Pause/Resume and intensity control; `GET /network/check` the
first-run wizard's step 3 and the dashboard's "reachable" flag;
`GET /sources` Settings → Sources (§18.8). Every count reads the counter
`/metrics` (or the status page) shows; rates are the server's, from
samples of those counters every 10 s. All **main** (B14).

| Field | Type | Nullable | Unit / meaning | Formatter | Time base |
|---|---|---|---|---|---|
| `stats.searchable_torrents` | integer | yes: null = search is off | Documents in the index (`lodestar_search_docs`) | `formatCompact` | Snapshot |
| `stats.stored_torrents` | integer | no | Torrents stored (`lodestar_torrents`) | `formatCompact` | Snapshot |
| `stats.harvested_per_hour` | number | yes: null in the first minute after start or a resume, and while paused (a gap, not a zero); after a resume the 5 min window starts at the resume | Keys the sources reported (`lodestar_intake_received_total`), per hour over the last 5 min | `formatRate` | Event time |
| `stats.fetched_per_hour` | number | yes (as `harvested_per_hour`) | Info dicts fetched (the pool's `fetched`, as on the status page), per hour over the last 5 min | `formatRate` | Event time |
| `stats.queue_depth` | integer | no | Keys queued or in flight in the fetch pool (`lodestar_fetch_pending`) | `formatCount` | Snapshot |
| `stats.index_lag_s` | integer (s) | yes: null when search is off, or the index is behind and hasn't committed since the start (a backlog after a restart) | 0 when everything stored is searchable (the index watermark ≥ the store's highest torrent id); while behind, seconds since the index last committed (the indexer's `last_commit`), so a stalled index shows a growing lag | `formatSpan` | Snapshot |
| `stats.data_bytes` | integer (bytes) | yes: null when the directory can't be read | Bytes in the data directory (database, index, state), measured at most every 30 s | `formatBytes` | Snapshot |
| `stats.disk_free_bytes` | integer (bytes) | yes: **always null in this build** (std has no free-space call; see §16 B14) | Free bytes on the data directory's volume | `formatBytes` | Snapshot |
| `stats.uptime_s` | integer (s) | no | Seconds since the pipeline started (`lodestar_start_time_seconds`) | `formatSpan` | Snapshot |
| `stats.paused`, `crawler.paused` | boolean | no | The crawler is paused | — | Snapshot |
| `stats.updated_at` | integer | no | When the numbers were read | `formatDateTime` | — |
| `crawler.state` | string | no | `running` / `paused` (icon + text, glossary) | — | Snapshot |
| `crawler.since` | integer | no | Last pause or resume; the start when running since then | `formatRelative` | — |
| `crawler.intensity` | string | no | `low` / `balanced` / `aggressive` (`/settings/crawler`'s `intensity`) | — | Snapshot |
| `crawler.intensity_env` | string | yes: null = editable | The variable that sets it | — | — |
| `crawler.dht` | `{bootstrapped: boolean, outstanding: integer, routing_nodes: integer}` | yes: null = DHT off or not started | Routing-table size, queries awaiting an answer (`lodestar_dht_*`) | `formatCount` | Snapshot |
| `crawler.fetch` | `{in_flight, lookups, queued: integer}` | no | Fetches running, peer lookups out, keys waiting | `formatCount` | Snapshot |
| `network.reachable` | boolean | **yes: null = unknown** (DHT off, bootstrapping, or under `min_window_s` since bootstrap or a restart) | Unsolicited inbound DHT queries arrive on the DHT port | Text + icon | Snapshot |
| `network.reason` | string | no | `dht_off`, `bootstrapping`, `measuring`, `unsolicited_queries` (≥ 3 networks), `few_unsolicited_sources` (1–2 networks: not enough), `no_unsolicited_queries` | Text (glossary) | — |
| `network.inbound_queries`, `network.unsolicited_queries` | integer | yes: null until measuring | Queries received in the window, and those from nodes we hadn't sent anything to lately | `formatCount` | Event time (the window) |
| `network.unsolicited_sources` | integer | yes: null until measuring | Distinct networks (/24 or /64) the unsolicited queries came from, at most the 64 most recent; `reachable` needs ≥ 3 | `formatCount` | Event time (the window) |
| `network.measured_s`, `network.min_window_s` (120), `network.window_s` (600) | integer (s) | no | Seconds of evidence; the least before a verdict; the longest looked at | `formatSpan` | — |
| `network.window_started_at` | integer | yes: null before bootstrap | Bootstrap or the last restart | `formatDateTime` | — |
| `network.external` | `{ip: string\|null, port: integer\|null, voters: integer, agreeing: integer}` | ip/port yes: null until ≥ 5 voters and a majority agree | Our address as other nodes report it (BEP 42 `ip`): one vote per /24 or /64, the last 32 voting networks, IPv4 and IPv6 apart (IPv4 wins when both agree on one) | `formatCount` for the counts | Snapshot |
| `network.mapping` | string | no | UPnP / NAT-PMP: `unavailable` (not in this build) | Text | — |
| `sources[].health.harvested_24h`, `.stored_24h` | integer | no | Keys this source was first to report in the last 24 discovery cohorts, and of those, stored | `formatCount` | Discovery cohort |
| `sources[].health.last_run` | integer | yes: null = not started this run | When it last started | `formatRelative` | — |
| `sources[].health.last_error` | string | yes: null = no error | Why it last stopped | Text | — |

### 7.7 Insights at daily grain, removals and repair (B11b)

All **main** (B11b, two PRs): daily grain, removals and repair (part 1);
the stage pages, `export.csv` and the `insights` event (part 2, below).

**Grains.** Hourly rows (`rollup_hour`, `cohort_hour`) are kept for 30
days. A daily job folds whole UTC days that ended more than 30 days ago
into `rollup_day` and `cohort_day` (`day` = Unix time / 86,400). Readers
take the boundary (`insights_state.compacted_before_day`) in the same
read transaction as the rows, so a running compaction never counts a day
twice or drops one. Days below it are read at daily grain (a compacted
day is final), so any range reaching one is bucketed by day. With
Compare, when the previous window reaches a compacted day and the
current one doesn't, both are bucketed by day: a custom range of 7 days
or less then widens to whole UTC days. Late rows
below the boundary (a clock set back, say) are read at once and folded
the next night. Compaction loses nothing: every response over compacted
days equals the one before compaction (§10.17, byte goldens).

**Aggregation kinds** (`lodestar_pipeline::metric::aggregation`; every
code has one, and a code with none is never compacted):

| Code | Metric | Kind | Day value |
|---|---|---|---|
| 1–4 | harvested, rediscovered, dropped, stored | Sum | the hours' sum |
| 5–9 | #48's scrape and search counters | Sum | the hours' sum |
| 10 | `NEWLY_SEARCHABLE` | Sum | the hours' sum |
| 11 | `TIME_TO_SEARCHABLE` (by cohort) | Sum | the cohort day's histogram |
| 12 | `COVERAGE` | Sum | seconds measured in the day |
| 13 | `SEARCHABLE` (gauge) | Last | the day's last hourly snapshot, never a sum |
| 14 | `FUNNEL_EVENTS` | Sum | the hours' sum |
| 15 | `REMOVED` | Sum | the hours' sum |
| cohort rows | every stage and loss | Sum | the cohort day's counts |

**New codes.**

| Code | Name | Dim / key | Written by | Time base |
|---|---|---|---|---|
| 14 | `FUNNEL_EVENTS` | dim = `Stage` code, key = `Loss` code (0 for a stage reached) | the writer, from the funnel rows it flushes, at the flush hour | Event time (within the 5 s flush) |
| 15 | `REMOVED` | dim = content type, or 255 `UNCLASSIFIED`; key 0 | the store's `torrents_removed` trigger, for a searchable torrent deleted by any path | Event time (the delete) |

**`insights_state`** (one row each): `compacted_before_day`,
`last_compaction_at` (Unix seconds, 0 = never), `funnel_events_since_hour`
(the first hour metric 14 counts; buckets before it are unknown, never 0; 0 on a store created with V11)
and `imported` (searchable imports: seeded at the V11 upgrade, raised by
each searchable range, lowered by the trigger).

**Removals** are lifecycle events (§10.9.3): a searchable torrent deleted
(a filter relabel, `delete_torrents`, retention) is counted in
`REMOVED`; the funnel's final cohorts never change. The Index page reads
them (part 2).

**`lodestar rollup --repair`** (offline; also run by the server at start
after an unclean stop, over the 72 h before the store's last activity)
recounts from `torrents` and `index_watermark_hour`, and never lowers a
total (a per-type or per-source cell in a replaced hour follows the
current classification; a raised Searchable cell replaces its cohort's
histogram with midpoint approximations):

| Rebuilt | From |
|---|---|
| Cohort Stored, by (cohort, source) | torrents' `discovered_at` hour and `first_source`, imports excluded |
| `STORED` per hour, by source | `metadata_at` hour, imports excluded (the writer counts at the same hour since B11b) |
| `NEWLY_SEARCHABLE` per hour, by type and import | the first watermark row covering each id |
| Cohort Searchable and its `TIME_TO_SEARCHABLE` histogram | the same, the delay taken to the watermark hour's midpoint (≤ 30 min off) |
| `imported` (the command only) | `count_flagged_searchable(IMPORTED)` |

**Never rebuilt:** pre-storage counts (harvested, peer found, fetched,
losses, keys reported, rediscovered, dropped, `FUNNEL_EVENTS`), coverage,
gauges (`SEARCHABLE`) and torrents indexed before searchable time was
tracked (hour 0). After compaction, hourly detail older than 30 days is
gone: hour buckets, hourly finality and hourly repair.

#### Stage pages (§10.9.5)

`GET /insights/{discover|fetch|understand|index|serve}?range=&from=&to=&compare=&reason=`.
The overview's range rules and `400`s; `reason` is a §6.4 loss name
(else `400` with `field: "reason"`), accepted on every page and read by
Fetch only (the others ignore it, and share a cache entry). Each answers `{range, updated_at, stale, kpi, trend[],
<sections>}`, plus `previous: {kpi, range, trend}` with `compare=1` (its
snapshot fields null). Rollups and snapshots only; cached per (query,
stage, reason, last flush). A trend count is **null where nothing was
measured** (no coverage row), and fetch, loss and block counts are also
null before `funnel_events_since_hour`. Adult is left out of every
per-type list while hidden (totals count it, as on the overview).

| Page | Field | Source | Time base |
|---|---|---|---|
| Discover | `kpi.reported`, `.rediscovered`, `.dropped`; `trend[].reported`…; `by_source[]` | metrics 1, 2, 3 (dim = source) | Event time |
| Discover | `kpi.harvested`, `trend[].harvested` | cohort harvested (first harvests, ≈) | Discovery cohort |
| Discover | `by_strategy[]`: the DHT's `sample`, `get_peers`, `announce`, `lookup`, `refetch` from any source, otherwise `<plugin>:<n>` (that plugin's own code) | metric 1's key: volume, not yield | Event time |
| Discover | `kpi.dht_nodes`, `kpi.reply_ratio` | the DHT now (`/crawler`'s routing nodes; `/metrics`' `dht_reply_ratio`, since start, null before the first query) | Snapshot |
| Discover | `kpi.bep51_sample_yield`, `.keyspace_coverage`, `.seen_filter_load`; `by_ip_family` | **null** until the DHT counts samples / reports coverage and the filter's load; until leases carry the address family | — |
| Fetch | `kpi.fetched`, `.lost`; `trend[]`; `losses[]` (count and share of all losses, by reason) | metric 14 (dims `Fetched`, `Lost`; `reason` filters `lost` and `losses`) | Event time |
| Fetch | `funnel[]`, `funnel_total` | the overview's cohort funnel (`reason` keeps only that loss) | Discovery cohort |
| Fetch | `kpi.queue_depth`, `.in_flight` | `/stats`' `queue_depth`, `/crawler`'s `fetch.in_flight` | Snapshot |
| Fetch | `kpi.queue_age_s`, `.utp_share`, `.mse_share`, `.fetch_latency_p50_ms`/`_p95_ms`, `.limiter_rejections`, `.ephemeral_port_pressure` | **null** until the pool reports queue age, transport and encryption outcomes, a latency histogram, limiter and port counters | — |
| Understand | `kpi.stored`, `trend[].stored` | metric 4 | Event time |
| Understand | `kpi.blocked`, `trend[].blocked` | metric 14, `Lost` × `Blocked` (counts only) | Event time |
| Understand | `kpi.newly_searchable`, `by_content_type[]`, `by_group[]` | metric 10 | Event time |
| Understand | `kpi.parser_confidence`, `.junk_distribution`, `.resolution_rate`, `.recent_corrections` | **null** until confidence and junk rollups, a resolution counter and a corrections counter | — |
| Index | `kpi.searchable`, `.index_lag_s`, `.data_bytes`, `.disk_free_bytes` (always null), `.rebuild` (`building`/`finishing`/`swapping`/`failed`, null with none) | `/stats`; the serving handle | Snapshot |
| Index | `kpi.new_searchable`, `kpi.removed`, `removed_by_type[]`; `trend[].new_searchable`, `.removed` | metrics 10, 15 | Event time |
| Index | `trend[].searchable` | metric 13, the bucket's last snapshot (null with none) | Snapshot per bucket |
| Index | `kpi.sizes`, `.projection_days`, `.segments`, `.merges`, `.file_index_coverage`, `.scrape_coverage`, `.scrape_freshness_s`; `trackers` | **null** until per-part sizes, free space, index segment and merge counts, file-index coverage and swarm rollups are measured | — |
| Serve | every `kpi` field, `trend[]`, `top_queries`, `top_zero_result_queries` | **null** until #48's search counters (searches, zero results, latency), cancel and facet-sampling counters, B9's alert counts, notification and send counters, and a query-logging setting (logging is off by default and nothing writes the log yet) | — |

#### `GET /insights/export.csv?panel=&range=&from=&to=&compare=&reason=`

`panel` ∈ `trend`, `funnel`, `losses`, `segments.by_group`,
`segments.by_content_type`, `segments.by_source`, `exceptions`,
`annotations` (the overview's) and `discover.trend`, `discover.by_source`,
`discover.by_strategy`, `fetch.trend`, `fetch.losses`, `fetch.funnel`,
`understand.trend`, `understand.by_content_type`, `understand.by_group`,
`index.trend`, `index.removed_by_type`, `serve.trend`; anything else is a
`400` with `field: "panel"`. The rows are the panel endpoint's own JSON
(the same builder and cache entry). RFC 4180, `\r\n`, a header of field
names: `period` (`current`, or `previous` with `compare=1` where the panel
has one), then the fields sorted, nested objects as `parent.child`, and
each time (`at`, `cohort`, `since`) followed by an ISO-8601 `…_utc`. A
panel's header is the same whether or not it has rows. Numbers raw (ratios
0–1, times in Unix seconds), **null empty, never 0**; text through
`export::csv_cell` (`'` before a leading `=`, `+`, `-`, `@`, tab or CR;
controls and bidi formatting as `\u{…}`; quoted with a comma or a quote).
Headers: `text/csv; charset=utf-8; header=present`, `attachment;
filename="lodestar-insights-<panel>-<from>-<to>.csv"`, `no-store`,
`nosniff`. `reason` is checked as on the stage pages and filters the
`fetch.*` panels as Fetch does.

#### `event: insights`

Every 30 s while anyone subscribes: `{stages, updated_at}`, the default
`7d` overview's stage strip; `stages.fetch`, `.index` and `.serve` are
nullable as in §7.5. A client showing another range refetches its own on
each event. The overview is built only while a stream with the topic (or
with no `topics`, which means all) is open.

### 7.8 Digest preview (B11c)

`GET /digest/preview?period=daily|weekly` (default `weekly`), **main**
(B11c). Behind a session. One `overview` read with `range=24h` (daily) or
`7d` (weekly) and `compare=1` under the request's "Show adult content",
rendered in English as plain text and simple HTML (§10.9.7). A bad `period`
is a `400 bad_parameter` with `field: "period"`; `503 insights_unavailable`
without Insights. JSON only: the client shows `html` only in an
`<iframe sandbox srcdoc>` (no `allow-scripts`, no `allow-same-origin`) and
`text` in a `<pre>`.

| Field | Type | Nullable | Notes |
|---|---|---|---|
| `generated_at` | integer | no | The overview's `updated_at` |
| `html` | string | no | A complete document, ≤ 96 KiB: no script, style, image, external URL, event handler or colour; `style="text-align:right"` on number cells is the only style |
| `period` | `"daily"\|"weekly"` | no | |
| `range` | as `overview.range` | no | The range the digest read |
| `subject` | string | no | ≤ 120 characters: `Lodestar daily digest: Oct 22, 2026` or `Lodestar weekly digest: Oct 15, 2026 – Oct 22, 2026` (`formatDate`, UTC) |
| `text` | string | no | Plain UTF-8, `\n` line ends, ≤ 4,000 bytes |

What the digest prints, from the overview and nothing else (no sums, rates,
Δs or store reads here):

| Line | Field | Formatter | Null |
|---|---|---|---|
| Searchable torrents | `kpi.searchable`, Δ `kpi.searchable_delta` | `formatCount`, `formatDelta` | `—` |
| New searchable | `kpi.new_searchable`, Δ `kpi.new_searchable_delta` | `formatCount`, `formatDelta` | (never null) |
| Yield | `kpi.yield`, Δ `kpi.yield_delta` | `formatPercent` f = 1, `formatDelta` percent | `— (No completed cohorts yet)` |
| Time to searchable | `kpi.time_to_searchable_p50_s` and `_p95_s`, Δs `…_delta_s` | `≈` + `formatSpan`, `formatDelta` in `formatSpan`'s unit | `— (No completed cohorts yet)` |
| Health | `kpi.health.{state, reason}` | text | no reason: the state alone |
| Funnel | `funnel_total.{cohorts, final_cohorts, stages, conversion}` | `formatCount` (Harvested `≈`), `formatPercent` f = 1 | a null conversion prints `—` |
| Biggest losses | the three largest non-zero `funnel_total.loss_share` | `formatPercent` f = 0 | none: "No losses." |
| Top movers | `insights::segment_movers(segments, previous.segments, 3)` | `formatDelta` | no `previous`, or none moved: the section is left out |
| Needs attention | `exceptions[]` | a fixed phrase per known `kind`; its `value` in the `unit`'s formatter (`s` → `formatSpan`, `torrents` → `formatCount`, `ratio` → `formatPercent`) | an unknown kind: "Needs attention: <kind>." |

A Δ that is null prints nothing, and with no Δ at all the "vs previous …"
header is left off. A Δ shows its sign (U+2212 for minus) and `▲`/`▼`, never
colour (`<span aria-hidden="true">` in the HTML). Any string from the
response that isn't a constant (segment keys, exception kinds, a health
reason) is cleaned first: controls, bidi formatting characters and line
separators removed, cut to 64 characters with `…`. Links are root-relative:
`/insights?range=<24h|7d>&compare=1`, and an exception's `fix` as
`/insights/<target>` (`stage`) or `/settings/<target>` (`settings`) only
when `target` matches `^[a-z_]{1,32}$`.


## CR-0 DHT operational diagnostics (2026-10-25)

These Prometheus series are aggregate, without endpoint/hash labels. Gauges reflect the latest actor maintenance turn; counters are cumulative for the process.

| Series | Meaning |
|---|---|
| `dht_sample_cooling`, `dht_sample_inflight` | Cooldown obligations (also indexed expiry entries) and reserved transactions; their sum stays at most 200,000. |
| `dht_sample_frontier` | Queued sample candidates, at most 20,000. |
| `dht_sample_suppression_seconds` | Startup suppression remaining. Zero permits sampling; it does not promise spare capacity. |
| `dht_sample_deferred_total`, `dht_sample_stale_total` | Capacity deferrals and rejected sampler completion tokens. Deferrals are backpressure, not lost hashes. |
| `dht_peer_admitted_total`, `dht_peer_rejected_total` | Actor ownership accepted/rejected. Source-command channel rejection happens before actor admission and is not in this actor counter. |
| `dht_peer_active`, `dht_peer_queued` | Logical owned requests, across configured families; combined cap 32. |
| `dht_peer_cancelled_total` | Active requests cleaned up after receiver drop; queued drops never become active. |
| `dht_peer_queued_dropped_total{reason}` | Admitted requests dropped from the queue before they started: `caller_closed` (the caller dropped its receiver) or `paused` (the pause dropped them). Counted whether or not coordinated admission is on. |
| `dht_lookup_scheduled` | Lookup legs with a scheduled continuation, including those awaiting replies. |
| `dht_lookup_deadlines_total` | Lookup legs terminated at their deadline, including maintenance/scrapes. |

`dht_lookups_completed_total` counts terminal owned requests, including empty results; a request whose caller cancelled it counts only in `dht_peer_cancelled_total` (CR-0f). It is not a metadata-yield counter. Every admitted request ends in exactly one of `dht_lookups_completed_total` (owned), `dht_peer_cancelled_total` or `dht_peer_queued_dropped_total`, or is still counted by `dht_peer_active` or `dht_peer_queued`; requests still held at shutdown are the exception. The typed `dht.socket_buffers` startup event reports the existing 4 MiB receive-buffer request, `send_requested=0` (retain the system default), kernel readback for both buffers, and set/read errors per socket. Zero readback with `read_errors > 0` means unknown. OS accounting may differ; do not infer packet drops from grants. Kernel drop counts remain unavailable unless separately collected. The six-hour startup suppression must be excluded from a steady-state sample-yield comparison and reported separately.


## CR-0d restart proof (2026-10-30)

With `dht.safe_restart` on, a clean restart can resume sampling at once instead of waiting six hours, honouring every restriction the last run recorded (`docs/decisions/2026-10-30-CR-0d.md`). These series say which happened and why. They are aggregate, with no endpoint labels.

| Series | Meaning |
|---|---|
| `dht_sample_suppression_reason{reason}` | Gauge, 1 for this run's reason and 0 for the rest. `resumed` means the proof held. `off` means the switch is off or there's no data directory. Every other label means this run suppresses for the full window, or isn't sampling yet: `locked` (another process holds the data directory, or the filesystem has no locks), `first_start`, `corrupt`, `version`, `dirty` (the last run didn't stop cleanly), `off_run`, `data_dir`, `node_identity`, `foreign_run`, `snapshot_mismatch`, `no_witness`, `generation` (a restored backup), `horizon_changed`, `clock_backward`, `stale` (more than six hours since the last clean stop), `invalidate_failed` (a write the proof needs before sampling hadn't landed at start: nothing is sampled until it does, and then not before this run's start plus six hours). |
| `dht_restart_obligations_restored` | Restrictions seeded from the last clean stop; 0 when this run didn't resume. |
| `dht_restart_obligations_pending` | Restored restrictions still cooling. |
| `dht_restart_obligations_refused_total`, `dht_restart_obligations_honoured_total` | Candidates a restored restriction turned away, and restored restrictions that expired unsampled. Monitoring only: the proof that restrictions are honoured is the wire-level test, not these counters. |
| `dht_restart_persist_errors_total{step}` | Restart-proof write failures by step: `lock`, `witness`, `invalidate`, `state_bin`, `snapshot`, `marker`. Failures never make sampling less safe; they cost the next start its resume, or hold sampling until a retry lands. |

`dht_sample_suppression_seconds` reads 0 from the start of a resumed run, unless a restored blanket gate still holds. While sampling is held with no known end (`locked`, `invalidate_failed`), it never reads 0. Typed events: `dht.restart_check` at start (mode, reason, the previous marker, generation, counts), `dht.restart_commit` at shutdown (ok, duration, entries) and `dht.restart_persist_failed` (Warn, step). A resumed run's sample yield isn't comparable with a suppressed run's first six hours: report the reason with any A/B window.

## Hardened baseline diagnostics (October 27)

See [the diagnostic boundary contract](decisions/2026-10-27-soak-diagnostics.md).
New metrics preserve the existing scheduling and attempt definitions:

| Metric | Meaning |
| --- | --- |
| `dht_peer_rejection_reasons_total{reason}` | Exclusive `caller_closed`, `paused`, `capacity` partition of actor rejection |
| `dht_peer_ack_failed_total` | Ownership acknowledgment lost before queue insertion |
| `dht_peer_command_full_total`, `dht_peer_command_closed_total` | Handoff failed before actor admission |
| `fetch_finder_full_total`, `fetch_finder_closed_total` | Pool-to-finder handoff failures |
| `fetch_lookup_unaccepted_total` | Request finishes/times out without observed accepted ownership, including no finder channel |
| `fetch_retry_unaccepted_total` | Repeated entry-lifetime request with zero accepted attempts |
| `fetch_evicted_service_total{stage}` | Furthest known service: `unaccepted`, `lookup`, `peers`, `fetch` |

These are event counts, not unique hashes, network attempts or cohort conversion.
No endpoint/hash/query labels are introduced. Restored entries lack historical
peer/fetch flags; do not interpret eviction categories as complete cross-restart
history. All absent measurements remain unknown.


## Coordinated admission (CR-ADM, 2026-10-30)

These series count only with `dht.coordinated_admission` on ([the decision](decisions/2026-10-30-CR-ADM.md)); with it off they're exported and stay 0. A permit is one logical peer request's slot at one finder, covering every address family; the DHT has 32. These are event counts, not unique hashes, network attempts or cohort conversion. The existing admission counters keep their meaning.

| Series | Meaning |
|---|---|
| `fetch_admission_waits_total{kind}` | A key due a lookup that joined the permission queue instead of being granted at once. `kind` is `arrival` (a discovery or a restored key), `retry` (after back-off) or `requeue` (after a refusal, a lost acknowledgment or a pause drop). |
| `fetch_admission_wait_cancelled_total{reason}` | Waiters that left without a request: `evicted`, or `peers` (peers arrived first). No network attempt is made. |
| `fetch_admission_wakeups_total` | Times a permit returned while the pool was idle woke it. One wakeup can grant several waiters; a grant the pool makes on seeing a completion itself needs none. |
| `fetch_admission_grants_total{kind,class}` | Requests granted permits, by `kind` as above and `class` `interactive` or `background`. |
| `fetch_admission_refusals_total{reason}` | Requests that held permits but weren't accepted: `refused` (the actor answered no; its reason is in `dht_peer_rejection_reasons_total`) or `unacknowledged` (the handoff or acknowledgment was lost). Neither uses an attempt or a back-off. |
| `fetch_admission_holds_total` | 1 s holds on a finder after a refusal. |
| `fetch_admission_permit_returns_total{boundary}` | Permits returned in the pool: `finder_full` and `finder_closed` (granted, never delivered), and `unused` (never granted: kept back from a wakeup or a partial grant and given back on a hold, a pause, shutdown or an empty queue, or taken for a waiter that had run out of attempts). |
| `dht_peer_permit_returns_total{boundary}` | Permits returned in the actor: `completed`, `cancelled_queued`, `cancelled_active`, `paused`, `refused`, `ack_failed`, `handoff_failed`, `shutdown`. |
| `fetch_admission_waiters` | Gauge: keys waiting for a lookup now. |
| `fetch_admission_permits_in_use` | Gauge: permits granted requests hold now, across finders. |

With one finder (the DHT), `fetch_admission_grants_total` = `dht_peer_permit_returns_total` + `fetch_admission_permit_returns_total{boundary=~"finder_full|finder_closed"}` + `fetch_admission_permits_in_use`. A persistent gap means a leaked or duplicated permit; a brief one is a permit in transit between the pool and the actor. At shutdown, a request still in the DHT plugin's channel returns its permit without being counted.

## Crawler gate (CR-gate-metrics, 2026-10-30)

The owner's gate (`docs/owner-decisions.md` §2.25) counts **unique infohashes whose info dictionary was fetched and hash-verified from the network in this run, then committed as stored and searchable**. See [the decision](decisions/2026-10-30-CR-gate-metrics.md). Everything is counted at a commit point (the writer's transaction, the prepare stage's and the pool's terminal loss of a key, the index's commit), never per fetch attempt.

**The gate's numerator is `searchable_new_total{origin="network"}`:** verified per minute is `rate(lodestar_searchable_new_total{origin="network"}[1m]) * 60`. `metadata_new_verified_total` is the stored stage of the same records, for diagnosing index lag; it isn't the gate.

| Series | Type | Meaning |
| --- | --- | --- |
| `metadata_new_verified_total{origin}` | counter | New rows the writer committed (`Inserted::New`), once per key. `origin` is `network` today; `import` and `recovery` are reserved and not rendered until they're counted. |
| `searchable_new_total{origin}` | counter | Of those, records an index commit has reached (its watermark ≥ the row's id), once per key. A record can be counted one index commit late (see the decision). Stays 0 with search off. |
| `metadata_excluded_total{reason}` | counter | Outcomes that never count towards the gate, by `reason`. Once per key: `already_stored` (fetched and verified, but the store had it: refetched imports, have-filter misses; once per key except for the stale-lease race the decision describes), `filtered` (a drop rule, or the floor's hash list in prepare), `quarantined`, `invalid_metainfo` (hashed to the key but not usable metainfo), `failed_verification` (invalid: a hash mismatch, or a protocol or handshake violation, on the final attempt; the pool's terminal `Loss::Invalid`. A key whose last failure was something else is in `fetch_lost_total` under that reason), `store_failed` (its transaction failed). Event counts, possibly several per key: `rediscovered` (each sighting of a stored torrent, not fetched) and `duplicate` (a result under a lease no longer current; the current holder's result is counted on its own). |
| `discovery_to_searchable_seconds` | histogram | Whole seconds from the key's first sighting in this run (the lease's discovery time; for a key restored from the fetch queue, this run's start) to the index commit that made it searchable. Buckets `le`: 1, 2, 5, 10, 30, 60, 120, 300, 600, 1800, 3600, 7200, `+Inf`. `_count` equals the `+Inf` bucket. |
| `discovery_to_searchable_dropped_total` | counter | Latency measurements lost to a full bound: the writer's 4,096-entry hand-off or the 65,536-entry wait queue (oldest evicted). The record still counts in `searchable_new_total`, without a latency, once an index commit passes the highest id dropped. |

Imports, recovery and every excluded reason are separate series, so a stricter filter moves `filtered` or `quarantined`, never the numerator. Rows a rule only hides, demotes or flags are stored and indexed, so they count (see the decision). Keys lost for other reasons (no peers, time-outs) are in `fetch_lost_total{reason}`. Like the other `/metrics` series, these are process-lifetime and carry no hash or endpoint labels.

## Storage and cache sizes (R-gauges, 2026-10-30)

Added for the v0.5.1 Linux soak, so a memory or disk ramp can be explained (owner's ruling on #189). See [the decision](decisions/2026-10-30-R-gauges.md). The file sizes are sampled every 60 s by a background task; a scrape reads the last sample and never touches the disk. The have-filter and hot columns already have `have_bytes` and `hot_columns_bytes`.

| Series | Type | Meaning |
| --- | --- | --- |
| `store_db_bytes` | gauge | Size of `lodestar.db`. |
| `store_wal_bytes` | gauge | Size of `lodestar.db-wal`; 0 when there is none. |
| `search_index_bytes` | gauge | Regular files under `data/index`: every index version and any rebuild in progress. 0 with search off. Symlinks aren't followed; the walk stops after 100,000 entries (an error). |
| `storage_sampled_seconds` | gauge | Unix time of the last sample that read all three sizes; 0 before the first. A stale value means samples are failing. |
| `storage_sample_errors_total` | counter | Sizes that couldn't be read, one per size per sample. Each keeps its last value. |
| `search_file_cache_entries`, `search_file_cache_bytes` | gauge | `file:(…)` id sets the serving index caches, and the bytes they retain as the 8 MiB budget charges them. |
| `search_browse_cache_entries`, `search_browse_cache_bytes` | gauge | Per-segment sorted tops the serving index caches (bounded at 4,096 entries), and their estimated bytes. |
| `search_result_cache_entries{kind}`, `search_result_cache_bytes{kind}` | gauge | The result cache by `kind` (`search`, `facets`): entries and estimated bytes. |

The cache series render only with search on. There is no server-side detail cache: the detail caches are in the browser. SQLite's page cache isn't reported (see the decision).
