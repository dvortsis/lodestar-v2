# Soak memory and performance review, 2026-11-04

A read-only review of the v0.5.1 soak evidence, the rc.3 source and the plan,
written while soak r4 (rc.3, Linux Docker) was 5 h into its baseline. Nothing in
the repository, on the soak host or on the Windows machine was changed. Its
purpose: name what can be put in place before the next frozen soak (r5 if r4
fails the memory gate, otherwise the v0.5.2 soak), with the evidence for each.

**Evidence used.** The collector sample sets of every Linux run (r1 `v051-rc1-main`,
`v051-rc2-main`, r2, r3 `v051-rc2-main-r3`, r4 `v051-rc3-main-r4` to
2026-11-04 05:35Z); r3 and r4 metric snapshots; `release/v0.5.1` at `bd9300dd`
(rc.3); tantivy 0.27.0 and libmimalloc-sys 0.1.49 sources from the local cargo
registry; `docs/decisions/2026-11-03-soak-r3-verdict.md`,
`docs/decisions/2026-11-03-X-mmap-cache-prune.md`,
`docs/decisions/2026-10-30-CR-0-acceptance.md` and
`docs/plan/2026-11-03-soak-r3-memory-fix.md`. Memory figures are the
collector's `private_bytes` (cgroup `anon`), in MiB, unless stated.

## 1. Where the memory sits: levels

| Component | Size | Evidence |
|---|---|---|
| Idle process before sampling starts | 55–95 | hours −6 to −0.5 in r3 and r4 |
| **The fetch pool filling to its cap** | **~185 step** at suppression end, in every run | r3 94→279, r4 150→275 within the hour; `fetch_pending` reaches 200,000 |
| Hot columns, have-set, search caches | 1.5, 1.3, 0 | gauges flat for the whole run |
| Corpus-proportional residual | plateau ~420 at 16k torrents (r3) against **~820 at 22k** (`v051-rc2-main`, started from the migrated 7k database) | rc2-main's working set passed 1 GiB at baseline hour 10 |

A pool entry (`crates/sources/src/pool.rs`, `struct Entry`) carries
`peers: Vec<SocketAddr>` with up to `max_peers_per_key = 32` peers at 32 B each,
which is most of the ~925 B per key the r3 verdict measured. (`max_attempts = 4`
counts lookups per key, not peers; within a lookup every peer is tried before
the key moves on, so all 32 can be used. An earlier draft of this review said
otherwise; corrected after the runner's review of #425.) In r3, 5.5 M keys were
evicted against about 2.3 k lookups an hour: **about 99 % of discovered keys
leave the pool untried.** The 200k cap is a one-hour FIFO buffer that costs
185 MiB; lookups, not queue depth, bound throughput.

## 2. The slope (the gate)

**Fixed in rc.3:** tantivy's `MmapDirectory` cache leak. M1-exp measured
2.3 KB retained per commit; at r3's ~665 commits an hour that is ~1.5 MiB/h,
most of the 1.88 the gate measured. The new gauge shows the prune working in r4
(12–72 live entries per index).

**Residual candidates after the fix, none measured yet:**

1. **One commit per torrent.** r4: 3,294 commits for 3,716 torrents
   (`LiveConfig::commit_every` is 2 s, `crates/search/src/live.rs`). With
   tantivy's `LogMergePolicy` defaults (`min_layer_size` 10,000,
   `level_log_size` 0.75 in log2, `min_num_segments` 8), every segment under
   16.8k documents falls in one level, so **the whole main index is rewritten
   every seven commits for the entire 0–16.8k-torrent range that r3 and r4
   cover.** That is roughly 1 GB/h of index writes and a merge high-water mark
   that grows with the index. It is the "merge churn" the verdict named as its
   second candidate, now with its mechanism.
2. **An unmeasured C heap.** The Linux build sets mimalloc as the Rust global
   allocator without the `override` feature (`crates/lodestar/Cargo.toml`), so
   the bundled SQLite and C zstd (the store's `BlobCodec`,
   `crates/store/src/compress.rs`, which reuses one compressor and one
   decompressor per dictionary per connection; and tantivy's `zstd-compression`
   feature) allocate through musl's `mallocng`. (Whether ring allocates on the
   heap is unverified; an earlier draft listed it.) dhat never saw those
   allocations during M1-exp, and the `purge_delay = 0` and `mi_collect`
   experiments could not reach them. This is the blind spot in "the remainder
   is the allocator keeping memory".
3. mimalloc page fragmentation from the commit churn: 0.3–0.8 KB per commit in
   M1-exp's B runs, at most ~0.5 MiB/h.

**A caution on the Windows CR-0 pass** (−0.26 MiB/h, 95 % CI [−0.85, +0.32]):
it ran the same tantivy and the same 2 s cadence, so the leak was present there
too. Its private bytes started the baseline at 513 MiB and ended at 514 after a
613 peak, so a slow leak could hide behind the drain of an early high-water
mark. It is not a clean control for the Linux result.

## 3. r4 watch items

- r4 runs **30–45 MiB above r3 at matched hours** from suppression end
  (medians at h+3, h+4, h+5: 326/348/356 against 300/304/324), with the same
  pool step. Aggregates don't show why; the only rc.2→rc.3 crawler change is
  #402's lease fix.
