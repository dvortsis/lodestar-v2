# X-remote-verified decisions

A torrent a remote worker brought in is indexed `verified:imported`, never `verified:yes`, until it's re-verified. #1139's review asked whether such a row's metadata is verified or only the worker's word. It's the worker's word. Decided by the runner (step 0, 2026-11-27), flagged to the owner under `docs/owner-decisions.md` §2.19. Amends `2026-11-27-X-import-verified-no.md` "Read from the row".

## What the coordinator checks of a remote record (step 0)
- **It never hashes the info dict to the key, and can't.** The wire format never carries `pieces` (`crates/pipeline/src/wire/results.rs`, WK-6), and the swarm hashes and leased key come from the worker. The intake (`remote.rs` `build`, WK-9) rebuilds the file list from the worker's blob, checks it against the worker's own `size`, `file_count` and piece count, and rebuilds the flags and enrichment. That's consistency, not a hash to the key.
- **That holds for every remote record.** `archive_raw` only decides whether the worker's raw dict is kept, and it's never hashed either.
- **This is a trust ruling, not a missing check.** The worker hashes the dict during its own fetch (§19.4); the coordinator just can't confirm it. So `yes` would rest on the worker's word.
- **A remote result that upgrades a partial import** keeps `IMPORTED`, gains `REMOTE_UNVERIFIED`, and its import record is marked `verified` (`store` `verified_upgrade`) on the worker's word. Checking the flag first keeps that row `imported` too. Anything else that reads `import_quality` still sees `verified`; that predates this slice and is left for WK-13 (below).
- **Only WK-13's re-verification checks a remote row,** by re-fetching its dict, and only it clears `REMOTE_UNVERIFIED` (`2026-11-07-WK-9.md`). It isn't built yet.

## The rule (`stored_verified`, `crates/search/src/doc.rs`)
- **A row with `REMOTE_UNVERIFIED` is `imported`,** whatever `IMPORTED`, `info_size` or an import record says. It's checked first, so a `verified` import record can't vouch for it. That fails closed, as the flag already does for privacy.
- **`imported` stays shown by default** (`verified:yes,imported`), so a worker's finds don't disappear from search. They're only left out of `verified:yes`.
- **The API follows the index value, with no code change:** `api/search.rs` `is_import` goes through `stored_verified`, so a remote row's search hit and torrent detail say `verified: false` and `imported: true` (`api/tests/provenance.rs`).
- **No store migration and no new flag.** The flag WK-9 already sets is enough.

## Existing indexes: the `verified` generation goes to 3 (`population.rs`)
- **Every existing index rewrites `verified` through the population walk,** as #1139 did with generation 2, so remote rows indexed `yes` become `imported`. While the walk runs, the notices and facet behaviour #1139 describes apply.

## Wording (runner's grant)
- **The Imported help text covers both kinds of row:** "Not checked against the torrent's hash: from another indexer or a remote worker." (zh: 未与种子的哈希核对：来自另一个索引器或工作节点。). The label stays "Imported", the `verified:imported` value's name.
- **The glossary splits the two meanings (runner, option A):** a new `verified_imported` row covers the badge and the `verified:imported` value (imports and remote finds), and the `imported` row, the import term, is unchanged. Insights' imported counts and `flag:imported` stay keyed on `IMPORTED`, so a worker's find still counts as crawled there.
- **It's no longer than #1139's text,** so the size limits #1139 set hold without a raise (Alert route 35.38 of 35.39 KB, Search results 44.07 of 44.09 KB, Search cards 8.16 of 8.17 KB). A longer first draft went over by 30–38 B.
- **The API doc comments** on `imported`, `verified` and `AstVerified::Imported` say the same; `openapi.json` and `schema.d.ts` are regenerated.

## For WK-13
- **When it clears `REMOTE_UNVERIFIED`, it must log the torrent for reindexing** (`reindex_log`), or the row stays `imported` in the index after it's re-verified. The runner adds this to WK-13's notes.
- **A row that fails re-verification** is WK-13's to handle (quarantine, purge). Until then it stays `imported`.
- **Scale: whether WK-13 re-verifies every remote row or a sample decides how many stay `imported`.** §19.4 samples 0.5% of each worker's results; WK-13's notes say it clears the flag on remote rows. If only samples are re-verified, about 99.5% of worker finds stay `imported` (shown by default, out of `verified:yes`) for good. This is the owner's flag under §2.19: WK-13 should say which, and whether a passing sample vouches for its worker's other rows.
- **An import record marked `verified` by a remote upgrade** should go back to WK-13's rule as well, so nothing that reads `import_quality` takes the worker's word.
