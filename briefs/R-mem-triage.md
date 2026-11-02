# R-mem-triage: find and fix rc.2's steady anonymous-memory growth
**Model:** Opus (crawler, pipeline, store and concurrency code). A fresh Opus reviewer and `/security-review` follow if the fix touches untrusted-input paths. Runner brief, 2026-11-02. Track R, release blocker.

## Why
The Unraid soak r3 of rc.2 (`release/v0.5.1` at 6be95525, Linux Docker) is failing its memory gate on the evidence so far. The owner ran a preliminary read-only fit with `analyze_cr0.py`:
- **private_bytes, baseline hours 12 to about 16, 247 samples:** OLS slope **6.66 MiB/h, 95% CI [4.62, 8.70]**. The bar is an upper bound ≤ 2.0 MiB/h, and even the lower bound is double it.
- **The earlier average:** about +9.3 MiB/h over hours 6–15, while stored torrents rose about 685/h. That's roughly **10–14 KiB of anonymous memory per stored torrent.**
- The peak is 441 MiB at about 16 h, with 10.7k stored. There were no restarts, health stayed 200, and nothing else was wrong.

The verdict comes after 22:16Z, but a fail is now likely. If it fails, the build plan's failure path applies (Track R, "On failure at R1/R2"): a fix PR on `main`, cherry-picked to `release/v0.5.1` together with #357 as the next rc, then a fresh frozen soak. Your job is to have that fix ready and proven.

## Read first
- `docs/decisions/2026-10-30-X-soak-memory-fit.md` and `2026-10-30-CR-0-acceptance.md`. The gate measures anonymous private memory, not RSS: the memory-mapped search index doesn't count.
- `docs/build-plan.md` §0.2 Track R and §0.4 (the delta rule).
- `docs/soak/evidence/2026-10-30-cr0-final/cr0-final.md` and the CR-0 checkpoint analysis: earlier memory behaviour on Windows. r1's partial fit was 1.85 MiB/h [0.92, 2.77].
- The code at `origin/release/v0.5.1`. That's what's soaking. Diff it against `main` where it matters.

## Do
1. **List every structure that grows per hash, per stored torrent or per peer** in the crawler, pipeline, store, search and insights paths of rc.2. Look at:
   - the have-filter;
   - dedupe and seen sets;
   - lookup, sampling and obligation maps (CR-0 and CR-0d);
   - funnel and Insights per-hash or per-cohort maps;
   - metadata and file LRUs;
   - the routing table and peer caches;
   - the Tantivy writer heap and caches the search crate holds outside mmap;
   - the filter caches;
   - the logging buffers;
   - SSE and broadcast backlogs;
   - per-torrent `Arc`s kept by background tasks.

   For each one, say whether it's bounded and where.
2. **Measure, don't guess.** Build rc.2 (or a minimal harness against its crates) and drive the real ingest path with synthetic torrents at a realistic rate. There's no UDP egress in the cloud, so use the store, pipeline and writer path directly, `crawler-sim`, or a fixture mode; say which.
   - Profile the heap with `dhat`, jemalloc profiling, or heaptrack if it's available.
   - Report anonymous bytes per stored torrent over at least 20k torrents, and the top allocation sites by retained bytes.
   - Check that the harness reproduces growth of the right order (about 10 KiB per torrent). If it doesn't, say what it lacks: real DHT churn, peers, failed lookups. Then find those structures by reading the code and with targeted tests.
3. **Fix** each real unbounded or oversized structure with the smallest change: a cap, eviction, a compact representation, or freeing on completion. Make the fix on `main`, cherry-pickable to `release/v0.5.1` with no post-cut dependencies. Check by trying `git cherry-pick` onto `origin/release/v0.5.1` in a scratch branch, and report any conflicts.
4. **Prove it:** the same harness before and after, showing anonymous growth per stored torrent. Add a regression test that bounds the structure, or a bench gate if a test can't.
5. **Delta report:** for every changed line, say whether it touches the crawler lifecycle, the sampler or the endpoint policy. Under §0.4 that decides whether a re-soak is required; assume one is.
6. **Decisions file:** `docs/decisions/2026-11-02-R-mem-triage.md`.

## Own
Whatever files the fix needs in `crates/{dht,pipeline,store,search,api,lodestar,crawler-sim}`.

**Message the runner before editing any file a running slice owns:**
- SRC-14a: `crates/pipeline/src/{control,lib,log_events}.rs`, `crates/sources/src/http.rs` and `crates/lodestar/src/{metrics,main,services,ops}.rs`;
- SRC-2b: `crates/lodestar/src/percolator.rs` and `crates/store/src/saved.rs`;
- SRC-7: `crates/sources-plugins/**`.

Profiling harnesses go under `bench/` or `crates/*/examples/`, never in the shipped binary.

## Done when
- fmt, clippy `-D warnings` and `cargo test --workspace` pass;
- the before and after measurements are in the PR;
- the cherry-pick onto `release/v0.5.1` is checked.

Message the runner with interim findings as soon as you have a ranked suspect list, before the fix, so the owner hears early.

Branch `fix/R-mem-triage`. Open the PR titled "R-mem-triage: bound rc.2's per-torrent anonymous memory growth". Follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with findings and the PR number, using the `send_message` tool of the Claude Code Remote MCP server. Never merge. Push once per round. Work frugally. Don't background long jobs with `&`.
