# X-remote-verified decisions

A torrent a remote worker brought in is indexed `verified:imported`, never `verified:yes`, until it's re-verified. #1139's review asked whether such a row's metadata is verified or only the worker's word. It's the worker's word. Decided by the runner (step 0, 2026-11-27), flagged to the owner under `docs/owner-decisions.md` §2.19. Amends `2026-11-27-X-import-verified-no.md` "Read from the row".

## What the coordinator checks of a remote record (step 0)
- **It never hashes the info dict to the key, and can't.** The wire format never carries `pieces` (`crates/pipeline/src/wire/results.rs`, WK-6), and the swarm hashes and leased key come from the worker. The intake (`remote.rs` `build`, WK-9) rebuilds the file list from the worker's blob, checks it against the worker's own `size`, `file_count` and piece count, and rebuilds the flags and enrichment. That's consistency, not a hash to the key.
- **That holds for every remote record.** `archive_raw` only decides whether the worker's raw dict is kept, and it's never hashed either.
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
- **It's no longer than #1139's text,** so the size limits #1139 set hold without a raise (Alert route 35.38 of 35.39 KB, Search results 44.07 of 44.09 KB, Search cards 8.16 of 8.17 KB). A longer first draft went over by 30–38 B.
- **The API doc comments** on `imported`, `verified` and `AstVerified::Imported` say the same; `openapi.json` and `schema.d.ts` are regenerated.

## For WK-13
- **When it clears `REMOTE_UNVERIFIED`, it must log the torrent for reindexing** (`reindex_log`), or the row stays `imported` in the index after it's re-verified. The runner adds this to WK-13's notes.
- **A row that fails re-verification** is WK-13's to handle (quarantine, purge). Until then it stays `imported`.
