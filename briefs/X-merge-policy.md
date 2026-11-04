# X-merge-policy: bound tantivy's merge high-water (r4 contingency, memory fix 2)
**Model:** Opus. Core: search, and memory. The runner reviews with a fresh Opus reviewer. Runner brief, 2026-11-04; auto-approved under owner §2.33. This is Track R priority.

## Why
Soak r4 (rc.3, with #406's mmap prune) is in its deciding window, hours 12–24. Its provisional fit over hours 12–14 is 23 MiB/h, 95% CI [15.6, 30.6]. That's a short, noisy window, but r4 isn't below r3 at the same hours, so the prune alone may not get under the ≤ 2.0 MiB/h bar. The runner's static analysis is on runner-state at `notes/r4-retainer-analysis.md`. It finds every Lodestar structure bounded, and puts **tantivy's default `LogMergePolicy`** first: commits are about 1 document, so the whole corpus is re-merged about every 8 commits, and the allocator keeps the high-water. R-mem-triage measured 1,276 against 678 B/torrent with NoMergePolicy (`docs/decisions/2026-11-02-R-mem-triage.md`).

This slice is ready either way. If r4 fails, it goes into rc.4. If r4 passes, it lands on `main` for v0.5.2.

## Read first
- `docs/decisions/2026-11-02-R-mem-triage.md` and `docs/decisions/2026-11-03-X-mmap-cache-prune.md`: the method, the harnesses and the decision-sequence rules.
- runner-state `briefs/X-mmap-cache-prune-decision-sequence.md` (the A/B estimator rules) and `notes/r4-retainer-analysis.md`.
- `bench/examples/mem-ingest.rs`, `mem-churn.rs`, `mmap-drip.rs` and `bench/tests/mmap_retention.rs`.
- `crates/search/src/index.rs` (both writers' setup, about lines 954–971 and 1451; no merge policy set), `live.rs` (`LiveConfig`), and tantivy 0.27's `log_merge_policy.rs`.

## Phase 1: measure first (no production code change yet)
SB-6 (#415) owns `crates/search/src/index.rs` and is in review. Don't edit `index.rs` until the runner says SB-6 has merged.

1. **Extend the harness** (new example or flags in `bench/`): a merge-policy option on the writer, applied through a test hook. If no hook exists, construct the writers in the harness.
2. **Arms, run as separate processes:**
   - **A:** the default policy;
   - **B:** a tuned `LogMergePolicy`, with `min_layer_size` of a few hundred documents per index and a capped `max_docs_before_merge` on the files index;
   - **C:** B plus M2's commit cadence (≥ 30 s or 5,000 docs);
   - **D, diagnostic only:** A on mimalloc `v2`, and A with `MIMALLOC_ALLOW_THP=0`.
3. **Workload:** 1 document per commit, ≥ 20k torrents, merges on, the real schema.
4. **Measurements:**
   - musl anon MiB per 1k torrents and per hour at the production pace;
   - dhat peak bytes per merge;
   - segment count over time;
   - search p50 and p95 during ingest;
   - discovery-to-searchable p95 for C.
5. **The estimator:** the same OLS method as analyze_cr0's `memory_fit`. Fix the pass rule in advance, the same way the decision sequence did: B's upper bound below A's point estimate, in every run pair.
6. **Report the numbers** to the runner before building the production change.

## Phase 2: the change, after SB-6 merges and on the runner's go
- Set the explicit policy (and C's cadence, if it wins) in `index.rs` and `live.rs`.
- Keep #406's prune intact and `mmap_retention` green.
- Add a retention regression test for the merge high-water in the style of `mmap_retention`: release mode, its own binary, no fixed sleeps.
- Decisions file: `docs/decisions/2026-11-04-X-merge-policy.md`.
- Flag C's cadence change to the owner (§2.19, a UX change): discovery-to-searchable p95 must stay ≤ 10 min.

## Own
`bench/examples/mem-merge.rs` (new), `bench/tests/merge_retention.rs` (new), and your decisions file. After the runner's go: the merge-policy lines in `crates/search/src/index.rs` and `LiveConfig` in `live.rs`.

## Done when
- Phase 1's report is sent.
- Phase 2: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `cargo test --release -p lodestar-bench --test mmap_retention -- --test-threads=1` plus the new retention test.
- A fresh self-review is done.

Branch `slice/X-merge-policy`. The PR is titled "X-merge-policy: Bound tantivy's merge high-water". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with `send_message`. Never merge. Push once per round. Don't background long jobs with `&`; run them in the foreground with a timeout, or via run_in_background and wait for the notification.
