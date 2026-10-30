# CR-0 work during the baseline wait

The owner authorized the previously suggested offline analyzer, lifecycle
regression coverage and Windows lint cleanup on 2026-10-26. This work is based
on `43d054df` in branch `work/cr0-soak-tools`, in a separate checkout with its
own Cargo target directory. It does not implement CR-1 or change crawler behavior.

## Delivered scope

- [Offline analyzer and usage](tools/README.md): Python standard library, aggregate
  JSONL/Prometheus evidence, Markdown or JSON output, separate phases, per-counter
  coverage, discontinuity detection, capacity observations and descriptive RSS
  changes. No automatic acceptance or speedup claims.
- Five regression tests: stale replies after same-hash cancellation/retry;
  completion and deadline cleanup with either IP family first; abandoned admission
  and queued pause; repeated sampler reuse with stale completions; shared attempts
  and timeout cancellation across multiple peer finders. Time is advanced directly
  or controlled by Tokio's paused clock; no fixed wait assumes completion.
- Windows lint fixes: Linux-only cluster connection-test helpers and imports;
  Unix-only builder mutability; update-policy and Linux memory-parser helpers
  compiled only where used, including their existing tests. Unix permissions and
  update policy are unchanged.

## Soak isolation

The running native CR-0 executable, copied collector, configuration, database and
evidence remain untouched. The analyzer only reads aggregate files. Rust builds
and tests share the Windows host and run with two Cargo jobs during startup
suppression, before the measured baseline. These validation intervals should
not be interpreted as an idle-host startup memory benchmark.

The original Docker crawler remains stopped. Its existing restart restriction
still applies: at least six hours plus four seconds after the actual native
crawler stop. This work does not schedule or perform that restart.

## Validation

- `python -m unittest discover -s docs/soak/tools -p test_analyze_cr0.py`:
  **16 passed**. Covers phase separation, gaps, restarts, per-series counter
  resets, missing counter coverage, partial writes, corrupt snapshots, failed
  health with valid metrics, capacity equality versus overflow, short/full
  baseline windows and output protections.
- Live aggregate evidence cross-check: 52 startup samples, no health/metrics
  failures or collection issues. Admission rate and peak RSS independently
  recalculated from JSONL and matched the analyzer. Baseline correctly reported
  `not_started`. Reports remain local under the worktree's ignored `target/`.
- `cargo test --workspace --offline -j 2 --quiet --no-fail-fast`:
  **2,110 passed, zero failed, 16 ignored**, including doctests; exit 0.
  The first workspace process ended during compilation without a result; the
  resumed run completed. A prior focused run caught a new test's incorrect
  assumption about persistence snapshots; the test now checks the charged
  attempt after completion, respecting the existing in-progress exclusion.
- `cargo clippy --workspace --all-targets --offline -j 2 -- -D warnings`:
  **passed** on Windows. The existing build-script notice about missing
  `web/build` and an embedded stub page remains; it is not a Rust lint failure.
- `cargo fmt --all --check` and `git diff --check`: **passed**.

Linux execution, packaged UI, fuzzing, scale gates and soak acceptance remain
separate gates. No push, merge or deployment is part of this work.

AgentMemory action `act_mvpcjn7r_b4c83155914c` was updated during implementation.
Its MCP connection became unavailable before completion; this file records the
completed outcome for later synchronization.
