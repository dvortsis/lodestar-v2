# X-sp4-followups decisions: a tighter SP-4 set index; the pre-hiding guard deferred

Runner brief, 2026-11-05T23:40Z; the two follow-ups named in `docs/decisions/2026-11-05-RP-5.md` ("The set", "A threat, for the runner"). Architecture §20.6. No migration and no change to the stored rows. No owner decision, security or content-safety decision, or owner-set number changes. One refinement, flagged for the runner (§2.19): the limit on rows held at once, below.

## The pre-hiding guard: deferred to `X-sp4-guard` (runner, 2026-11-06T00:05Z)
The brief asked for a guard: don't add a payload signature to SP-4's set while the store already holds a torrent with that payload that isn't hidden or demoted by a spam rule. Such a payload is already known as a real release.
- **What the guard needs:** a lookup from a payload signature to the stored torrents with it. Nothing provides one today:
  - The store has none. `torrents` keeps `size` and `file_count`, with no index on either. A torrent's payload can only be read by decompressing its `torrent_files` blob.
  - The search index has a `payload_sig` field (SB-6, `INDEXED | FAST`), but its generation is 0 in `population::GENERATIONS`, so no producer fills it. `lodestar-pipeline` doesn't depend on `lodestar-search`, and the index trails the writer's commits.
  - CS-8a's `PayloadSig` is matched against `cs_sigs` only. It never looks up torrents.
- **The three designs reported to the runner:**
  - **A, a store column and a partial index (a migration):** `torrents.payload_sig INTEGER`, set for specific payloads at insert and at an in-place upgrade, with `CREATE INDEX … ON torrents (payload_sig) WHERE payload_sig IS NOT NULL`.
    - The guard reads `SELECT id, flags FROM torrents WHERE payload_sig = ?` only when SP-2 or SP-3 would add a signature. A row without `HIDDEN | DEMOTED` counts as a real release. A row with them counts unless its newest hit (`latest_hits`) is a spam-pack rule. Earlier commits in the same batch aren't stored yet at judging, so they come from a map kept for the batch.
    - Disk: about 9 B in the row plus about 20 B in the index, so 25 to 30 B per torrent. That's about 5% of M0's measured 565 B, or about 300 MB at 10M torrents.
    - Ingest: one more B-tree insert per commit, with a random key like the info-hash index. It could come close to the 5% writer gate (about 1.4 µs) on a large, cold store.
    - Rows stored before the upgrade stay NULL, which errs loose for them.
  - **B, the search index's `payload_sig`:** ship its producer (generation 1, with a backfill walk), and give the writer a lookup through a trait the binary injects.
    - No store bytes, and no cost on the writer's insert.
    - It lags the writer's commits, which errs loose. It couples two crates, and the backfill is a full walk.
  - **C, no guard yet:** the remedy stays an exception for the planted original or its payload.
- **The runner chose C for this PR, and B for the follow-up.** Reasons:
  - A adds a migration, about 300 MB at 10M torrents and an insert on every commit to defend against a narrow, rare attack whose remedy already exists.
  - The M7 plan (#514) already schedules the `payload_sig` producer (the GS-* chain), so B's lookup comes without a second copy of the data.
- **`X-sp4-guard`** depends on that producer. It uses B, an injected trait plus the batch-local map, and the runner adds its `slices.yaml` entry.
  - It's decided at the writer's commit, in commit order, with no store transaction held while waiting on the set's lock (RP-5's lock order).
  - It errs loose for a real release committed too recently for the index to show it.
- **What the guard will cost in recall:** spam that copies a real release's exact file sizes isn't cloned. SP-2 or SP-3 still hides each such torrent on its own; only SP-4's inheritance stops for that payload.
  - The guard also can't help when the planted original arrives before the real release: no real release is stored yet. The remedy is the same exception.
- **Until then:** RP-5's "A threat, for the runner" still holds as written.

## The tighter index (`crates/filter/src/spam_sigs.rs`)
- **The index holds 4-byte locations, not keys.** `Index` is an open-addressed table of `u32` slots. A slot holds a row's place in memory (19 bits) and an entry's index in that row (13 bits, since a row as read holds up to 8,191 entries). The key is read back from the entry it points at, so it isn't stored twice.
  - It's at most half full, which means 2M buckets (8 MB) at the cap. Probing is linear. A removal shifts the run after it back, so there are no tombstones.
  - `u32::MAX` marks an empty bucket. No slot can equal it, because the largest index is 8,190 (`a_slot_never_reads_as_empty`).
- **Hash flooding:** buckets come from SipHash under keys drawn at start (`RandomState`, the same hasher the std `HashMap` used), never from the signature's own bits, which a sender can steer (`fmix64` can be inverted). The table is at most half full, so a probe reads about two buckets.
- **Rows are held by place:** `Rows` keeps them in a `Vec` and maps row numbers to places in a `BTreeMap`, which gives the ordered walks the cap and compaction need. A deleted row's place is reused.
  - Compaction rewrites a row in place. It finds each live add's bucket before the rewrite and points that bucket at the add's new index afterwards. Buckets don't move in between.