- The gated window is 2026-11-04 12:56Z to 2026-11-05 00:56Z.
- The Windows MSI rc.3 service was installed at 04:38Z on the same public IP
  and leaves its own suppression at 10:38Z (§5).

## 4. What can be in place before the next soak

### A. Metrics only, allowed without a re-soak
`docs/decisions/2026-10-30-CR-0-acceptance.md` admits gauges that touch neither
the crawler lifecycle, the sampler nor the endpoint policy.

1. **Attribution gauges:** SQLite's `sqlite3_memory_used` and highwater
   (libsqlite3-sys ffi); mimalloc's `mi_process_info` `current_commit` (the
   `extended` feature); a self-read `RssAnon`; segment and merge counts per
   index. `anon − mimalloc commit − SQLite` isolates the musl heap. Without
   these the next verdict cannot tell candidates 1–3 apart.
2. The collector allowlist entries for them, as CR-COL-4 did for the mmap gauge.

### B. Behaviour changes: class A, so they need a frozen soak
In order of value per line changed.

1. **M2 unconditionally:** `commit_every` 2 s → 30 s (`commit_docs` is already
   5,000, so it is the only value that changes). About 15× fewer commits,
   merges, reader reloads and prunes, and the whole-index-rewrite regime
   shrinks with them. The guardrail (discovery-to-searchable p95 ≤ 10 min) is
   met by construction and its histogram already ships. This addresses
   candidates 1 and 3 together.
2. **Pool right-sizing:** `max_pending` 200,000→~20,000, and `max_peers_per_key`
   only if measurement supports it. The cap cut saves 150–175 MiB of level with
   no throughput cost (99 % of keys are evicted untried), and is a plausible
   throughput gain (fresher peers behind the 4.7 % fetch success), so it suits a
   CR-SPEC A/B. It also answers the E-v06-workers flag that a 256 MB worker
   cannot carry a 200k pool. A smaller `max_peers_per_key` needs its own
   evidence first: how often a key's later peers are the ones that yield the
   torrent (all 32 can be tried today, see §1).
3. **Route C allocations through mimalloc.** Either the crate's `override`
   feature (a static musl link hits a duplicate `posix_memalign` against
   `libc.a`; Tweag patched musl, and mimalloc_rust #146 added a `#[used]`
   reference to keep the archive member) or `sqlite3_config(SQLITE_CONFIG_MALLOC)`
   with mimalloc-backed methods before the first connection, which is contained
   and avoids the link fight. Do A.1 first so the effect is measured, not hoped.
4. Merge-policy tuning only if M2 leaves growth, as the verdict already says.
5. Upstream a tantivy change making `MmapDirectory::delete` drop its cache
   entry, and retire the local prune once it ships.

(An earlier draft listed "reuse zstd contexts in the store codec" here. The
codec already does; dropped after the runner's review of #425.)

### C. Protocol
Never run a second Lodestar on the soak's public IP while a window is open.
When it happens anyway, record it with `collect.py note --category other_crawler`.

## 5. The concurrent Windows smoke
The Windows service came up at 04:38Z, 3 h 42 m into r4's baseline, behind the
same router. Its own 6 h suppression ends at 10:38Z; from then it samples at
full rate through r4's gated hours. Two crawlers on one public IP can trip
per-IP rate limits on other nodes (libtorrent blocks an address at 5 req/s), and
any stored-rate loss lowers r4's memory slope, biasing the gate towards a pass.
CR-SPEC rule 6.5 names this an invalidator for the v0.6 runs; r4 is not under
CR-SPEC, but the plan sequenced R-win after R1′ and R2. Recommendation: stop the
service before 10:38Z and redo the smoke after the gluetun leg from a fresh
install (`REMOVE_DATA=1`; the first-run token is already spent). r4's stored rate
showed no dip while the Windows node was suppressed (hours 1–4: 692, 602, 605,
650 an hour; r3's: 636, 561, 646, 624).

## 6. Throughput, briefly
From r4 at 11 h: 32 active peer lookups → ~2.3 k lookups/h → ~14 k fetch
attempts/h → 4.7 % success → ~665 stored/h, with fetching using 8 of 400
sockets. Admission is the throttle. The levers (CR-ADM, responder hints,
streaming, uTP) are merged behind switches and belong to the A/B programme, not
to a pre-soak tweak. The pool shrink (B.2) is the one change that helps both
memory and yield. The merge regime (candidate 1) is also a disk-write and CPU
cost worth fixing on its own.

## Sources
- [tantivy `MmapDirectory` documentation](https://docs.rs/tantivy/0.14.0/tantivy/directory/struct.MmapDirectory.html)
- [Tweag: statically linking Rust with musl and mimalloc](https://blog.tweag.io/blog/2023-08-10-rust-static-link-with-mimalloc/)
- [mimalloc_rust #146: keep the override symbols linked](https://github.com/purpleprotocol/mimalloc_rust/pull/146/files)
- [libmimalloc-sys source](https://docs.rs/crate/libmimalloc-sys/latest/source/src/lib.rs)
- [BEP 51, DHT infohash indexing](https://www.bittorrent.org/beps/bep_0051.html)
