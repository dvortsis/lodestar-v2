VERDICT: MERGE

# rv-727 @ebd93dd4 (X-wk-own-lease-lost)

No blocking findings. The fix is correct on every ordering I could construct, the coordinator's built-in pipeline is unchanged, and both mutations the brief names are caught. Two Important items: the race tests barely race, and the memory cost of the out set isn't stated. Both are cheap; neither blocks.

## Correctness walk-through (no defect found)
How the holder's own lease (same token T) can come back, and what each path does now:
- **Still held (in the pool):** `insert` gives `Held`, and the peers go in as a reply. The pool merges them into the entry, or, for a key it no longer has, counts a duplicate without calling the funnel (`crates/sources/src/pool.rs:1283-1294`). It can't end the lease.
- **Racing `PrepareStage`'s move out:** `send_out` removes T from held and inserts it into out under one guard (`worker.rs:158-177`). `insert` sees either held or out, never neither.
- **Out, not finished:** `Out`, so nothing happens. This is the case the chaos scenario pins.
- **Finished:** by a commit (`runtime.rs:2152-2155`), a release (`:1348`), a judge loss (`:1321`, `:1326`), a batching refusal (`:1994`), or a send failure (`worker.rs` prepare). The entry still answers `Out`, and tokens never repeat (epoch = start time). So a claim answer that the coordinator produced before it processed the commit or loss report, and that reaches the runtime after `done`, stays a no-op. That's right for both commit and loss: after a loss, the coordinator still holds T until the report lands, and re-fetching under a lease being given up would be wrong. The runtime test pins the after-commit case (M2 below).
- **`Regranted` (batch 0 and late answers):** these go through the same `granted()`. Before this PR, a regrant of a key the pool had fetched lately hit `Funnel::duplicate` too, so the fix also covers it. Per `client.rs:171-177`, "treat leases by key and token" now really holds.
- **A new token for the key (R-mem-triage):** `New`, as before, so the pool still ends a new lease for a key it fetched but didn't store. When an older unfinished out entry for the same key is overwritten, `pending` is carried over correctly (`send_out_at`'s `is_none_or(finished)`). The prune's `(token, at)` match never drops the newer entry, and the unit test pins this.
- **`pending` can't underflow:** `done` and `prune` each decrement only for an unfinished entry, and each such entry is counted once.

**The horizon limit is stated honestly, and it's no worse than before.** Past the horizon, `insert` returns `New`, exactly the old code path. In practice it only bites when the session stays up and heartbeats renew the lease while the result sits uncommitted for more than 20 min, which needs a coordinator that keeps refusing batches. With the session down for longer than the TTL, the coordinator expires the lease, and the worker's later claims get new tokens anyway.

**Concurrency.** `WorkerLeases`' mutex is a leaf: nothing else is locked inside it, and every method is synchronous. No guard lives across an `.await`: `send_out` returns before `out.send(r).await`, and `done` runs after it. `Instant::now()` is read before the lock, so `order` can be slightly non-monotonic between threads. The worst case is a prune that stops a few microseconds early, which is harmless. The prune is amortised O(1). `order` gets one entry per `send_out`, and `send_out` needs a prior `insert`, so pruning only on `insert` still bounds it.

**The built-in pipeline is unchanged.** `serve.rs:2730` still uses `WorkerLeases::default()`. With `horizon: None`, `insert` skips the out check, `send_out` is `held.remove`, and `done` finds nothing. `untracked_leases_behave_as_before` pins this.

## Blocking
None.

## Important
1. **The race tests hardly race, and they don't catch a split lock.** See `crates/pipeline/src/worker.rs:610-636`.
   - **What I measured:**
     - I logged the outcome of each round: `commit=false` gave 1000/1000 `Out` and 0 `Held`; `commit=true` gave 999 `Out` and 1 `Held`. The `Barrier` wakes the spawned thread first almost every time, so the two orderings the doc comment promises ("whichever runs first") are hardly exercised.
     - I mutated `send_out` to release the lock between `held.remove` and `out.insert` (M3). The test passed 5/5. It only failed once I added a `yield_now()` in the gap.
   - **Failure scenario:** a later refactor splits `send_out` into `take` plus a separate `track_out`, the very design the decisions file rejects. CI stays green, and the bug comes back under production load.
   - **Fix (cheap):**
     - Release both threads from a spinning `AtomicBool` instead of the `Barrier`.
     - Alternate which side spins a few iterations before calling, so both orders happen.
     - Count the `Held` and `Out` outcomes, and assert each was seen at least once.
     - To catch a split lock deterministically, add a `#[cfg(test)]` hook between the two halves, or use a loom test. If that's not worth it, at least fix the doc comment so it doesn't claim more than the test shows.
2. **The memory cost of the out set isn't stated against the worker's 256 MB RSS budget.** See `crates/pipeline/src/worker.rs:62-77` and `docs/decisions/2026-11-12-X-wk-own-lease-lost.md:40`.
   - **The estimate:** each result is kept for 1,200 s, finished or not, in a `HashMap` entry of about 56 B plus control bytes and spare capacity, and a `VecDeque` entry of 48 B. That's roughly 150 B per result: about 18 MB at 100 results/s, or 45 MB at 250/s.
   - The map never shrinks after a peak. The bound is real, but at the top of the §19.8 range it's a material share of the §19.8 worker budget.
   - **Failure scenario:** WK-G's RSS gate (§19.8, "Worker steady-state RSS ≤ 256 MB") inherits it without knowing it's there.
   - **Fix:**
     - At minimum, put the cost per result and per rate in the decisions file and in WK-G's notes.
     - Optionally, keep commit-finished entries for a short grace only; the commit and claim-answer race is in-process, with milliseconds between them. Keep the full horizon for unfinished entries and for loss-finished ones (the coordinator holds those until the report lands).
     - Or call `shrink_to_fit` after a large prune.
   - Deferring this to WK-G's notes is acceptable.

