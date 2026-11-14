# Lodestar UI/UX review, part 2: skills pass and plan for future modules

2026-11-13, `main` at `55e155a9`. Part 1 (`01-review.md`) holds findings B1–B3, I1–I20 and P1–P14. This part adds what the 15 installed design skills found, and plans the UI for modules not yet built.

## 1. How this pass was done

- **Skills loaded and applied:** `design-review`, `application-interface-design`, `layout-spacing`, `layout-grids`, `responsive-layout`, `typography-ui`, `component-geometry`, `iconography-ui`, `motion-ui`, `minimalist-visual-style`, `dashboard-storytelling`, `analytics-chart-selection`, `analytics-validation`, `infographic-design`, `data-visualization-design`.
- **Authority:** the owner's decisions and `docs/architecture.md` §10 come first; a skill decides only what they leave open. So the font (Atkinson, not the skill's list) and Phosphor icons are not findings.
- **Measured on the real server** (fixture data): computed spacing, radii, control heights, fonts, line heights, icon sizes, transitions and prose width on 17 screens at 1366 and 390 px; reduced-motion emulation; icon imports in source.
- **Numbers reconciled** against the API for Insights (`/insights/overview`, `/stats`, `/search`).
- **Not done:** dialog and drawer radii and motion timings were not measured live; 2560 px, 200% zoom and screen-reader runs are still open; the wizard was measured only in part 1.

## 2. What the skills found

### Blocking

None new. B4 was first filed here as blocking and is now Important: see below.

### Important

**B4 (now Important). The fixture has no event-time fetch counters, so Insights shows "Fetch 0" beside a funnel of 413 (`analytics-validation`).**
- State: range 7 d, default filters, fixture data.
- Expected: the overview funnel reports 413 fetched and 1,364 lost (sum of 150 cohorts; checked).
- Actual: the stage strip says "Fetch · 0", and the Fetch stage page shows "Metadata fetched 0", "Lost 0", an empty chart and ten loss rows of "0". The API returns `stages.fetch: 0`.
- Layer, found after filing: fixture data. `stages.fetch` and the Fetch page read `metric::FUNNEL_EVENTS` (`crates/api/src/insights.rs`, about lines 1105 and 1268). `bench/src/fixture.rs` seeds `rollup_hour` for harvested, rediscovered, dropped, stored, newly searchable and time to searchable only; `FUNNEL_EVENTS` is never written.
- Fix: seed `FUNNEL_EVENTS` (fetched and each loss reason) in the fixture, consistent with its `cohort_hour` rows: the fixture feeds the user-guide screenshots and the fixture e2e. In the UI, label the strip's time base and show an empty state instead of ten "0" rows.
- Not checked: the counter on a live crawl.

**I21. Icon weight is mixed: 124 of 141 icon imports are Phosphor Regular (`iconography-ui`, §10.2).**
- What: the plan says Phosphor Bold, Fill for selected. Only 17 imports end in `-bold` or `-fill`. Rows use bold arrows; banners, status and settings use regular `warning`, `info`, `check-circle`, `x`. Three settings screens also draw 14 px icons (the plan has 16 and 20).
- Fix: switch imports to `-bold`; add a lint ban on `~icons/ph/*` without `-bold` or `-fill`; 16 px minimum.

**I22. Reduced motion is not honoured by form controls (`motion-ui`, §10.2).**
- What: under `prefers-reduced-motion: reduce` the duration tokens are 0, but 59 elements on Search still transition at 200 ms: daisyUI's own button, checkbox, toggle and select transitions don't read the tokens. Normal hover is 200 ms against the 150 ms token; toggles run 300 ms.
- Fix: one global rule mapping daisyUI's transitions to `--duration-fast` (so they zero with the tokens).

