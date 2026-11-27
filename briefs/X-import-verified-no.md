# X-import-verified-no: Imported torrents are never indexed as verified

**Runner brief, 2026-11-27T10:26Z.** Core, Opus (search index provenance; content honesty). Target < 800 changed lines. `/security-review` before you report ready.
- **Branch:** `slice/X-import-verified-no` from fresh `origin/main` (with #1136 merged as e11d5be7).
- **PR title:** `X-import-verified-no: Imported torrents are never indexed as verified`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. A fresh Opus review gates the merge.
- **Add this slice's own `runner/slices.yaml` entry** (kind core, model opus, deps [], your owns, notes "runner 2026-11-27T10:26Z, from the G-import-1M ladder on the test VM") and run `cd tools/status-connector && npm test`. Edit nothing else in that file or in `runner/dashboard.yaml`.

## Why
The Magnetico import test (10M torrents, on the test VM) found every imported torrent indexed as `verified:yes`. `DocProvenance::stored_torrent()` (`crates/search/src/doc.rs:92`) hard-codes `Verified::Yes` for every row in `torrents`. But IMP-1 and IMP-2 store imports as unverified: Magnetico and bitmagnet keep no info dict, so nothing ties the stored fields to the hash (`docs/decisions/2026-10-30-IMP-1.md:87`, `2026-10-30-IMP-2.md:34`). A `verified:yes` search, or anything that tells a user a result is verified, makes a claim the data can't back.

At the same time, architecture §18.5 (line 2122) has imports land in `torrents`, flagged `imported`, and **visible by default**, not in the hidden unverified tier. So the fix must not simply index them `verified:no`: that would hide a whole imported library behind "Show unverified".

## Runner's design (decided under owner-decisions §2.26 and §2.28; flagged to the owner under §2.19)
1. **Three provenance values, not two.**
   - `verified:yes`: the info dict was fetched and hashed to the key, by a network fetch or a full `.torrent` import (ENR-1).
   - `verified:no`: the unverified tier (claims), hidden by default as today.
   - **New `verified:imported`**: a `torrents` row whose metadata came from another indexer's database and was never hashed here (`FILES_PARTIAL` / `IMPORTED` per IMP-1). It is **not** hidden by default.
   - When IMP-06 or a later network fetch verifies an imported row, it becomes `yes` at its next index write.
2. **Read it from the row, never assume.** `stored_torrent()` takes the row's flags, or the loader passes the verification, so a document can't come out `yes` by omission. Find where the indexer builds the provenance (`doc.rs:367` and its callers) and what the row already records. Use the existing flags; add no store migration. If a migration really is needed, stop and message the runner.
3. **The query language.** `verified:imported` parses. `verified:yes` no longer matches imports. The hidden-by-default clause still hides only `verified:no`. "Show unverified" (`verified:yes,no`, SRC-17) must keep imports visible: check how SRC-3's "everything but" compilation (`docs/decisions/2026-11-04-SRC-3.md:25-28`) treats a third value, and keep it one flat intersection, with no score change for verified-only queries. On an index older than this change, rows written before it still say `yes`. Say how they're corrected: an index generation bump, a backfill, or the existing rebuild path. Pick the cheapest correct one and justify it in the decisions file.
4. **The facet and the UI.** The `verified` facet gains `imported`. Find out what the web UI shows for each value (badges, the facet rail, the drawer). If imports need only a label ("Imported", with plain help: "From another indexer's database; the file list wasn't checked against the torrent's hash"), add it within the owns below. If it needs more than labels and one badge, message the runner before building it.
5. **Tests.**
   - A network-fetched row, a full `.torrent` import, a Magnetico import and a bitmagnet import each index the right value.
   - `verified:yes` excludes imports.
   - Default search still shows imports and still hides `verified:no`.
   - "Show unverified" shows all three.
   - A property or table test over the flag combinations.
   - No fixed sleeps.
6. **Docs.** Update architecture §18.6's `verified` values only through your decisions file, which records the third value and why. The query-language user docs gain one line for `verified:imported`; use the user-docs skill.

## Owns
`crates/search/src/doc.rs`, the indexer loader that builds provenance (name the file in the PR), the query parser and compiler files for `verified:` under `crates/search/src/query*`, `crates/search/src/facets/**`, `crates/search/tests/**` except `crates/search/tests/enrichment.rs` (#1135 holds it), the web verified facet/badge files you need (list them in the PR) plus their `messages/{en,zh}.json` pairs, the query-language page in `docs/user/`, `docs/user/revisions.md`, `docs/decisions/*-X-import-verified-no.md`, and this slice's `runner/slices.yaml` entry.

**Not yours:**
- `crates/store/**` (#1135 holds identify_sweep, claims and unverified; message the runner if you need a store helper);
- `crates/dht/**`, `crates/lodestar/src/config.rs`, `crates/lodestar/src/metrics.rs` and `docs/metrics.md` (CR-0w);
- `crates/integrations/**` (X-tls-root-load-event);
- `web/.size-limit.mjs` (ask the runner if a budget moves).

## Done
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test` for search, api, pipeline and lodestar, or the workspace if the disk allows. Say which you ran. The web checks from CLAUDE.md if web changed. Report `ready <n> <full sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) once ci-ok, web-ok and e2e-ok are green. Push once per round of fixes.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
