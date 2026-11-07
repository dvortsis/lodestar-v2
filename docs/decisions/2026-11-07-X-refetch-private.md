# X-refetch-private decisions

- **The finding.** At every start, the pipeline queues imports whose file list is partial for refetch through the first enabled peer finder, with `private_source: false` (`crates/pipeline/src/lib.rs`, the start's partial-refetch block). The keys came from `Store::partial_hashes`, which selected `flags & FILES_PARTIAL != 0` with no privacy filter. `FILES_PARTIAL | PRIVATE`, `FILES_PARTIAL | PRIVATE_SOURCE` and `FILES_PARTIAL | REMOTE_UNVERIFIED` are all real states. So a private partial import was sent to public DHT and PEX lookups after every restart, which §18.7 and BEP 27 forbid. A refetch queued as public could also clear a private pool entry for the same key, because the pool's merge keeps an entry private only while every find is private (`e.private &= ...`).

- **The fix: filter in the query.** `partial_hashes` now also requires `flags & flags::PRIVACY = 0`, so it never returns a BEP 27 private torrent, a private source's find or an unverified remote row.
  - **Why the query, not `private_source: true` at the queue.** A private refetch has nowhere to go: the only place it's queued is a public peer finder. Carrying the key as private would leave it in the pool doing nothing, using up part of the 50,000 cap (`PARTIAL_REFETCH`). Filtering in SQL is simpler, and provably safe because nothing is queued at all. It also means the cap counts only keys that can actually be refetched.
  - **The name stays `partial_hashes`.** It has one caller in production, the start's refetch, and its doc comment now states the guarantee. That kept the change out of other slices' test files.
  - **Private partial imports stay partial.** Their file lists are never fetched from the public swarm. Upgrading them through a private source (a client's own copy, say) is not built; that would be a separate slice.
  - **Public parity.** Public partial imports are refetched exactly as before: same order (oldest first), same cap, same queue entry.

- **The audit** covers every construction of `QueuedKey`, `Found`, pool entries, `FetchedKey`, `Results` and `Interest`, and every literal `private_source: false`, in `crates/pipeline/src/**` and `crates/sources/src/**` on `main` 8979a9b7. Only the start's refetch was wrong.

  | Site | What it builds | Verdict |
  |---|---|---|
  | `pipeline/src/lib.rs`, start's partial refetch | `QueuedKey`, `private_source: false` | **Fixed**: the keys are public by the query's filter. A comment says so. |
  | `pipeline/src/lib.rs`, `saved_refetch` | drops saved refetches at start | Safe: they're queued afresh from the store, through the fixed query. |
  | `store/src/sources.rs`, `load_fetch_queue` (restoring the saved queue) | `QueuedKey` | Safe: `private_source` round-trips through the `QUEUE_PRIVATE` bit that `save_fetch_queue` writes. |
  | `sources/src/pool.rs`, `restore`, `snapshot`, `queued`, admission and merge | pool `Entry`, `QueuedKey` | Safe: privacy is taken from the queued key or the find, and the merge keeps it private only while every find is private. The eviction path sets it `true`. |
  | `sources/src/plugin.rs`, `Found::key` | `Found`, default `false` | Safe: a private plugin sets it itself (`clients.rs` always, `prowlarr.rs` per indexer). The DHT's finds (`pipeline/src/dht.rs`, `found`) are public by origin. |
  | `pipeline/src/writer.rs`, `lease_free` (the torrent cache's commit) | `Results`, `private_source: false` | Safe: a cache never looks up a private key (`Interest` comes from the store, through `Gate::check`). The commit re-reads the store's privacy (`stored_private`). Already commented. |
  | `pipeline/src/writer.rs`, claims re-ingest after commit | `Discovery`, `private_source` from the commit | Safe: taken from what the commit stored. |
  | `pipeline/src/torrent_cache.rs`, `interest` | `Interest` | Safe: `Withheld::Private` from the store gives `private_source: true`, which `Lookup::new` refuses. |
  | `pipeline/src/unverified.rs`, held items and their recovery | `Found` | Safe: privacy comes from the item's stored `private_source`. |
  | `pipeline/src/stages.rs`, `worker.rs` | `ResultRecord`, `FetchedKey` | Safe: copied from the fetched key. The other literals in `worker.rs` are tests. |
  | `pipeline/src/remote.rs` (WK-9b's) | remote results | Safe: a result whose every source `finds_private` is private. Not edited here. |
  | Every other literal `private_source: false` (`writer.rs`, `wire/results.rs`, `adult_enrich.rs`, `lib.rs`'s `a_saved_refetch_is_matched_by_code_and_owner`) | test fixtures | Not production paths. |

- **Tests**, each failing without the fix:
  - `crates/store/tests/partial_privacy.rs`: a store with public, `PRIVATE_SOURCE`, BEP 27 (`info.private` and the bare `PRIVATE` bit) and `REMOTE_UNVERIFIED` partials returns only the public one. Four older private partials don't use up a cap of 1. Public partials still come back oldest first.
  - `crates/pipeline/tests/refetch_private.rs`: the real pipeline, started twice on the same database file. A recording peer finder, with a fake peer that serves every torrent, is asked only for the two public imports. The private partials stay partial, and none of them is in the saved fetch queue afterwards.

- **v0.5.1 exposure and backport.** `release/v0.5.1` has the same refetch (`lib.rs:913-922`) and the same unfiltered `partial_hashes`. There, `FILES_PARTIAL` rows come only from the bitmagnet importer, and that importer copies bitmagnet's `private` column into the BEP 27 bit. So a BEP 27 private torrent that bitmagnet indexed with a partial file list is looked up on the public DHT after every v0.5.1 start. v0.5.1 has no private sources, so `PRIVATE_SOURCE` and `REMOTE_UNVERIFIED` partials can't exist there. The practical risk is low: bitmagnet itself found those hashes on the public DHT, and Lodestar only sends `get_peers` for them and never announces. It is still a BEP 27 violation. **Recommendation:** backport the one-line query filter (and the store test) to any later v0.5.x patch release. It doesn't justify a release on its own. The runner and the owner decide on releases.
