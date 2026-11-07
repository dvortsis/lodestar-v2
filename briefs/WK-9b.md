# WK-9b: Coordinator intake, part 2 (counters into rollups with the worker dimension, grant backpressure, remote leases in fetch_queue, sealed-record open)
**Model:** Opus, with `/security-review`. Worker counters and sealed records are untrusted input. A fresh Opus reviewer follows.

Runner brief, 2026-11-07T09:05Z. It runs in **the same session as WK-9** (slot 1), from the latest `main` once #568 has merged.

## Spec
- **The plan:** `docs/plan/2026-11-06-v06-workers.md`, the WK-9 row and paragraph in §4.3. WK-9b completes them.
- **WK-9's decisions file** (`docs/decisions/2026-11-07-WK-9.md`, merged with #568), especially:
  - the opening paragraph, on what is deferred to WK-9b;
  - "For WK-9b and WK-13", where grant backpressure doesn't apply to late results;
  - the "WK-9b" hand-off list: `Admit`, `BY_WORKER` rows (metric code 17), `GrantAdmission` on the writer's fill, `fetch_queue` rows for remote and waiting leases, parked keys to the built-in worker while no server runs, and the Insights budget numbers;
  - the funnel note for WK-10.
- **Architecture:** §19.3, §19.4, §19.5 and §10.9.3. Owner §1.6.4 sets the Insights budgets: a page under 50 ms, and rollups at most 50 MB a year.
- **`runner/slices.yaml`:** the `WK-9b` entry. **HARD REQUIREMENT 2 is a gate:** counters are accepted only in each metric's domain, within an hour window after offset correction, with saturating or capped accumulation, so `rollup_hour` STRICT INTEGER can't overflow and rows can't grow without bound.
- **WK-8's decisions file**, once WK-8 merges: `WireKeys`, `SealedRecord`, the per-day wire key, and the `SEALED` flag in `results.rs`.

## Do
1. **Counters.** Replace today's refusal (`accepted: false`) with `Admit`:
   - metric domain;
   - an hour window around the coordinator's clock after offset correction;
   - per-row and per-day caps, with saturation.

   Write them into the rollups with `dim = worker`, as `BY_WORKER` rows under metric code 17, the next free one; the codes are append-only. A worker can't write another worker's rows, and the dimension comes from the authenticated session.
2. **Re-check the Insights budgets** with the worker dimension at the plan's fleet size, and report the numbers: page time, and rollup bytes per year.
3. **Grant backpressure:** set WK-0's `GrantAdmission` hook from the writer's fill, so grants slow above the high-water mark. Late results aren't subject to it (WK-9's note). Add a test that the writer queue stays bounded under a flood of grants.
4. **`fetch_queue`:** write remote and waiting leases alongside the local ones, including WK-1's "leases after off" case. Keys parked while no server runs go back to the built-in worker. Then a late result for a remote key from before a restart finds its record.
5. **Sealed records** (after WK-8 merges): `filter_snapshot::WireKeys::open` before the rebuild, then the usual rebuild and re-judge.
   - A record that fails to open is refused and charged like a failed decode.
   - Never trust the worker's claim about whether a record is sealed: the flag must agree with what opens.
   - Until WK-8 merges, WK-6's codec refuses the unknown `SEALED` bit, which fails closed. Build the other parts first.
6. **Three carry-overs from #568's last review** (each with a test):
   1. `over_budget` (`remote.rs:175-177`, `:603-609`) also counts the two budget refusals that are counted only as `refused` today:
      - shape's count refusal when the components left run out;
      - `decompress_capped`'s "over its cap" once `budget.bytes` is spent.
   2. Delete the stale "no more than its own byte cap" clause near `remote.rs:588-590`.
   3. Make the removal of a late result's `prior` entry the check itself: take the entry with `remove`, and treat `None` as stale. Then "rebuilt at most once" doesn't depend on `feed` taking batches one at a time.
7. **The decisions file:** `docs/decisions/2026-11-07-WK-9b.md`, with notes for WK-10 (the funnel's `PeerFound` and `Fetched` for remote keys), WK-13 and WK-15. Name every §2.19 refinement.

## Owns
- **Yours:** `crates/pipeline/src/remote.rs`, `crates/pipeline/src/rollups.rs`, `crates/pipeline/tests/remote_intake.rs` and `crates/lodestar/src/cluster/intake.rs`.
- **Ask the runner first, naming the functions:**
  - `fetch_queue` lives in `crates/pipeline/src/writer.rs` and `crates/lodestar/src/insights.rs`;
  - `GrantAdmission` is in `crates/cluster/src/server.rs`.
- **Running alongside:**
  - **WK-14** edits `crates/cluster/src/server.rs` (pause), `crates/lodestar/src/services.rs` and `crates/lodestar/src/cluster/mod.rs`;
  - **WK-8** edits `crates/lodestar/src/cluster/filter.rs` and `crates/pipeline/src/wire/**`.
  - Don't change their functions. Whichever PR merges later brings `main` in.
- **Metrics:** added rows only, in `crates/lodestar/src/metrics.rs` and `docs/metrics.md`. Name them in the PR.

## Done when
- §4.3's remaining gates pass: nothing is counted twice; backpressure bounds the writer queue; the funnel reconciles with remote leases in `fetch_queue`.
- HARD REQUIREMENT 2 has tests for out-of-domain keys, a window miss, overflow and saturation, and a worker writing another worker's rows. Each fails without its check.
- The sealed-record path is tested for:
  - a valid record;
  - a wrong key;
  - a tampered record;
  - the `SEALED` flag set on a plain record, and missing on a sealed one.
- Single-machine parity: with no worker joined, nothing changes.
- No fixed sleeps: wait on conditions with deadlines of at least 10 s. No tokio `test-util` or `testing` features on the `lodestar` package.
- Checks: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and a short `remote_blob` fuzz run.
- Size: about 1,000 lines, tests included.
- Process:
  - follow `.claude/skills/steward/SKILL.md`, push once per round, and never merge;
  - title the PR `WK-9b: Coordinator intake, part 2`, on branch `slice/WK-9b`;
  - message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA once CI is green.
