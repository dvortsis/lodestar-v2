# P-v06-sources-plan decisions

Slices the v0.6 Sources epic (`E-v06-sources`, build-plan P4) into 24 `SRC-*` slices. The plan is `docs/plan/2026-11-01-v06-sources.md`; the entries are in `runner/slices.yaml`. Docs only. Nothing here reverses or loosens an owner decision, weakens a security or content-safety decision, or changes an owner-set number. The refinements below are the runner's to flag (`docs/owner-decisions.md` §2.19). The open questions are in the plan's §5, with five marked for the owner.

- **One migration, V15, after CR-3's V14** (SRC-1). The fixed order V12 → V13 → V14 (`2026-10-30-master-plan.md`) stays as it is. V15 holds `unverified_items` and `torrent_claims` (architecture §18.5) plus `source_state`, the plugin KV store. §18.1 sketches that store without saying where it lives; a table keeps cursors transactional with the items they produced. A new external, `v14-merged`, gates SRC-1.
- **The snapshot purge covers claims and unverified items.** Both hold names, so owner §2.16, §2.17 and §2.21 apply to them as to `torrents`. This tightens; it doesn't loosen.
- **Claims and unverified items are filtered before storage, locked floor included** (§18.7, §20.1). The plan names this explicitly because §18.5 describes the tier without restating it.
- **Verification fetches of unverified keys stay behind a switch, default off,** until the crawler acceptance soak ends. They share `fetch_queue` and the fetch pool (§18.4), and the v0.6 crawler gate is measured with no imports (build-plan §0.6).
- **Search fields ride the M7 `SCHEMA_VERSION` bump** (build-plan §1.3). Until then, unverified items are stored but not searchable. A new external, `m7-schema-bump`, gates SRC-3.
- **Two new crates:** `crates/sources-feeds` (pure, fuzzed parsers) and `crates/sources-plugins` (one module per plugin behind its `src-*` feature, §18.1). The Sonnet glue slices then own one file each and can run in parallel (build-plan P4).
- **AnimeTosho reuses the Torznab parser,** since §18.2 calls its feed "Torznab-style". There's no separate parser slice for it.
- **Sonarr/Radarr webhooks are their own slice (SRC-11b),** because an inbound endpoint is a new auth surface. It may slip to v0.7 without blocking the v0.6 tag.
- **Scheduler v2 is narrowed for v0.6:** per-host limits, schedule windows and per-plugin egress only. Shares per resource class and auto-balance wait for yield data (§18.4 says "v0.6+"; plan Q10).
