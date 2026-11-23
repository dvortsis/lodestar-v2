# X-unverified-cap-index decisions

The usenet cap's cost (UN-1's measurement, `docs/decisions/2026-11-12-UN-1.md`, "The cap's cost") taken out of the writer's transaction by one migration, **V16**, before usenet writes can default on (runner, §2.26). UN-0 and UN-1 proposed the two parts; this slice builds them.

## What V16 adds (`crates/store/src/schema.rs`)
- **An index on `unverified_items (network, id)`, `unverified_items_network`.** The cap's eviction (`DELETE … WHERE id IN (SELECT id … WHERE network = ?1 ORDER BY id LIMIT ?2)`) now reads a network's lowest ids in order. Before, it read the whole usenet range of the `(network, key)` index and sorted it in a temporary B-tree. The tier purge's page (`purge_network`) uses it too.
  - Column order: `network` first, because every query on it fixes the network and walks ids in order.
  - A test checks both query plans: the index is used, with no temporary B-tree and no table scan (`unverified::tests::the_cap_and_the_purge_use_the_index`). The SQL is held in constants (`EVICT_OLDEST`, `PURGE_PAGE`), so the test checks the statements the code runs.
- **A kept count per network, `unverified_counts (network, n)`, kept by triggers.** The cap reads one row instead of counting the tier, so a write under the cap does no counting, and a write past it counts nothing either: it knows the excess at once.
  - **Three triggers** on `unverified_items`: after insert (an upsert that adds 1), after delete (subtracts 1) and after an update of `network` that changes it (moves 1). They run in the same transaction as the write. A crash loses both or neither, so the count is exact and crash-safe without any recount at start.
  - **Triggers, not counting in `put_one` and the purge paths:** rows also go through V14's content-safety triggers (a tombstone, quarantine or floor hash), the snapshot purge, promotion and the re-judge sweep. A count kept in Rust would have to be repeated in each, and would miss the SQL-only ones.
  - **What could bypass them, and why nothing does:** a `REPLACE` conflict deletes a row without firing delete triggers unless `recursive_triggers` is on. Nothing writes `unverified_items` with `REPLACE` (checked; the schema comment says so for later writers). Nested triggers (V14's content-safety deletes) fire them as usual.
  - **No `CHECK (n >= 0)`.** A bookkeeping check must never fail a content-safety delete. `stored_count` reads a negative or missing count as 0 instead, so a count too low stops eviction rather than failing a write. A count too high would evict below the cap on each adding write, and unlike the old `count(*)` it wouldn't correct itself. Nothing writes the table in a way the triggers miss, and the property tests hold the count to `count(*)` across every path; a recount pass can be added if a later writer ever needs one.
- **A refinement of the brief (§2.19):** the slice's note asks for "a per-network count so counting runs only past the cap". At the cap every adding write is past it, so counting only then would still count on every write there. The kept count is exact, so nothing counts at all, under the cap or past it.
  - **The backfill** counts the rows already stored (`INSERT … SELECT network, count(*) … GROUP BY network`), after the index exists.
- **`Store::unverified_count` reads the kept count** instead of `count(*)`. It's the number the tier purge offers and the cap bounds, and they now agree by construction.
- **`has_unverified` is unchanged** (an `EXISTS`, which uses the new index).

## Measured
UN-1's own measurement, unchanged (`usenet_cap_cost_at_one_million`, `crates/sources-plugins/tests/prowlarr_usenet.rs`, `--ignored`, release build): 1M BitTorrent unverified items plus the usenet items shown, writes of 256 finds, a poll of 4,096 results. Run before and after on the same machine (12 threads, 15 GB, a WSL2 disk); after is the median of two runs, which agreed within about 1 ms.

| Store | Usenet items | Before: one write, median (max) | After | Before: a poll | After |
|---|---|---|---|---|---|
| in memory | 100k | 7.8 ms (9.1 ms) | 7.1–11.8 ms | 0.13 s | 0.12–0.20 s |
| in memory | 1M, at the cap | 71.6 ms (74.6 ms) | 9.8–10.6 ms (12.3 ms) | 1.13 s | 0.16–0.17 s |
| file | 100k | 7.5 ms (27 ms) | 7.7–8.8 ms | 0.18 s | 0.29 s |
| file | 1M, at the cap | 78.0 ms (181 ms) | 20.3–21.9 ms (128 ms) | 1.74 s | 0.88–0.89 s |

- **In memory, the cap now costs about what 100k does:** the count and the sort are gone.
- **On a file, a write at the cap costs about a quarter of what it did,** and a full poll holds the writer for about half as long. What's left is the eviction's own deletes: 256 rows a write, whose entries in the `(network, key)` index sit on random pages (the key is a hash), so they miss the page cache on a 1M tier. That's the price of deleting, not of finding what to delete. It isn't worth more schema here.
- At 100k, each insert now writes one more index entry and one count row. The medians moved by about 1 ms. The file poll's total rose from 0.18 s to 0.29 s, carried by one or two slow writes in each run (64 ms, the same in both after-runs, likely a WAL checkpoint), not by the medians.

## Migration time
`v16_migration_time_on_a_2m_item_store` (`schema.rs`, `--ignored`, release): a V15 store of 1M BitTorrent and 1M usenet unverified items, 477 MB. **V16 takes 0.61 s alone, 1.40 s with the pre-migration snapshot.** Building the index and the backfill are one pass each over the table. A real store has far fewer unverified items than torrents (the snapshot, `VACUUM INTO`, grows with the whole store, as every migration's does: about 300 MB at 1M torrents in SRC-1's measurement), so the snapshot dominates, as it does for every migration.

## Tests
- `schema.rs`: `v16_counts_a_v15_store_and_comes_back_after_a_rollback`. Up: a V15 store with BitTorrent and usenet rows migrates once, keeps every row, takes `pre-v16`, backfills the counts, and the triggers count an insert, a tombstone's delete and a network change. Down: the snapshot restored over the store is V15, with no V16 object, and a V15 build opens it and writes to it. Up again: V16 counts what that left, and takes `pre-v16.1`.
- `schema.rs`: a fresh store's schema equals one upgraded from V14 and from V15; V12–V15 builds refuse a V16 store untouched; the V15 test now stops at V15.
- `unverified.rs`: the query-plan test above, and a property test of the cap with small caps (1–7) over random batches of new and repeated keys: after each write the kept count is the rows, and an adding write leaves exactly the newest `cap` items.
- `tests/unverified_cap_index.rs`: a property test over random mixes of BitTorrent and usenet finds (bare or with claims), each content-safety hold, the re-judge sweep (holding or not), promotion, crawling, the tier purge, a source purge, a raw network change and a raw delete, checking every network's kept count against `count(*)` after each step. Plus each path one at a time.
- `snapshots.rs`: a V16 snapshot's counts agree with its rows after the snapshot purge deletes unverified items.

## Outside the slice's paths (named for the runner)
- **`crates/store/src/schema.rs`:** V16 and its tests. **V16 is taken provisionally:** no other open PR held a migration (SRC-3b, which shares the queue, has none). The runner confirms or assigns another number.
- **`crates/store/src/snapshots.rs`:** `Plan::KNOWN_THROUGH` to 16, as its own comment asks of every new migration. V16 changes nothing in the snapshot purge (its triggers count the purge's deletes), so V16 shares V15's plan. Plus the snapshot count check above.
- **`crates/store/tests/swarm.rs`:** one line. Its V4 test rolls a current store back by hand, so it now drops `unverified_counts` too, or V16 couldn't create it again.
