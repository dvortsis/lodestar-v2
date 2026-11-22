# X-hit-resolution (runner brief, 2026-11-22Z)

From UI review I15 (the Resolution half), recorded in runner records r13 (`docs/decisions/2026-11-22-runner-records-r13.md`). Kind: core (the search API's response shape and the search route). Opus throughout. Deps merged: X-units-typed (#1076), X-search-toolbar (#1075, which added the table's Raw name column and deferred Resolution here).

## Goal
`SearchHit` (`crates/api/src/dto/search.rs:467`) gains `resolution: string | null`, the release resolution the `res:` clause already filters on (`crates/search/src/query.rs` `TagKey::Res`; values in their canonical `NNNp` / `4k`→`2160p` form), null when the name has none. The table view (`web/src/lib/features/search/table/`) gets a Resolution column, hidden by default for a column choice saved before this change (as X-search-toolbar did for Raw name), shown by default otherwise.

## Step 0 (send me, then wait for "go")
1. Where the per-torrent resolution already lives (search document field, tag set, store column or release-parser output), with file:line, and whether the search route can read it for each hit without an extra store or index read per hit. If it needs one, give its cost.
2. The response change: the field name, its type, the OpenAPI schema, null vs absent, and its canonical values (the same strings `res:` writes, so a click-to-filter later can use them). Confirm no client breaks (the web reads the generated `schema.d.ts`).
3. The latency budget (§3 search route): how you'll show it holds (the search bench, if `bench/` covers it, before and after).
4. The table: the column id, header text (search messages en/zh), its sort (none unless the API already sorts by it), its width, and the saved-columns migration.
5. Files outside owns you need (the store, index or search-doc file), with why. Files and an estimate.

## Rules
- The value comes from what the index already holds. Don't parse names in the API or the web.
- No new `unsafe`. No fixed sleeps; condition waits with deadlines ≥ 10 s.
- `docs/metrics.md` §7.1 only if the route's documented response or budget text changes.
- Size: the Search table mode (on demand) entry is 6.12/6.5 kB, and the Results chunk is at its cap (40.85 kB). Nothing may land in Results; `.size-limit.mjs` is not yours, and no raise is expected. If the table entry would pass 6.5 kB, stop and send me the numbers.
- Never beside X-detail-depth (detail/**, routes/t/**), X-settings-find (settings/**) or X-insights-story-stages (insights/**). Ask before touching any file outside owns.

## Owns
- `crates/api/src/dto/search.rs`
- `crates/api/src/search.rs`
- `crates/api/tests/openapi.rs`
- `web/src/lib/api/openapi.json`
- `web/src/lib/api/schema.d.ts`
- `docs/metrics.md`
- `web/src/lib/features/search/table/**`
- `docs/decisions/2026-11-22-X-hit-resolution.md`
- Tests next to those files. Refine at step 0 (the store or index read may need its file).

## Branch and PR
- Branch `slice/X-hit-resolution`. PR title "X-hit-resolution: Search results carry the release resolution, for a Resolution column".
- Gates: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar-api -p lodestar-search`, and in web/ `npm run check`, `npx vitest run`, `npm run build`, `npm run size:ci`, and the search table e2e specs (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium`; never `playwright install`).
- A screenshot-changing edit needs the update-snapshots marker; keep only search PNGs from the regenerated set.
- Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
