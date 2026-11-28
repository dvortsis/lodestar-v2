# X-ci-test-shard decisions (#1189)

Date: 2026-11-28 (UTC). CI only; refines no owner-set number. The runner approved the step 0 design and the rulings below on 2026-11-28.

## Why
- **The Rust test jobs set the pace of every Rust PR.** The medians of 8 green runs (2026-11-26 to 28) were 46:00 on Linux and 72:42 on Windows.
  - The `cargo test --workspace` step was 78% and 96% of those times.
  - Measured locally on 4 cores, the same as the Linux runner: compiling with warm dependencies took 971 s, and the serial test run took 1,250 s (lib unit tests 331 s, bin unit tests 88 s, integration tests 824 s, doc tests 7 s).
  - Windows runs the tests about 2.2× slower.

## Calls
- **Shards are whole cargo targets, and every shard keeps `--workspace`** (runner).
  - Each shard runs `cargo test --workspace --no-fail-fast <target filters>`. Cargo resolves features from the package selection, and target filters don't change that selection, so each shard builds the same binaries a whole run would.
  - **Checked locally:** the four shards' `--no-run` builds produce the 342 test executables of a plain `cargo test --workspace --no-run`. None is missing or duplicated, and cargo reports every one fresh, so none was rebuilt differently.
  - The same libtest harness, working directory and environment run each binary, one binary at a time within a shard, exactly as before.
- **Four shards per OS:**
  - **`lib`:** `--lib`.
  - **`bins`:** `--bins` plus the four integration tests the root-only steps run again (`privdrop`, `cli`, `worker_join`, `supervise`). It also builds the examples and runs Windows clippy and every Linux-only step.
  - **`tests-a`:** half of the other integration tests, plus `--doc` as a separate invocation, since cargo can't mix `--doc` with other filters. Doc tests need the plain libraries the integration tests build anyway.
  - **`tests-b`:** the other half.
  - **The split:** `.github/scripts/test_shards.py` reads `cargo metadata --no-deps` and splits the integration tests between the two halves, greedily by a small table of measured seconds. The table only balances the halves. A target it doesn't list gets a default weight, so a new test file lands in a shard on its own.
  - **No `-p` subsets,** which would change features. That leaves `lib` bound by the log and store unit tests on Windows; `scrub_text_is_linear` gets its own slice (runner).
- **Coverage is checked, not assumed.** `test_shards.py` fails when:
  - a target kind plain `cargo test` would build is one no shard covers (a bench or example with `test = true`, or a test with `test = false`);
  - a root-step test disappears;
  - a name has characters other than `[A-Za-z0-9_-]`;
  - a test with `required-features` might be switched on by the workspace. Plain `cargo test` skips such a test while its features are off (lodestar-bench's `mmap_retention`, which the `mmap-retention` job runs), and `--test` would refuse it.
- **Examples:** plain `cargo test` builds every example without running it. The `bins` shard runs `cargo build --workspace --examples --profile test`.
  - Checked: after a full `cargo test --workspace --no-run`, every unit that command builds is fresh.
  - `--examples` on `cargo test` would build them as test harnesses instead.
  - The step runs after the tests (`!cancelled()`), so a broken example can't hide a failing test.
- **`--no-fail-fast` is kept.** A shard with two invocations runs both and fails if either did. `fail-fast: false` keeps the other shards running when one fails.
- **The matrix is literal** (runner, from #1186's security review).
  - `os: [ubuntu-latest, windows-latest, macos-latest]` × `shard: [lib, bins, tests-a, tests-b]`. `exclude:` entries computed from `needs.changes.outputs.test_os` drop the operating systems a change doesn't need. `exclude` can only remove entries, so `runs-on` never takes a value from repository code.
  - This closes #1186's follow-up. `runner_guard.py` drops DYNAMIC_OK, `check_dynamic_chain`, the pinned chain and CHAIN_BEFORE_SHA256. Every `runs-on` is now a hosted image or `matrix.<key>` over a literal list, and `include` entries are still checked. `exclude` is the one part of a matrix that may be an expression. The guard's tests cover the old dynamic matrix failing in ci.yml too, an expression-driven `exclude` passing, and `include` still checked.
  - ci_changes.py's `test_os` selection is unchanged. macOS is sharded the same way (runner).
- **ci-ok is unchanged.**
  - It reads `needs.test.result`, the matrix's aggregate: any failed or cancelled shard makes it `failure` or `cancelled`, and ci-ok fails.
  - Excluded entries are not jobs.
  - A change with no Rust skips the whole job, which reads `skipped` exactly as before.
  - `test_test_shards.py` runs ci-ok's own script against a failed and a cancelled result.
- **The Linux-only steps run on `bins`, reusing its build** (runner).
  - "updater tests selected" and the privilege-drop probes switch from `-p lodestar` to `--workspace`. This drops a ~4 min rebuild in a different feature set. They now list and run the binary CI actually tested.
  - The selected test names are identical before and after (listed in the PR). The step still fails at 0.
  - "companion refusal as root" builds only `--test supervise` instead of the whole workspace.
- **Shared cache:** rust-cache keys on the job, not the matrix, so the four shards of an OS share one cache. Only `lib` saves it.
- **Counts:** each shard writes `passed, failed, ignored` and its number of test binaries to the job summary, so the OS totals add up across shards.
- **Cost, accepted by the runner:**
  - 8 test jobs per Rust push instead of 2, and about 1.5× the runner minutes, because each shard compiles the workspace libraries it needs.
  - The runner revisits this if queue delay p90 goes over 10 min.
- **No new tool or action.** Nextest was ruled out: one process per test would run tests that 46 in-process `static Mutex`/`LOCK`s serialise concurrently. Sharing built binaries was ruled out too, at about 37 GB of test binaries per OS. Permissions and secrets are unchanged.
- **ci_changes.py:** `test_shards.py` and its test are Rust-job inputs (RUST_ALSO). A change to the split runs the test job, whose `lib` shard runs the script's tests.
