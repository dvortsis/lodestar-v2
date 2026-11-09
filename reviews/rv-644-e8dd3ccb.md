VERDICT: MERGE

# Delta review of PR #644 (PK-win-upd-3), round 5: 93a26643 to e8dd3ccb, correctness and security

**Scope:** one commit, no merge from `main`. It touches `windows/service.rs` (production and tests), the Fake and one test in `update/windows/helper.rs` (test module only), and the decisions file. `rollback.rs` is unchanged. Line numbers are at e8dd3ccb.

**Result:** R4-1 is fixed by the runner's fix (a). R4-2 and R4-3 are done. No blocking findings. Three non-blocking notes follow.

## Checks

| Item | Status | Evidence |
|---|---|---|
| **R4-1:** the continue path | **Fixed** | `service.rs:691-763`. The idle arm maps `Idle` through `exit_code()` (`:140`). `Stopped` gives 0. `MarkerGone` gives `continue` (`:717`), which runs `load()` and `rollback_refusal` again. |
| **SCM status** | **OK** | `register` and the one Running report (`:670-685`) happen before the loop. There's no second register and no second Running report, and Stopped is reported once, after the loop. |
| **A stop during the second load or start** | **Honoured** | The watch keeps `true`. `idle_during_rollback` checks `stopped()` before the marker, so a stop wins when both are true at the same look. A stop that lands after `MarkerGone` reaches `serve`, where `stopped(&mut early)` uses `borrow_and_update` and resolves at once, so the start ends Stopped with 0. That's the same path as a stop during a normal start. |
| **A refusal on the second load** | **Exit 3** | The `Err` arm breaks with `REFUSED`, as before. If the marker is back, the service idles again. |
| **Logging** | **Initialised once** | `init_logging` runs only in the serve arm, which always breaks. The idle arm writes only `log_refusal` (the temp log). `startup` and `Restart::global().set_service()` also run once. |
| **Moving `stop_rx` inside the loop** | **OK** | The move into `serve` is followed by `break`. It compiles: `test (windows-latest)` is green. |
| **Leaks from the idle phase** | **None** | `idle_during_rollback` is synchronous on the service thread. It spawns no thread and holds no handle on the marker, and it returns before the next load. |
| **Step 5's order** | **Unchanged** | `rollback.rs:717-743` isn't in the diff. After the marker goes, the helper touches only staging (`rotate_back`, `save`) and the service, so nothing races the service opening the store. |
| **The 3010 path** | **OK** | The waiting pass's `stopped()` still ends an idle process with exit 0. After the boot, the process that idles is `from`, so carrying on serves the right version. |
| **Drop path** (`rollback.rs:411-416`) | **OK** | A version installed by hand while the marker existed was started by its own MSI and idles. `remove_marker` lets it carry on in the same process, so it ends up running. See R5-2 for the missing test. |
| **R4-2** | **Done** | `finishes()` (`service.rs:1461-1467`) waits up to 10 s, then asserts `is_finished()` before the join. Both idle tests use it. |
| **R4-3** | **Done** | The `exit_code()` mapping is a pure function, tested on every OS. The decisions file adds the G-m6 line "Running, `/healthz` 200". |
| **Fixed sleeps** | **None** | The 5 ms in the idle tests is the idle wait's own poll interval, not a sleep the test waits on. Every wait is a `recv_timeout` against a deadline of at least 10 s. |

## Mutations (each reverted; `git status` was clean afterwards)

Output is in `/tmp/claude-0/-home-user-lodestar-v2/5bc338cd-3b83-5f81-a00f-2f3f5b6527f2/scratchpad/mut644r5/`.

| Mutation | Result |
|---|---|
| **M1:** `Idle::MarkerGone => Some(0)`, so the service exits again once the marker goes | **Caught**, but only by `the_idle_service_ends_once_the_marker_goes` (`service.rs:1497`) through its `exit_code() == None` assert. The new rollback test passes. See R5-1. |
| **M2:** the drop path no longer removes the marker | **Survives.** All 111 tests pass. See R5-2. |
| **M3:** step 5 stops the service after its start | **Caught** by 3 tests, the new one among them. |

## Findings (none blocking)

**R5-1. The new rollback test doesn't model the R4-1 failure, and would have passed on 93a26643** (`helper.rs:2048-2065`, the Fake at `:1237-1242`, `:1290-1296` and `:1313-1329`).
- **Why it can't fail on the old code:** production `rollback.rs` and `helper.rs` are byte-identical between the two commits. The Fake also hard-codes the new behaviour:
  - `start_service` always sets `running = true`;
  - removing the marker never stops it.
- **What it does check:** that step 5 doesn't leave the service stopped (M3).
- **What catches R4-1:** only the mapping assert in the service unit test. That's acceptable, because `exit_code()` is the single decision point and the SCM loop is `cfg(windows)`.
- **Suggested fix:** tie the two together.
  - Have the Fake watch the marker: when it goes while `running`, set `running = Idle::MarkerGone.exit_code().is_none()`.
  - Make `start_service` on a running service a no-op.
  - M1 would then also fail the rollback test.

**R5-2. The drop path's `remove_marker` (`rollback.rs:414`) has no test.**
- **Evidence:** M2 survives. `a_manual_install_drops_the_journal` (`helper.rs:2171`) has no rollback marker in place.
- **Origin:** the gap predates this delta, but the fix makes this removal the step that lets a hand-installed service serve.
- **Suggested fix:** plant the marker and assert that it's gone after the run.

**R5-3. Nit: a reflowed doc line in `service.rs:152-153` runs past the comment width.** It reads "…(exit\n/// 0) or the marker goes (on into the normal start). Exiting at once instead would fail an MSI's". It's cosmetic only, and rustfmt doesn't wrap comments.

## Runs
- **`cargo fmt --check`:** passes.
- **`cargo clippy -p lodestar --all-targets -- -D warnings`:** clean. The only warning is the toolchain's `unknown_lints` for `clippy::chunks_exact_to_as_chunks`, which is environment noise and ignores `-D warnings`.
- **`cargo test -p lodestar --bin lodestar -- windows::service update::windows`:** 111 passed.
- **CI on e8dd3ccb:** every check that ran passed, including `ci-ok`, `test (windows-latest)`, `test (ubuntu-latest)`, `fmt + clippy`, `smoke (linux/amd64)`, `web-ok` and `e2e-ok`. The rest were skipped by path filters.
- **Merge state:** `git merge-tree --write-tree origin/main e8dd3ccb` (main at 161c2a1f) is clean.