## Nits
- **`crates/lodestar/src/worker/runtime.rs:913-915`:** the horizon uses `ServerConfig::default().lease_ttl_secs` rather than the TTL the coordinator really enforces. The session client knows that TTL as `welcome.lease_ttl_secs` (`crates/cluster/src/client.rs:376`).
  - Both are 600 today: `pipeline/src/lib.rs:97` `LEASE_TTL_SECS`.
  - If the coordinator's TTL is ever raised, the horizon silently becomes shorter than a lease.
  - Fix: a comment tying the two together, or a test asserting `ServerConfig::default().lease_ttl_secs == LEASE_TTL_SECS`.
- **`docs/decisions/2026-11-12-X-wk-own-lease-lost.md:41`, the limit:** say plainly that a **worker restart** forgets every out entry whatever its age, since `WorkerLeases` lives in memory. A result replayed from the spool after a restart, with its lease still live at the coordinator, behaves as before the fix. "A spool replayed later" reads as an age limit only.
- **`docs/decisions/2026-11-11-WK-20.md:89`:** the sub-bullet still says "The fix belongs in `worker/runtime.rs` … A WK-G gate built on `sight` alone passes while this bug stands." With the new lead-in that's stale. Reword it to past tense, or point to the scenario that now pins the fix.
- **`crates/pipeline/src/stages.rs:47`:** the PR adds `Insert` to a `pub use` in `stages.rs`, a `crates/pipeline/**` file that neither the runner's approval (which named `worker.rs`) nor the PR description names. It's a necessary one-line consequence of the approved edit; name it in the decisions file or the PR description.
- **`docs/decisions/2026-11-12-X-wk-own-lease-lost.md:1`:** per `docs/decisions/README.md:8` the heading should carry the PR number: `# X-wk-own-lease-lost decisions (#727)`. This is the same nit rv-712 raised.
- **`runtime.rs` test `the_holders_own_lease_coming_back_never_ends_it`:** the pool bumps `duplicates` before it calls `funnel.duplicate` (`pool.rs:1288-1292`). So the first `lost == 0` check runs a few instructions ahead of a regression's loss. The final `lost == 0` at the end of the test covers it, so this is harmless; a comment would help the next reader.

## Verified
- **Setup:** a worktree at `/tmp/claude-0/-home-user-lodestar-v2/5bc338cd-3b83-5f81-a00f-2f3f5b6527f2/scratchpad/wt-727`, detached at `ebd93dd4e6d20a028aad6270f5197882e42968bd`. I read `CLAUDE.md`, `runner/standards.md`, the brief, rv-712, the PR body, both decisions files and the full diff (7 files, +576/−21).
- **`cargo fmt --check`:** clean.
- **`cargo +1.99.0 clippy -q -p lodestar-pipeline -p lodestar --all-targets -- -D warnings`:** exit 0. The 1.99.0 toolchain was present.
- **`cargo test -p lodestar-pipeline`:** all suites pass; the lib run is 185 passed, 2 ignored. All 8 `worker::tests` pass.
- **`cargo test -p lodestar --bin lodestar cluster::testing`:**
  - 3 serial runs: 9/9 each, 20.2 s;
  - 3 copies at once on 4 cores: 9/9 each, about 20.2 s.
- **`the_holders_own_lease_coming_back_never_ends_it`:** 3 runs by name, each passed in about 0.9 s.
- **`cargo test -p lodestar --bin lodestar worker::`:** 97 passed, 1 ignored.
- **Mutations** (each reverted, and the worktree checked clean afterwards):
  - **M1, out-tracking off** (the runtime builds `WorkerLeases::default()`): **caught.** `a_holder_sighting_its_own_keys_keeps_their_leases` fails at `scenarios.rs:334` with `left: (3, 3, 0) right: (0, 3, 0)` (the worker lost 3). `the_holders_own_lease_coming_back_never_ends_it` fails with "the lease came back: still 0".
  - **M2, `done` removes the entry instead of finishing it:** **caught.**
    - `done_finishes_only_its_own_result_and_keeps_it_known` and `a_grant_racing_the_move_out_or_the_commit_is_never_held_twice` (commit variant) fail in pipeline.
    - In lodestar, the runtime test fails with "the lease came back after its commit: still 1".
    - The chaos scenario passes, as expected: it targets the window before the commit.
  - **M3, `send_out` split across two lock acquisitions:**
    - with no gap, 5/5 runs pass: **not caught** (Important 1);
    - with a `yield_now()` in the gap, 3/3 runs fail: caught.
  - **Ordering probe:** I logged the race test's outcome per round. Without the commit, 1000/1000 rounds were `Out`; with it, 999 were `Out` and 1 `Held`.
- **No fixed sleeps:**
  - the new waits are `h.wait(…, DEADLINE)` (30 s, `testing.rs:159`) and the runtime's `until` (20 s deadline, polled every 20 ms);
  - the diff adds no `sleep`.
- **Cargo:** no `Cargo.toml` or lockfile changes, so no tokio `test-util` or `testing` feature on `lodestar`.
- **Scope:**
  - in the brief's owns: `runtime.rs`, the #712 harness (`testing.rs`, `scenarios.rs`), and both decisions files;
  - approved shared edit: `crates/pipeline/src/worker.rs`;
  - `stages.rs` has a one-line re-export (nit above);
  - `spool.rs` and `services.rs` are untouched.
- **Hygiene:** the added lines have no model identifiers, no 12-digit numbers and no local times or offsets.
- **Not run:** `cargo test --workspace`, to save disk; the PR reports it green.