**I23. Six control heights where the plan has two (`component-geometry`, §10.2).**
- What: measured 24 (facet exclude), 26 and 34 (segmented items), 28 (Discovered presets), 32 and 40. The plan: 32 dense, 40 default.
- Fix: segmented items 32 inside a 40 or 32 shell; presets and exclude buttons 32 (visible glyph may stay small).

**I24. Chart text is not in the product font; prose runs too wide (`typography-ui`, §10.2).**
- What: uPlot axis labels render in `system-ui`. Paragraphs in Outputs run 86–99 characters a line and one Insights note 208, against the 70 ch cap. Off-token line heights: 12/18 (108 nodes), 14/21 (66), 13/19.5 (20). Cohort dates aren't tabular, so stacked rows don't align.
- Fix: set the chart font from `--font-sans`; apply `max-w-(--prose-measure)` to paragraphs; `tabular-nums` on date columns.

**I25. Insights reads as equal-weight cards, with duplication (`dashboard-storytelling`).**
- Audience and decision: the owner asking "is it working, where are torrents lost, what do I do?".
- Story told now: controls → stage strip → five KPIs → needs attention → funnel and losses → cohorts → two trends → events → three segment panels → five Serve tiles → five links.
- Gaps: the strip and KPI row repeat each other in ten same-size tiles; "Needs attention" is a full-width card even when empty; the trend comes after the funnel, against §10.9.2.
- Stage pages: ten loss rows of "0 —" where an empty state belongs; "Searchable torrents" is an empty plot with one tick; four KPI tiles leave a fifth of the row unused.
- Recommended: one status row (strip merged with KPIs, each tile labelled with its time base), trend 8 + attention 4 as planned, then funnel, segments, Serve. Collapse empty panels to one line.

**I26. Chart-by-chart assessment (`analytics-chart-selection`).**

| Chart | Verdict | Change |
|---|---|---|
| Loss reasons 100% bar | Change type | One colour makes segments unreadable; the sorted list under it already answers the question. Drop the bar, or give the list bars. |
| Searchable by group / type | Keep, format | Use the category colours (I20); stop stretching the shorter card. |
| Funnel | Keep | Label "of the stage before" once, not per row. |
| Newly searchable, harvested lines | Keep, format | Clipped first tick, verbose ticks, dangling legend colon (P1). |
| Cohorts list | Keep, format | Label the first number ("11 harvested · 1 searchable"). |
| Discovered histogram | Keep, format | Separate the year-wide bin (I8). |
| Detail composition | Keep, format | Hide the list when there is one group; it repeats the bar. |

**I27. Two more number mismatches (`analytics-validation`).**
- "Searchable by group" and "by content type" each sum to 407; "New searchable" is 409. Two torrents unexplained (probably hidden types). Label the exclusion or reconcile.
- Search "browse all" returns 2,196; searchable is 2,208. Twelve are hidden with no "N hidden · Show" (P7).
- Passed: funnel never increases down a cohort (150 of 150); harvested = losses + in progress + searchable per cohort; funnel totals equal the cohort sums; yield 22.4% equals final cohorts 237 / 1,060; Searchable equals `/stats`.

**I28. Placeholder used as the label on three inputs (`application-interface-design`).**
- What: the file filter in the detail view, and the level select and text filter on `/logs`, have an `aria-label` but no visible label.
- Fix: visible labels (they can be compact, above or inline).

### Polish

- **P15. Near-miss edges (`layout-grids`).** Insights content sits at 40 and 41 px, and 711 and 712 px. Page `h1` left edges measure 16, 24, 47, 139 and 304 px across destinations (I11).
- **P16. Card overuse (`minimalist-visual-style`, `component-geometry`).** Insights has 31 bordered 12 px cards on one page; headings and whitespace would carry most of that grouping.
- **P17. Three primary buttons** on Security (Change password, Create…, Save…). Each is its own section, so it's acceptable; lower the unsaved-state ones until the section is dirty.

### What passed

