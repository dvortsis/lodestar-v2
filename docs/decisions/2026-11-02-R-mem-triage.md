# R-mem-triage decisions (triage of rc.2's anonymous-memory slope)

Runner brief R-mem-triage, 2026-11-02, Track R. rc.2 is `release/v0.5.1` at 6be95525. The Linux Docker soak r3 measured its `private_bytes` slope (the cgroup's `anon`) at 6.66 MiB/h, 95% CI [4.62, 8.70], over baseline hours 12–16, about 10–14 KiB per stored torrent. This file records what was measured, what was ruled out, and what is still open. **No code change to the shipped binary is made here.**

## How it was measured
- **Two harnesses, in `bench/examples/` as dev-dependencies, so they stay out of the binary and the bench gates' change detection:**
  - **`mem-ingest`** runs the real ingest path: `Pipeline::start_with` (intake, have-filter, leases, fetch pool, prepare, writer, SQLite), the hot columns, and a `Serving` live index with its title FST, wired as `serve` wires them.
    - Synthetic torrents come from the bench generator, with realistic piece lists. A stand-in source and fetcher replace the DHT and the peer wire.
    - It reports `Anonymous` from `/proc/self/smaps_rollup`, the process's share of the soak's cgroup `anon`.
    - It can run under mimalloc (as shipped), the system allocator or dhat's heap profiler.
  - **`mem-churn`** drives the fetch pool's eviction path the way a busy DHT does:
    - a source emits new keys with no peers;
    - a stand-in finder refuses seven lookups in eight and answers the eighth with no peers;
    - the pool fills to its 200k cap and evicts one key for every new one.
- **Built as the image ships it** (static musl via `cargo zigbuild`, mimalloc) as well as for gnu. The libc changes the result (below), so the musl figures are the ones that apply to the soak.

## What was ruled out (measured)
- **No unbounded or oversized per-torrent structure on the ingest path.**
  - dhat's live Rust heap was 41.6 MB at 20k torrents and 52.6 MB at 33.6k. Without the harness's own queue that is about 150 B per stored torrent, which is the title FST, the hot columns, the have-filter and the gate's buffers, as designed.
  - SQLite's own memory (`sqlite3_memory_used`) is flat at about 8 MiB.
- **On the shipped (musl) build, the ingest path retains about 1.3 KiB of anonymous memory per stored torrent at 40k, all inside mimalloc's arena.**
  - It doesn't depend on the ingest rate (10 or 400 per second), the writer heap (64 or 256 MiB) or the title rebuild.
  - Most of it goes with the live index: without it, the gnu build grew about 0.7 KiB per torrent instead of 3.
  - It is allocator retention, not live data: a forced `mi_collect` gives back only about a fifth of it.
  - That is roughly 13–20% of r3's slope.
- **On a glibc build the ingest path retains 3–4 KiB per torrent.**
  - The extra comes from the file index's doc store: zstd at level 19 over 1 MiB blocks (`crates/search/src/files.rs`). Each segment close builds an approximately 17 MB compression context with libc `malloc` on a thread Tantivy has just spawned.
  - glibc then raises its dynamic mmap threshold, and its per-thread arenas keep that memory.
  - Pinning the threshold (`GLIBC_TUNABLES=glibc.malloc.mmap_threshold=131072`) removes it.
  - musl's allocator always maps and unmaps blocks that size, so **this is not the soak's cause**. It matters for gnu builds only, and no packaged build is gnu.
- **No per-key leak in the fetch pool under eviction churn.**
  - 2.5M evictions at the 200k cap, about 14 h of CR-0's eviction rate (≈178k/h), left anonymous memory flat at about 170 MiB.
  - Pending keys and outstanding leases stayed at 200k.
  - The pool at its cap costs about 690 B per queued key, about 150 MiB: oversized, but a plateau.
- **The big DHT structures plateau early.** In CR-0's hourly samples (`docs/soak/evidence/2026-10-30-cr0-final/`), `fetch_pending` and `dht_sample_cooling` reach their 200k caps by hour 6.5 and stay there. Private memory still drifts about 4 MiB/h while stored torrents go from 1k to 18k. The slope therefore comes after every large capped structure has filled.

## Inventory (rc.2)
Everything below is bounded, except where marked.

| Structure | Bound |
|---|---|
| Have-filter | Bloom filter, rebuilt at twice the size when full; about 1.3 MB at 20k |
| Gate queue | `WAITING`, with drops counted |
| Funnel and rollups maps | Drained at every flush |
| Fetch pool `entries` | `max_pending` 200k (architecture §6) |
| Pool `admitted` | Compacted at 2× entries + 1024 |
| Pool `lookup_backlog` | Cleaned lazily. In the 10-24 run it was 204,821 at the end, against 200,000 pending |
| Pool `recent` | 50k |
| Coordinator `LeaseTable` and `WorkerLeases` | Track pending keys. **Unbounded leak, about 200 B per key:** a fetched key rejected in prepare and seen again gets a new lease, which the pool drops as a recent duplicate without ending it (about 2/h in CR-0; filed as a follow-up) |
| DHT sampler states and expiry | `MAX_KNOWN` 200k, about 35–45 MB at the cap |
| Sampler frontier | 20k |
| `PeerStore` | 10k hashes × 50 peers, 30 min TTL |
| Routing table | K = 8 entries and 8 replacements per bucket |
| Lookups | 16, 32 or 16, with deadlines |
| Pending queries | `max_outstanding` |
| Seen and contacted filters | Fixed size |
| Scrape lanes | `LANE_CAP` 50k per lane |
| Scrape rounds | Expire at their deadline |
| Search result, facet and file caches | `CacheConfig` |
| Search per-segment sets | Per search |
| Search leases | Have a TTL |
| Title FST | One set, rebuilt at most once a minute |
| zstd store dictionaries | Never trained at run time |
| Log ring | 5000 records or a 4 MB estimate. **Fills slowly at a low log rate**, so over 24 h it looks linear: a few MB |
| Log fold table | 1024 keys |
| Log start-up buffer | 2000 lines |
| SSE ring | 512 |
| SSE broadcast lag | 256 |
| Per-client SSE queue | 16 |
| Insights page caches | 8 |
| `request_log` | 512 routes |
| SQLite page caches | About 2 MB per connection (the default) |

## What is open
- **About 80% of r3's slope (roughly 5 MiB/h) is not reproduced** by anything that runs without the live network.
  - The harnesses lack DHT replies and their timing, real peers, real TCP fetch failures (about 16k/h in CR-0) and hours of wall time.
  - The next step is to diff r3's own metrics snapshots (about 250 series a minute, already collected) against `private_bytes` over hours 6–16, and find whichever gauge rises with it.
- **The size of a re-soak is the runner's to set under build plan §0.4.** This file changes no line in the crawler lifecycle, the sampler or the endpoint policy: it only adds two dev-only examples and their dev-dependencies.
