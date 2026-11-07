# WK-9: Coordinator intake (results under fencing, late results, outcomes, counters with a worker dimension, peer hints, backpressure, remote leases in fetch_queue)
**Model:** Opus, with `/security-review`. This slice is where untrusted worker output enters the store, so content safety and exactly-once both rest on it. A fresh Opus reviewer follows. Runner brief, 2026-11-07T04:35Z. It takes the slot X-soak-screen freed (#562 merged), within owner §2.42's four slots.

## Spec
- **The plan:** `docs/plan/2026-11-06-v06-workers.md`. Read:
  - the WK-9 row and paragraph in §4.3, **which is your spec and gates**;
  - §4.2's shared files: one `writer.rs` entry point, the `pipeline/src/lib.rs` mod line, and `docs/metrics.md` rows;
  - **Q7**, the late-result rule. It is a runner call, flagged under §2.19, and the plan row is the decision.
- **Architecture:** §19.3, §19.4, §19.5, §18.7 and §10.9.3. Owner §1.6.4 sets the Insights budgets: a page under 50 ms, and rollups at most 50 MB a year.
- **`runner/slices.yaml`:** the `WK-9` entry. **Its notes list seven HARD REQUIREMENTS from the WK-0 (#538) and WK-6 (#539) reviews. Each one is a gate**, with a test.
- **The decisions files of the merged slices**, with their notes for WK-9:
  - `docs/decisions/2026-11-06-WK-0.md`: "For WK-9 and WK-10", the holder-checked `LeaseAuthority::commit`, the `GrantAdmission` hook, hints, and `pending` vs `fetch_queue`;
  - `docs/decisions/2026-11-06-WK-1.md`: leases after "off";
  - `docs/decisions/2026-11-06-WK-6.md`: the intake requirements. The coordinator stores its own rebuilt enrichment, never the worker's.
  - `docs/decisions/2026-11-07-WK-5.md`;
  - WK-7's decisions (#549).
- **Running alongside:**
  - **WK-3** (have-filter replication) owns `crates/pipeline/src/have.rs` and `crates/lodestar/src/cluster/have.rs`. Don't touch them.
  - **X-fetch-meta-reserve** owns `crates/lodestar/src/metrics.rs` and `docs/metrics.md`. Whichever of you merges later brings `main` in. Keep your `metrics.rs` and `metrics.md` edits to added rows, and name them in the PR.

## Do
Follow §4.3:
1. **Decode `Results`** with WK-6's codec, then fence every record against the lease table. A token that isn't current for that worker is dropped and counted as stale.
2. **Hand each surviving record to the writer's commit,** which re-judges it (§1). It does so through one new `writer.rs` entry point, the shared edit; nothing else in `writer.rs`. Then the seven requirements:
   1. **Content safety.** For every remote record, store the enrichment the coordinator rebuilds (`enrich_infos`), never the worker's bytes. Refusing a record instead is allowed only for this build's `ENRICHMENT_VERSION`, comparing the decoded record, because search reads `Enriched::data`. A record of another version is never kept as sent.
   2. **Counters.** Accept only keys in each metric's domain, with an hour window around the coordinator's clock after offset correction. Accumulate with saturation or a cap, so `rollup_hour` STRICT INTEGER can't overflow and rows can't grow without bound.
   3. **Fields from the lease, not the worker.** `sources` and `private_source` come from the lease. `metadata_at` is offset-corrected. `size` and `file_count` are checked against the rebuilt file list.
   4. **Remote commits** go through `LeaseAuthority::commit` (holder-checked).
   5. **Peer hints.** A claim denied as held elsewhere relays its hints to the holder in memory, filtered with `lodestar_network_policy::allows`. Hints are never stored.
   6. **`RECENT_COMMITS`:** a re-commit keeps its old eviction slot (key → seq map).
   7. **The worker's held set:** always call `committed`, or prune past the TTL.
3. **Outcomes end leases.**
4. **Counters go into rollups with `dim = worker`.** Re-check the Insights budgets with the worker dimension, and report the numbers.
5. **Backpressure.** Grants slow when the writer's queue passes its high-water mark, through WK-0's `GrantAdmission` hook.
6. **Remote and waiting leases** are written to `fetch_queue` alongside the local ones. Cover WK-1's "leases after off" case.
7. **Late results (Q7, as its plan row gives it):**
   - A result from an earlier epoch is committed only if its key is neither stored nor held by any worker now, through the same re-judge. It never counts "harvested" again.
   - Its cohort, source bits and strategy come from the coordinator's own `fetch_queue` record.
   - If the key's terminal outcome was already recorded, count a `late_result` correction beside it, so the funnel still reconciles.
   - **Refused outright:** any late result from a quarantined, revoked or purged worker, and any result older than that worker's last purge.
   - WK-13 adds quarantine and purge. Build the refusal against a small trait or a store read that WK-13 fills, and test it with a fake.
8. **Metrics:** add rows for stale, late, refused and backpressure, in `metrics.rs` and `docs/metrics.md`.
9. **The decisions file:** `docs/decisions/2026-11-07-WK-9.md`, with notes for WK-10, WK-11, WK-12, WK-13 and WK-15. Name every §2.19 refinement.

## Done when
- **§4.3's gates pass:**
  - late results from a quarantined, revoked or purged worker, or older than its last purge, are refused;
  - current-epoch stale-token results and outcomes are ignored;
  - a late result never counts "harvested" again, and the funnel reconciles;
  - nothing is counted twice;
  - backpressure bounds the writer queue;
  - hints never reach the store.
- **Each of the seven HARD REQUIREMENTS has a test that fails without it.** Name the tests in the PR. Requirement 1 includes a test where a worker sends adult-classified content labelled as non-adult, and the stored record and search terms follow the coordinator's own verdict.
- **Single-machine parity (§19.8):** with no worker joined, nothing changes. Run the existing pipeline tests and the parity bench.
- **No fixed sleeps:** wait on conditions with deadlines of at least 10 s. No tokio `test-util` or `testing` features on the `lodestar` package.
- **Checks:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`.
- **Size and owns:** about 800 lines (the runner splits anything over about 3k). Ask the runner before editing outside your owns and the shared one-line edits named above. If the split looks better as two PRs (intake plus fencing, then counters, backpressure and `fetch_queue`), say so early.
- **Process:** follow `.claude/skills/steward/SKILL.md`, push once per round, and never merge. Title the PR `WK-9: Coordinator intake`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA once CI is green.