- **Spacing values** are on the ramp almost everywhere: the only strays are 3 px (toggle), 1 px (histogram bins), 11 px (query bar) and one 23 px margin. Spacing problems are about grouping, not values (P4, I2).
- **Radii** are only 4, 8 and 12 px (plus round radio buttons). Borders are all 1 px solid.
- **Style rules:** no gradients (only daisyUI's select caret), no shadows, no pill containers, no emoji, no cliché copy.
- **Touch layouts** raise controls to 44 px (exceptions in P8).
- **"How it works"** in the wizard is a correct use of an infographic: a four-step sequence with an ordered-list equivalent.

## 3. Blind spots

- **CI can't see overflow-only defects.** axe runs on the mock, where lists are short; B3 only appears with real data. Snapshot baselines currently enshrine the clipped rows at 768 and 1024 px.
- **No test width between 1366 and 1920**, which is where the drawer changes mode (I1).
- **Only two languages.** Nothing longer than English has been tried in the toolbar, segmented controls or settings labels.
- **Day-one experience.** With a small index the "still growing" banner shows on every search and the pill says "Idle"; nobody has walked the first hour.
- **Failure paths.** Server down, session expiry mid-edit and a failed chunk load were not exercised in either pass.
- **Touch gestures.** Histogram brush, drawer resize and table column scroll on a real touch screen.
- **Sources will triple.** Settings → Sources is a flat list built for about 10 plug-ins; the plan adds about 20 more (section 5.8).

## 4. Opportunities

- **One page frame and header primitive** fixes I11 and P15 at once and gives every new module a place to land.
- **One label formatter per field** (I10) is needed before enrichment adds more codes.
- **A row that adapts to its container** (I3) is needed before grouping, usenet and debrid add more facts to it.
- **Three lint rules** would have caught I21, P2 and I23: icon weight, removed breakpoint prefixes, control heights.
- **A fixture-backed axe and screenshot run** per PR for five key screens.
- **Click a row badge to filter** by it; **recent searches** on the empty state; **"Differences only"** in Compare.

## 5. UI plan for future modules

Each plan names the primary action, where the module lives, what it reuses, its states and the decisions it needs. "Reuse" means an existing primitive in `web/src/lib/ui` or an existing pattern; nothing here needs a new visual language.

### 5.0 Do these first

These current defects would be inherited by every module below:

1. Page frame and header primitive (I11).
2. Adaptive result row (I3) and the drawer decision (I1).
3. Toggle labelling rule (I4): the label says what on means.
4. Label formatter (I10) and icon weight (I21).
5. Settings navigation: field search and status badges (I12), since two more screens arrive.

### 5.1 Version grouping (GS-3, GS-4, GS-5)

- **Purpose:** find the right version without scrolling past ten near-duplicates.
- **Where:** a "Group versions" switch in the results toolbar (a parameter, not a filter, per §10.4), kept in the URL.
- **Row:** the best version as a normal row, plus a quiet "+3 other versions" disclosure. It expands in place to at most five compact rows, then "Show all 12" opens the drawer's Related section.
- **Count:** "128 groups · 412 torrents", with "≈" above 100k as elsewhere.
- **Drawer → Related:** four labelled lists (Other versions, Identical clones, Same file, Similar payloads), each with a count, the attributes that differ shown per row, and "Compare these" pre-filling `/compare` (2–5).
- **States:** while grouping isn't ready, the switch stays usable and a one-line note says why results are ungrouped. Table and Cards get a "Versions" column or count.
- **Keyboard:** `→` expands a group, `←` collapses; `j`/`k` walk the visible rows.
- **Default:** on once grouping data is ready (section 6).

### 5.2 Regex search (RX-1)

- **Where:** the query bar. `/…/` and `name:/…/` get their own token style (mono, weight, colour, as other tokens).
- **Feedback:** an invalid or too-broad pattern uses the existing error line with the reason and position. A time-budget stop shows a banner: "Stopped after 2 s: showing what matched so far", with the count marked "≈".
- **Discovery:** one example in the Syntax popover; a "matches pattern" operator in the Builder.
- **When the trigram switch is off:** say so beside the first regex ("Slower: pattern index is off"), linking to the setting.
- **Don't:** autocomplete inside a pattern, or flag an unfinished one as an error while typing (P10).

### 5.3 Upload and watch folder (UP-2)

- **Purpose:** add `.torrent` files by hand or from a folder.
- **Where:** "Add torrent files" in the overflow menu and on the empty Search state; the whole Search page accepts a drop. Watch-folder settings live in Settings → Sources as its own row.
- **Upload dialog:** drop zone with a visible "Choose files" button (drag is never the only way), then a per-file result list: Added, Already in the index, Not a torrent file, Held by filters. Counts in the header; "Open in Search" for added ones.
- **Watch folder row:** path (read-only when set by `LODESTAR_WATCH_DIR`, with the usual "Set by" line), on/off, last scan, counts for added and failed, and "Show failed files" with the reason and a Retry.
- **States:** too many or too large files are refused before upload with the limit named; a stalled file shows "Still being written" rather than failing (the watch-folder defect from the code review).
- **Decide:** whether failed files can be retried from the UI or only by moving them back.

### 5.4 Usenet results and Send (UN-3, UN-4)

- **Purpose:** show items from usenet indexers beside torrents without confusing the two.
- **Where:** Search, behind the Network facet (default stays BitTorrent only, §10.4).
- **Row:** a "Usenet" badge first; facts are size, age, group and indexer. No seeders, trackers or composition bar: show nothing rather than "—", since the idea doesn't apply.
- **Action:** the primary action is "Send to SABnzbd" (or NZBGet), not Copy magnet. With no client configured the button reads "Set up a usenet client" and links to Integrations.
- **Rules:** these are unverified items, so the existing Unverified badge and toggle apply. Excluded from Compare and from "Copy magnets" in bulk; the bulk bar says why.
- **Drawer:** summary and provenance only; no file tree until the NZB is known.
- **Visibility:** already decided (plan Q6, 2026-11-06): hidden by default, shown only with `network:usenet` or `verified:no`.

### 5.5 Enrichment (ENR-11, ENR-13, account plug-ins, debrid)

- **Drawer → "Identified as":** matched title, year and kind; the source and how sure it is in words ("Matched by name and year"); ID links (IMDb, TMDB, …) with the external-link icon; a "Not this one" action that opens Fix parse. No posters by default: they would mean a third-party image request per view.
- **Search:** `imdb:`, `tmdb:` and similar fields in autocomplete with the matched title beside the ID; alternate titles match silently, with "matched an alternate title" on the row.
- **Sources:** enrichment plug-ins get their own group. Account plug-ins use the existing consent dialog and secret field; each row says what is sent ("the parsed title and year, never the info hash").
- **Debrid badge:** "Cached on <service>", icon + text, only when an account is set up; never a colour-only dot. One badge slot, so the row doesn't grow.
- **Adult items:** nothing is looked up or shown (ENR-G); the section is absent, not empty.
- **Decided:** no posters (section 6). **Decide:** which fact gives way when a row has five badges already.

### 5.6 Stremio output (OUT-1, OUT-3)

- **Where:** Settings → Outputs, as a third block after Torznab and the bitmagnet export. That screen needs its `h1` first (I11).
- **Block:** what it does in two sentences; "Install in Stremio" as the one primary action; the manifest address with Copy; a note when the address is `127.0.0.1` or plain HTTP that Stremio on another device can't reach it, with the fix.
- **Options:** "Include adult items" off by default, with the same wording pattern as the export's; the fixed exclusions listed, not configurable.
- **Access:** its own key, created and revoked like Torznab keys.

### 5.7 Workers, placement and companions on workers (WK-16, PL-7a/b, CW)

- **Purpose:** see whether each worker is healthy and act on the one that isn't.
- **Where:** Settings → Workers (Crawling group). Hidden until the cluster feature is on (§10.10.2).
- **List:** a table, one row per worker: status as icon + text (Online, Draining, Offline, Quarantined), name, address it crawls from, version, throughput per minute, spool. Sorted problems first. Below 640 px it becomes cards with status, name and throughput.
- **Detail:** a drawer per worker with the remaining §19.7 facts (uptime, CPU and memory, shards, clock offset, leases), then tabs: Sources, Companions, Budget.
- **Add worker:** a dialog with one tab per install kind (command line, Docker, Compose, Unraid, Windows), a copyable command, and the join token's expiry shown as a countdown. The dialog stays open until the worker appears, then says so.
- **Actions:** Pause and Drain are plain buttons (reversible, no confirmation). Revoke and "Purge data from this worker" sit in a Danger zone with a specific confirmation naming the worker and the torrent count.
- **Placement (PL-7a):** each Sources row gains "Runs on: Automatic" with a control to pin, and chips for where it runs now. PL-7b: the worker's Sources tab lists what it may run, with per-worker consent using the existing password dialog.
- **Elsewhere:** the pill reads "Crawling · 3 workers · 4.2k/h"; an offline or clock-skewed worker appears in Settings → Overview's Needs attention.
- **Decide:** whether a worker's name is editable in the UI; what "Update" does on platforms that can't self-update.

### 5.8 More sources: IRC, Nostr, Lemmy, FlareSolverr, third-party adult claims

- **The scale problem:** Sources will hold about 30 rows. Before adding any: collapsible groups with a summary ("Discovery · 3 on, 1 needs setup"), a filter box, rows collapsed to one line (name, status, switch) with details on expand, and a legend for the consent classes (None, External, Account, Restricted, Overlay).
- **IRC announce (Account, private):** the row states that every item is private and never leaves Lodestar. Fetching the `.torrent` is an explicit action with the snatch warning each time.
- **Nostr and Lemmy (External):** relay or instance list as a chip field; state per endpoint (connected, backing off, refused); finds are unverified.
- **FlareSolverr (opt-in):** appears only as a dependency chip on Prowlarr's row ("Needs FlareSolverr · Set up"), never as a suggestion.
- **Third-party adult claims (ADH-0):** no new UI; the existing "Show adult content" and Hidden-by-filters paths cover it. State that in the brief so nobody adds a switch.

### 5.9 The 1.0 design gate (M8-DR, G-v1-design)

- Run `design-review` with all 14 specialist skills loaded, on fixture data, at 320, 390, 768, 1024, 1366, 1440, 1920 and 2560 px.
- Include the measured checks from this pass (spacing ramp, radii, control heights, icon weight, transitions under reduced motion, prose width) as scripts, so the gate is repeatable.
- Add `analytics-validation` cases for the strip, funnel, segments and each stage page.

## 6. Decisions

The owner answered on 2026-11-13: "Go with your recommendations to all the decisions." The build runner records them; this is the list as sent.

1. **Drawer at 1366–1463 px (I1).** The drawer sits beside the results from 1366 px, as §10.15 says. While it is open the results track may shrink to a 480 px floor, and the drawer opens at 480 px until the window fits the stored width. Do it with I3.
2. **B4** was not a choice: it is a fixture gap (above).
3. **Version grouping.** "Group versions" is on by default once grouping data is ready, remembered per account and kept in the URL. If the scale gate fails with collapse on, the default falls back to off and the question returns to the owner.
4. **Usenet visibility** was not open: the owner answered it on 2026-11-06 (plan Q6): hidden by default, shown only with `network:usenet` or `verified:no`, with the unverified badge. Section 5.4 matches it.
5. **"Identified as".** No posters or other third-party images.
6. **Settings → Sources.** Restructure before the next plug-in row is added, as its own small slice.
