# Lodestar v2 build plan: M5 → 1.0, with helper-agent orchestration

> Written 2026-10-22, after M3. It plans the order of work and who does it: the main session or a helper agent, per the delegation policy in `CLAUDE.md`.

## Context
M0–M3 are merged: the crawler with MSE fetch, store schema v3, the Tantivy torrent and file indexes, the query language, enrichment and the worker boundary. There's also a minimal API (setup, login, parse, search with offset paging, detail, files) and a 211-line status page.

M4 is being built **in other sessions**. Merged so far:
- `ContentType::Adult` (#28);
- the `filter` crate with the child-safety core (#29);
- the filter wired into store and pipeline (migration V4, #31);
- `title_stem` (#30, search `SCHEMA_VERSION` 4).

Still open: tracker scrape, BEP 33 and hot columns (#32), and blocklist subscriptions (#33, migration V5). This plan uses M4's fields and APIs but doesn't build them.

What the code shows (checked):
- `web/` and `packaging/` don't exist.
- API handlers return untyped `json!` values, so there's no typed contract yet.
- Routes are split between `crates/api/src/lib.rs` and `crates/lodestar/src/http.rs`.
- Migrations are one append-only array in `crates/store/src/schema.rs`. M4 is actively adding to it (V4 merged, V5 open).
- The `settings` table already exists.
- `index_watermark_hour` doesn't exist yet.
- There's no blue/green index rebuild, so any `SCHEMA_VERSION` bump today starts from an empty index.
- CI is a single `ci.yml`.

The owner wants the rest of the application planned in detail through 1.0, with an outline after that. It should use as many helper agents as add value without trading away quality. So this plan covers both what gets built and how the work is cut, delegated and reviewed.

`docs/architecture.md` stays the source of truth for *what* is built. This document covers *order and delegation*. When a slice changes a decision, the slice adds a file under `docs/decisions/`, not an edit to this file.

---

## 1. Orchestration model

### 1.1 Who does what (CLAUDE.md policy, made operational)

**Main session (Opus 5.5, high effort): the integrator.** It owns:
- **Rust: search.** Leases, rebuild, facets, suggest, percolator, regex prefilter.
- **Rust: store.** Migrations and on-disk formats.
- **Rust: API.** Contracts, auth and keys, SSE, insights queries.
- **Rust: protocols and untrusted input.** The cluster and mTLS, uTP, Torznab query mapping, `.torrent` import, and hand-written feed, XML and IRC parsers.
- **Rust: other security-critical code.** Companion signature checks, credential storage and the SSRF guard.
- **Security-sensitive UI state machines.** Setup token, 18+ confirmation, §20.11 password and effect warnings, CSX-5.
- **Every review and merge.**

**Sonnet helpers** take everything well specified:
- web foundations and screens per §10, and the theme compiler;
- formatters, written to a main-session spec and reviewed by the main session line by line;
- the typed client and URL state, reviewed by the main session;
- test harnesses, CI workflows, packaging and docs;
- download-client and notification adapters behind a trait the main session defines;
- plugin glue, manifests and recorded-fixture tests, after the main session lands plugin #1 of each family;
- serde-derived JSON clients;
- simple CRUD endpoints that follow an established pattern.

**Haiku helpers** do mechanical work only:
- generating the 47 non-Riverstone theme JSONs from a palette table the main session writes;
- message-key extraction and renames;
- formatting sweeps.

**What's never delegated:** parsers of untrusted input, on-disk formats, concurrency-heavy code, security and content-safety logic.

To protect the critical path, **the main session writes almost no Svelte.** Its time goes to Rust and to reviews.

### 1.2 How a helper runs
- **Launch:** Agent tool with `isolation: "worktree"`, `run_in_background: true` and the model above. Each helper branches from the latest merged foundations (tag `web-foundations-v1` once it exists).
- **Brief:** every helper gets one, using the template in §7.
- **Cap: at most 4 helpers at once**, alongside the main session's own Rust track. *(Superseded for the build runner: `runner/slices.yaml` `limits` allow 16 core and 16 helper slices, owner 2026-10-22; the same applies to the "at most 4" waves below.)* Review bandwidth is the bottleneck, not how many agents can be spawned. The main track pauses when reviews queue up.
- **Review gate** (main session, for every helper result):
  - read the whole diff;
  - run `npm run check`, `npx vitest run`, `npx size-limit` and the affected Playwright specs;
  - run `cargo fmt --check`, `clippy -D warnings` and `test` wherever Rust is touched;
  - run `/code-review high` on diffs over 400 lines;
  - run `/security-review` on anything touching auth, credentials, filters, the cluster or companions;
  - redo weak output on Opus rather than patching it piecemeal.
- **Unanchored second read** for the riskiest main-session PRs (leases and cursors, rebuild and swap, percolator, mTLS join and fencing, uTP, companion verification, the filters API). A fresh Opus review agent gets only the diff and the spec sections, not the author's conclusions.
- **Workflow scripts are optional.** Any wave can run as a `Workflow` script instead of hand-launched agents, but only if the owner explicitly says "use a workflow".

### 1.3 Seams that make parallel work safe
1. **Contract-first API.**
   - Typed DTO structs with utoipa `ToSchema` produce `/openapi.json`.
   - A bin writes `web/src/lib/api/openapi.json`, and `openapi-typescript` turns it into `schema.d.ts` (types only, nothing at runtime).
   - CI fails if either generated file is stale.
   - Each new endpoint first lands as typed structs plus a `501` stub with an example, so screens can start early.
   - Helpers use only the generated types. A missing field is a request to the main session, never a local type.
2. **Fixture backend.**
   - `lodestar serve --fixture` (main session) builds a deterministic data directory with known facet counts, cohorts, events and saved searches. M4 fields are present both as null and as populated values.
   - Until it exists, a W6 mock replays the OpenAPI examples.
   - The M5 exit tests run against the real binary.
3. **Token contract.** `docs/design-tokens.md` fixes every CSS custom-property name before the theme compiler and the primitives start, so both can proceed in parallel.
4. **M4 contract.** Agree the nullable field names with the M4 session up front:
   - swarm: `seeders`, `leechers`, `trackers_reporting`, `trackers_checked`, `last_scrape_at`, `dht_seeds`, the swarm-history shape;
   - filters: `hidden_by_filter`, `why`;
   - Adult: `adult_kind`.

   "Never scraped" is `null`, so the UI's "—" state is built and tested now.
5. **Points that must run one at a time:**

| Shared thing | Rule |
|---|---|
| `crates/store/src/schema.rs` migrations | Main session only. One M5 migration, merged after M4's schema lands; renumber on rebase |
| `crates/search` `SCHEMA_VERSION` | Exactly one bump per milestone (M5: facets; M7: trigram, `payload_sig`, file category/`pieces_root`, v0.6 network/source/verified fields), and only after the blue/green rebuild lands |
| `crates/api/src/lib.rs`, `crates/lodestar/src/services.rs` | After the router split, each slice adds one line; merged in sequence |
| `openapi.json` / `schema.d.ts` | Regenerated after a rebase, never merged by hand |
| `web/package.json` and the lockfile | W0 pins every expected dependency up front. Adding one later needs main-session approval |
| `.github/workflows/*` | Split into `ci.yml` (Rust), `web.yml`, `e2e.yml`, `release.yml` and `nightly.yml`, with one owner at a time |
| Messages | One message file per feature (`features/*/messages/{en,zh}.json`), merged by a script, so translations never conflict |

---

## 2. Sequencing and critical path
```
P0 seams ─► P1 v0.5 MVP (min M5 + Docker) ─► P2 M5 complete ─► P3 M6 ─┐
                                                                       ├─► P5 M7/v0.7 ─► P6 v0.8 ─► P7 M8 → 1.0
                                       P4 v0.6 (overlaps P3) ──────────┘
```
- **Ship v0.5 first.** §14's MVP cut line is M0–M3 plus the minimum of M5 plus Docker. That puts a usable product out before the long tail of M5.
- **Critical path:** the main session's Rust queue, then B15 (the filters API, which waits on M4), then the Filters & Safety UI, then `design-review`. Everything else (web, packaging, CI and docs) runs alongside on helpers.
- **Main-session order for M5:** B0 → B1 → B3 → B2 → B5b → B5a → B8 → B4 → B6 → B16 → B11a → B7 → B12 → B9 → B5c → B13 → B14 → B11b → B15.

---

## 3. Phases and slices
Each item is one PR of about 1,500 changed lines or fewer. **[M]** = main session, **[S]** = Sonnet, **[H]** = Haiku.

### P0: Seams (mostly main, serial)
- **B0 [M] Split the API router.** One module per domain, each exposing `routes()`. A shared error envelope `{error:{code,message,field?}}`. A `crates/lodestar/src/services.rs` builder so `main.rs` stops growing.
- **B1 [M] Typed DTOs + utoipa + `/openapi.json`.** Plus the generator bin and a drift check.
- **B3 [M] Embedded SPA.**
  - Brotli and gzip precompressed files, immutable caching for `/_app/immutable`, SPA fallback.
  - CSP with the hash of the pre-paint script.
  - A stub when `web/build` is absent, so `cargo test` needs no Node.
  - The status page moves to `/status`.
- **B2 [M] Settings registry.**
  - Typed sections, layered as defaults < `config.toml` < DB < env. Fields set by the environment are read-only and say so.
  - `GET/PATCH /settings/{section}` with a JSON schema for form generation.
  - Live changes go out over `watch` channels, and fields that need a restart are flagged.
  - Needs no migration.
- **Web foundations (helpers, in parallel with B0–B2):**
  - **W0 [S] Scaffold.** SvelteKit 2 + adapter-static (`ssr=false`), strict TS, Tailwind v4 + daisyUI 5, Bits UI, TanStack Virtual, unplugin-icons (Phosphor), Paraglide, eslint + svelte-check. Every dependency is pinned; versions were checked on npm (bits-ui 2.20, daisyui 5.7, tailwindcss 4.3, uplot 1.6, paraglide 2.29, openapi-typescript 7.13, fontsource Atkinson Next/Mono 5.3). Empty pages for every §10.3 route. size-limit budgets from §10.1. A lint rule bans hex literals and arbitrary Tailwind values in `.svelte` files. **The main session reviews W0 closely**, because every later helper builds on its conventions.
  - **W1 [S] Theme compiler** `web/scripts/themes.ts`.
    - OKLCH derivation and every §10.18 gate.
    - Golden tests reproduce the §10.18 table (e.g. `primary-ink #4A7789`).
    - Outputs critical CSS for Riverstone, a lazy `themes.json` and a pre-paint script under 1 KB.
    - The main session reviews the colour maths.
  - **W1b [H] The other 47 theme JSONs**, from the main-session palette table. The compiler's gates catch bad values.
  - **W3 [S] Asset pipeline** `web/scripts/assets.ts`. Uses sharp; checks SHA256SUMS; makes AVIF/WebP at 360/540/724; builds favicons and the manifest; turns the compass SVG into CSS variables; gates each scene at 45 KB or less.
  - **W4 [S] Per-feature Paraglide messages**, plus the merge script.
  - **W6 [S] Test harness.**
    - vitest + testing-library.
    - Playwright projects for breakpoints × themes × densities, an axe helper, the OpenAPI-example mock and Lighthouse CI.
    - The CI split into `web.yml`/`e2e.yml`.
  - **Then W2 [S] tokens/daisyUI mapping/font subsets** (Next ≤ 35 KB, Mono ≤ 25 KB, tabular numerals verified) **and W5 [S] typed client.**
    - W5 covers abort and latest-wins, 401 → login, the error envelope, URL ⇄ AST state, and SSE batched per animation frame and paused while the tab is hidden.
    - The main session reviews W5 line by line.
  - **Contracts [M].** `docs/design-tokens.md` and `docs/ui-glossary.md`. `docs/metrics.md` plus the formatter spec, which W8 implements.
- At P0 start: `add_repo dvortsis/skills`, so helpers can read `ui-design-skills`.

### P1: v0.5 MVP (minimum M5 + Docker)
**Main track:**
- **B5b** Background rebuild into a new versioned directory, atomic swap, progress. Must land before any schema bump.
- **B5a** Searcher leases with cursors (10 min), cancel tokens between segments, exact count on demand.
- **B8** The single M5 migration, after M4's schema merges:
  - user data: `saved_searches`, `alerts`, `user_items`, `collections`, `collection_items`;
  - auth: `api_keys`;
  - reporting: `index_watermark_hour`, `query_log` (opt-in);
  - the store APIs for all of them.
- **B4** SSE `/events`:
  - a broadcast hub with topics;
  - heartbeats and `Last-Event-ID`;
  - a bounded buffer per client, with a `resync` event for clients that lag;
  - a connection cap;
  - a commit hook in `crates/search/src/live.rs`.
- **B6** `/facets`:
  - disjunctive term facets;
  - histograms: size (log bins), discovered, year;
  - seeders/trackers buckets behind a flag until M4 lands;
  - sampling above 1M with "≈";
  - analytics-validation fixture tests;
  - schema bump #1, together with B7.
- **B16** `lodestar serve --fixture`.

**Helper waves (at most 4 at once, disjoint directories):**
- **W7a–d [S×2]** Primitives in four directories under `src/lib/ui/`:
  - controls and fields;
  - labels and feedback;
  - overlays (Bits UI);
  - lists, tables and tree (virtual ARIA).

  Each helper takes two groups. Each primitive gets keyboard and state tests.
- **W7e [S] Charts** in `src/lib/charts/`: CategoryBar, HBarList, Histogram + brush, Sparkline, KPI, a lazy uPlot LineChart wrapper, DataTableAlternative.
- **W8 [S] Formatters** in `src/lib/format/`: one per metric, IEC sizes, null vs 0 vs "≈". The main session reviews every line.
- **W9 [S] App shell:**
  - top bar, nav and status pill;
  - command palette and keyboard registry (each feature registers from its own file);
  - illustrated full-page states and responsive nav;
  - auth screens, whose setup-token and login state machines are written by the main session.

  After W9, tag **`web-foundations-v1`**.
- **Search UI [S], three PRs:**
  1. query bar and highlighter, active-filter strip, chips;
  2. facet rail and results list (Comfortable/Compact) with all §10.4 states;
  3. toolbar and cursor infinite scroll.
- **Detail drawer and `/t/{hash}` [S]:** summary, composition bar list, file tree (Web Worker above 2k files), parsed metadata, raw viewer, copy magnet.
- **Docker [S]:**
  - a multi-arch musl `distroless/static` image, with mimalloc;
  - PUID/PGID handled by the binary, since distroless has no shell;
  - HEALTHCHECK and a `docker-compose.yml`;
  - `release.yml` with `nightly` and version tags;
  - a CI smoke test.

**Rule: the client never computes counts, cohorts or percentages.** Every number comes from the server through the W8 formatters and carries a `data-testid`.

**v0.5 exit:**
- Keyboard-only Playwright on the fixture backend: search → drawer → copy magnet.
- axe clean.
- size-limit: first load ≤ 100 KB, JS ≤ 45 KB.
- `docker compose up` reaches `/healthz`, and a fixture search returns hits.

### P2: M5 complete
**Main track:**
- **B11a** Insights overview: KPIs, cohort finality, compare period, events, the "stale" flag.
- **B7** `/suggest` (term prefixes, an FST over titles, fields and units, "did you mean") and natural phrasing on `/parse`.
- **B12** API keys and feed tokens, rate limits, the fresh-password confirmation primitive (§20.11), proxy-header auth.
- **B9** Percolator: each commit runs saved searches over ids above the watermark → alerts → SSE, plus RSS per saved search.
- **B5c** Result cache and `?explain=1`.
- **B13** Siblings, batch detail for Compare, `/discover` ("rising" after M4).
- **B14** `/stats`, `/crawler` (pause and live intensity), `/sources`, and the wizard's network check.
- **B11b** The five stage endpoints, `export.csv`, daily compaction and `lodestar rollup --repair`.
- **B10b** `/export` and `/jobs` (streamed and throttled).
- **B15** Filters API (§20.7), once M4's engine merges: CRUD, preview job, warnings, password gate, review queue, `classify` corrections.

**Helper slices [S]:**
- **B10a:** CRUD for user items, collections and saved searches, following B1's pattern.
- **B11c:** digest rendering and preview.

**Screens (at most 4 at once).** Each owns only `features/<x>/` and `routes/<x>/`:
- **Search completion:**
  - autocomplete, smart chips with a natural-phrasing preview, and the Builder (round-trips through the server AST);
  - table mode with a column chooser, selection and the bulk bar;
  - the live "N new" banner, zero-result recovery, and cards density.
- **Compare**, including the narrow-screen re-architecture.
- **Discover.**
- **Alerts:** list, detail with the "New since" divider, and create/pause/delete dialogs.
- **Insights, in three PRs:** the overview story, the five stage pages, then print and digest settings.
- **Settings and the wizard** (architecture §10.10):
  - **[M] B2b, B2c:** API additions (schema hints, revisions, batch patch, preview, test, preferences, SSE, export/import; then `/system`, restart, the HTTP-address keep-or-revert timer).
  - **[M] M-settings-form:** the form engine every screen and the wizard use (draft store, validation and server-error mapping, atomic save, conflict merge, leave guard, drafts, Undo, the `SettingField` and widget registry).
  - **[S] S-settings-1:** the shell (grouped navigation, Overview, list → detail, settings search and palette entries, restart banner, Show changed only, View as config.toml) and the generated screens: General, Crawler, Network & VPN, Storage & retention, Search & ranking.
  - **[S] S-settings-2:** Integrations, Notifications, Trackers, Insights & digest, Language & region.
  - **[M] B-update:** the update engine (§12.2): the signed manifest and key chain (key changes need the recovery key), levels and channels, the single update check, the overlay supervisor with probation, and automatic rollback. `/security-review` required. The Windows MSI path and manifest signing in `release.yml` come with M6.
  - **[S] S-updates:** Settings → Updates and the wizard's Updates step (§10.10.14).
  - **[S] S-about:** Settings → About (§10.10.13). B2c provides `/system` and `licenses.json`; Check now uses B-update's signed check.
  - **[M] M-security-settings:** the Security screen (password, sessions, API keys, proxy auth).
  - **[M] B-log1, B-log2:** the logging core (§12.1): the event catalogue (`log_event!`) with per-event switches and counters, and per-kind data switches per output (Private by default), enforced by typed wrappers, a CI check and a canary test; the rotating log file; then the ring buffer, the admin-only `GET /logs/stream` (not the `/events` hub) and the file API.
  - **[S] S-logs:** Settings → Logging and the live log at `/logs` (§10.10.12).
  - the first-run wizard, reusing the same fields;
  - Appearance: theme picker and custom-theme editor with a live contrast report.
- **Filters & Safety:** rules, editor, preview, review queue, the CSX-5 override list, "Block this…", Not junk / Not adult, Show adult (18+) and Adult facets. Helpers do layout only. The main session writes the dialog state machines, all guardrails stay on the server, and `/security-review` runs on it.
- **Keyboard map:** remapping and the shortcut sheet (§10.16).
- **Playwright matrix and Lighthouse CI.**
- **[H]** zh catalogue fill and icon-registration sweeps.

**M5 exit [M]:**
- the `analytics-validation` matrix: facets against independent SQL, histogram bins, and the §10.17 Insights reconciliation;
- the reporting-overhead benchmark (§3);
- a `design-review` pass with every blocking finding fixed;
- Playwright breakpoints × themes × densities, plus reduced motion and keyboard-only, and Lighthouse budgets;
- an "M5 decisions" file in `docs/decisions/`.

### P3: M6 packaging
- **[M]**
  - The CLI moves to clap, with `backup`, `rollback`, `vacuum`, `reindex` and `stats`.
  - FUSE and network-filesystem detection.
  - Windows-specific runtime work: the service entrypoint (`windows-service`), below-normal priority, the ephemeral-port warning, mmap delete-retry and `SO_LINGER(0)`.
- **[S]**
  - Unraid CA template, plus worker and gluetun variants.
  - WiX MSI via cargo-wix: service, firewall rules, opt-in Defender exclusion, Start-menu shortcut. Plus a portable zip.
  - A systemd unit, tarballs and the full `release.yml` matrix.
  - Smoke jobs: Docker; Windows MSI → service → `/healthz`; Linux tarball.
  - **The user guide** (`docs/user/`, written with `github-docs` and the `user-docs` layer, owner's decision §2.14). github-docs' outline: Before you start, Install Lodestar (Unraid, Docker, Windows, Linux), Set up Lodestar, Keep Lodestar safe, Everyday use (updates, backups), Troubleshooting, then the answers, settings reference, glossary, revisions and index. Screenshots come from `S-doc-shots`, and diagrams are Mermaid. `docs/operations.md` is rewritten for the same reader, or folded into the guide. It's written today for readers who already know Docker, ports and environment variables. The guide's required **Keep Lodestar safe** chapter (github-docs) carries `user-docs` §6.
- **Owner actions:** the Unraid CA submission. The code-signing certificate is deferred (`docs/owner-decisions.md` §2.23): Windows builds ship unsigned, and the guide covers the SmartScreen warning.

### P4: v0.6 (overlaps P3)
**Remote workers v1 (§19), main session only, about 6 PRs in `crates/cluster`:**
1. cluster CA, mTLS and one-time join tokens;
2. the postcard-over-WebSocket protocol, heartbeats and fencing;
3. have-filter replication;
4. `sharded(keyspace)` DHT;
5. fetch and prepare on workers, plus the spool;
6. the chaos test harness and the §19.8 gates.

**[S]** Settings → Workers and per-worker Insights.

**Companion manager:**
- **[M]** the signed catalogue, checksum and signature checks, supervised child processes and encrypted credentials.
- **[S]** compose and CA-template generation, detection adapters and the Companions UI.

**Sources:**
- **[M]** the unverified tier and claims (§18.5), the consent framework, and Prowlarr/Jackett. Prowlarr/Jackett is plugin #1 and sets the contract-test pattern with recorded fixtures.
- Then Nyaa RSS, AnimeTosho, Internet Archive, Academic/official lists and download clients as a source:
  - the main session writes the feed and XML parsers;
  - **[S]** writes the plugin glue, manifests and tests, one file each, in parallel.
- **[M]** BEP 48 full scrape, built on M4's scraper, and torrent-cache enrichment.
- **[S]** Settings → Sources and the Jobs view.

**Filters:**
- **[M]** curated rule packs.
- **[S]** ThePornDB/StashDB enrichment, following the plugin pattern.

### P5: M7 / v0.7
- **[M]**
  - The Torznab server.
  - `.torrent` upload and folder watch.
  - Grouping (MinHash, collapse-by-group), `payload_sig`/`sig:` and "same file". These are search schema bump #2, rebuilt blue/green.
  - The regex trigram field, prefilter and time-budgeted verify.
  - **uTP (BEP 29)** behind a flag, with its success rate measured.
  - Secret storage and the SSRF guard.
- **Download clients:**
  - **[M]** the `crates/integrations` trait and the qBittorrent adapter.
  - **[S], in parallel:** the Transmission, Deluge, rTorrent and Aria2 adapters, and the webhook, ntfy, Discord, email and RSS senders, all with recorded fixtures.
  - **[S]** the Send / bulk-send / "Send selected files" UI.
- **Enrichment plugins** (srrDB, PreDB, Wikidata, TMDB/TVDB, AniDB/AniList, MusicBrainz, OpenLibrary, IGDB, Discogs, Steam, debrid):
  - **[M]** the HTTP, egress, rate-limit and cache harness, plus the first plugin;
  - **[S]** the rest, at most 4 at a time, one module each;
  - the main session reviews each response parser.
- **[M]** The usenet tier, IRC announces and PreDB's IRC feed.
- **[S]** Stremio addon and bitmagnet export.

### P6: v0.8
- **[M]** Placement for singleton, account and overlay plugins on workers; companions on workers; the Anna's Archive / LibGen importer (restricted consent).
- **[S]** Nostr NIP-35 and Lemmy plugin glue (parsers are main-session work).

### P7: M8 hardening → 1.0
- **[M]**
  - The relevance judgment set and `relevance` bin, with an NDCG@10 gate.
  - 10M performance and size gates on a dedicated runner.
  - The Windows soak test.
  - A full-system security review.
  - Plugin politeness and purge audits.
  - The chaos gates green.
- **[S]**
  - CI wiring for those gates.
  - Docs: `query-language.md`, `relevance.md`, `themes.md`, a user guide, and a README with the brand lockups.
  - A nightly job running Playwright and axe across all 48 themes.
- Release 1.0 when every §17 gate is green.

### After 1.0 (outline)
- v1.1: network tiers and I2P (i2pd)
- v1.2: eD2k/Kad and aMule
- v1.3: IPFS and IPNI
- v1.x: the micro-piece prober (consent-gated), embedded WireGuard, saved-search auto-send

Each gets its own detailed plan when it's reached. All of the protocol work is main-session work.

---

## 4. Where helpers add value, and where they don't
- **High value.** These are many disjoint files with tight specs, roughly 60% of the remaining lines:
  - Svelte foundations, primitives and screens;
  - the theme JSONs;
  - packaging and CI;
  - adapters and plugin glue after a main-session pattern;
  - Playwright matrices and docs.
- **Little or negative value.** Splitting search, the percolator, leases and rebuild, the cluster, uTP or schema work across agents. These have tight invariants and shared files, and reviewing split work would cost more than writing it. The same goes for two helpers on one screen, or a helper inventing API shapes.

## 5. Risks and mitigations
| Risk | Mitigation |
|---|---|
| Helpers drift from §10 | Briefs cite exact § and skill paths; token names fixed first; the lint ban on hex and arbitrary values; snapshots and axe in Riverstone light/dark on every PR (all 48 themes nightly); `design-review` per screen |
| API contract drift | Generated OpenAPI → TS types, a CI drift check, `501` stubs with examples |
| Screens blocked on endpoints | Stubs plus the W6 mock now; the B16 fixture server for exit tests |
| M4 slips or its fields are missing | Nullable fields and a tested "—" state; Filters & Safety and seeders facets gated on M4, not blocking v0.5 |
| Migration or schema collisions | Main session only; one migration per milestone, rebased after M4; one search schema bump per milestone, after B5b |
| Conflicts in shared files | Router split, per-feature messages, pinned dependencies, no barrel files; hub edits reported, then applied by the main session |
| Bundle creep | size-limit per route chunk in every helper's acceptance commands; no new dependencies without approval |
| Review backlog lowers quality | Cap of 4 helpers; the main track pauses for reviews; weak output redone on Opus |

## 6. Verification
- **Every PR:**
  - Rust: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`;
  - web: `npm run check`, `npx vitest run`, `npx size-limit`;
  - `git fetch origin main` before pushing (clones are shallow).
- **v0.5 / M5 / M6–M8 exits:** as listed in each phase, which mirrors §14 and §17. That covers analytics-validation, design-review, Playwright and axe, Lighthouse, the reporting overhead, Docker/Windows/Linux smoke, the chaos, filter and NDCG gates, and 10M perf and size.

## 7. Helper brief template
```
Goal: <one sentence>.   Model: sonnet|haiku.   isolation: worktree, from tag <web-foundations-v1|main>.
Read first: docs/architecture.md §<n>; ui-design-skills/skills/<x>/SKILL.md; docs/design-tokens.md; web/src/lib/api/schema.d.ts
Own (create/edit): <paths>.   Do not edit: hub files (§1.3), package.json, anything not listed.
Inputs: <endpoints / fixtures>.   Out of scope: <list>.
Done when: npm run check && npx vitest run <dir> && npx size-limit && npx playwright test <spec> pass;
  no hex/arbitrary values; strings only in features/<x>/messages; ≤ 1,500 changed lines.
Report: files changed, hub edits needed, missing contract fields, deviations from spec and why.
```

## 8. Automation: the runner
`runner/README.md` describes a long-lived Opus **runner session**:
- It starts each slice in a new cloud session as soon as the slice's dependencies are merged.
- It has a fresh Opus reviewer check every PR.
- It merges PRs that are green and have no blocking findings, as the owner allowed on 2026-10-22.
- It keeps going around the clock.

The slice graph is in `runner/slices.yaml`.

## 9. First steps
1. Start B0 (router split) on the main track.
2. In parallel, launch W0 (scaffold) as the first helper.
3. Then the contract docs: `docs/design-tokens.md`, `docs/metrics.md`, `docs/ui-glossary.md`.
