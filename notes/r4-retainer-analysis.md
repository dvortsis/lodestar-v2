# r4 retainer analysis (runner, 2026-11-04 15:10Z, static, rc.3 bd9300dd)
- **Every Lodestar structure is bounded.** The bounded set (log ring, SQLite caches, PeerStore, hot columns, pool queues, leases) plateaus at about 25 MiB.
- **Suspects, ranked:**
  1. **tantivy's default LogMergePolicy on both writers.** The commit size is about 1 doc, so the whole corpus is re-merged about every 8 commits, and the allocator keeps the high-water. This fits steps.
  2. **Per-commit thread churn** (index worker plus docstore compressor) on mimalloc v3, ~0.6–1.5 MiB/h. M1-exp B musl anon was 936 B/commit while dhat stayed flat.
  3. **The real-network fetch path**, never exercised by the harnesses.
  4. **THP refill** (mimalloc `allow_thp`=1).
  5. **The title-set rebuild every 60 s** (small).
- **The fix sketch if r4 fails:**
  - an explicit merge policy per index (lower `min_layer_size`, cap the files index's `max_docs_before_merge`);
  - plus M2's commit cadence (≥ 30 s or 5,000 docs, a UX change to flag under §2.19);
  - validate with mem-ingest at 1 doc/commit over ≥ 20k torrents;
  - a diagnostic A/B of mimalloc v2 against v3, and `MIMALLOC_ALLOW_THP=0`.
- **Doc gap:** `search_commits_total`, `leases_outstanding`, `fetch_active`, `fetch_lookup_backlog` and `dht_routing_nodes` are missing from `docs/metrics.md`.