- **A load sizes the index once,** for the adds it reads up to the cap. Growing step by step left each outgrown table resident under glibc: 8 MB more after a load than after the same set filled by adds.
- **Refinement, flagged (§2.19): at most 524,288 rows held at once** (`MAX_HELD_ROWS`, the slot's 19 bits). About 2,000 rows hold the cap, and no set reaches the limit in practice. Past it:
  - A row a load can't hold is treated like a row that doesn't read: lost, listed as `packs.sp4.row.<n>`, left alone and never written over. That loses its signatures, as a row that doesn't read does.
  - A row that can't be started is treated like the row numbers running out: the set stops changing and lists `packs.sp4.row.numbers`. That errs strict.
  - Either lasts until a restart. Every change appends first: an add, a removal, and so the evictions and compactions that could delete a row. So a full set can't free a row either.
  - `rows_past_what_memory_holds_are_lost_or_never_started` checks both, with a lower limit.
- **Unchanged:** the stored rows and their format, the load rules (fail closed per row, last entry wins, the cap applied again), compaction, releases, and every SP-4 behaviour. RP-5's tests in `crates/filter/tests/sp4.rs` pass unchanged, including the reload property test `a_reload_holds_what_memory_held`. The unit test that checks the index against the rows now also runs at 5,000 signatures, with long probe runs and reused places, and checks the same again on its rows loaded into a fresh set.

## Measurements
Release build, this session's container, each run in its own process, three runs each. Memory is measured two ways, since the workspace forbids `unsafe`, which a counting allocator would need:
- **Resident:** the growth in resident memory (VmRSS) while the set is built. That's what the process holds at the end, the allocator's holes included. It isn't the peak.
- **Held:** the bytes the set's structures hold, counted from their capacities (`SpamSigs::held_bytes`, which errs high on the stale map).

| At the cap (1M signatures) | RP-5 (`HashMap`), resident | Now, resident | Now, held |
|---|---|---|---|
| Loaded from its rows (`memory_at_the_cap_loaded`) | 53.1 to 53.2 MB | 17.0 to 17.1 MB | 16.5 MB |
| Filled by adds, 10,000 past the cap (`memory_at_the_cap_added`) | 44.5 to 44.6 MB | 17.2 to 17.4 MB | 16.6 MB |
| Filled by adds, then a third of them re-added at random (`memory_at_the_cap_readded`) | not measured | 26.0 to 26.5 MB | 21.8 MB |
| A lookup that hits, keys spread over the set | 101 to 114 ns | 111 to 173 ns | |
| A lookup that misses | 38 to 43 ns | 95 to 148 ns | |

- **The clean sets are a best case.** A re-add (a payload hidden again) leaves a stale add in its old row until that row is a quarter dead and compacted. The set counts stale adds per signature in a std map. The re-added case covers that: 81,557 dead entries in 2,610 rows, and 75,913 signatures in the stale map. RP-5 had the same map, so it isn't new here.
- **The 25 MB gate holds on what the set holds** in all three cases, and on resident memory for the clean ones. The tests assert it. Under glibc, the re-added case is about 4 MB over resident: the holes compaction's reallocations leave. Two things were tried, and neither changed it by more than noise: rewriting a row in its own buffer, and starting each row at full capacity. Release builds on musl use mimalloc (`crates/store/src/memstats.rs`), which this container didn't measure.
- **Running them:** they're `#[ignore]` measurements. `cargo test --release -p lodestar-filter --test sp4 -- --ignored --nocapture --exact memory_at_the_cap_loaded` (and `…_added`, `…_readded`).
- **Lookups are slower,** mostly misses: each bucket a probe passes costs a read of the entry it points at, where the std table matched a control byte. On the writer that's 0.08 µs a crawl commit, about 0.3% of an insert:

| `sp4_writer_cost` (release; RP-5's code run twice here, this PR's three times) | RP-5 | Now |
|---|---|---|
| Per insert | 26.2 to 26.4 µs | 24.8 to 30.3 µs |
| A crawl commit, set at its cap | 0.23 µs (0.9%) | 0.31 to 0.32 µs (1.0 to 1.3%) |
| A batch with hides, set at its cap | 199 to 217 µs a batch (3.0 to 3.2%) | 173 to 202 µs a batch (2.6 to 2.7%) |
| A flood of one payload after a release | 8.0 to 8.2 µs a batch | 7.6 to 7.9 µs a batch |

- Both are within RP-5's 5% gate. A tag byte per bucket would bring the miss back to about one read, for 2 MB more. It isn't worth it at 0.3% of an insert.

## Outside the brief's listed paths
None. `runner/slices.yaml` and `runner/dashboard.yaml` are left to runner PR #522 (runner, 2026-11-06T00:13Z).
