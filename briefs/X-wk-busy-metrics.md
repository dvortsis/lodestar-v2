# X-wk-busy-metrics, then X-lease-report-atomic (Opus): two small slices, one after the other, in X-wk-busy-retry's session

Assigned by the runner, session_01EYhgh4JX3AmHzKXPjYCoUg, on 2026-11-08 at about 23:15Z. Follow `.claude/skills/steward/SKILL.md`. One PR each. Open the second only after the first merges, because both touch `crates/cluster/src/server.rs`.

## 1. X-wk-busy-metrics (from #637's review)
Branch `slice/X-wk-busy-metrics` from main e964b487 or later. PR title `X-wk-busy-metrics: <title>`.
- **Export the worker's Busy-retry counters** (`busy_retried`, `busy_gave_up`, `busy_dropped`, as #637 named them) as Prometheus metrics in `crates/lodestar/src/metrics.rs`.
  - Follow the existing worker metric names and the HELP and TYPE conventions.
  - Counters, not gauges.
  - No per-key or per-infohash labels.
- Add the matching rows in `docs/metrics.md` (a contract doc: keep its table format and units).
- Fix the stale `KeyCheck` doc line in `crates/cluster/src/server.rs`: the worker retries Busy now (#637). One comment line.
- Tests: a metrics render test showing the three names with their values, plus whatever the metrics golden or contract test needs.
- **Owns:** `crates/lodestar/src/metrics.rs`, `docs/metrics.md`, that one doc line in `crates/cluster/src/server.rs`, and the counters' accessors in `crates/lodestar/src/worker/runtime.rs` if they need exposing.

## 2. X-lease-report-atomic (concurrency; a fresh Opus review)
Branch `slice/X-lease-report-atomic` from main, after part 1 merges. PR title `X-lease-report-atomic: <title>`.
- **The bug:** WK-10d (#607) review item 7. `LeaseAuthority::report` in `crates/cluster/src/leases.rs` doesn't refuse a reported key to every claim at once. A claim racing the report can still win the key. The residual window is described in `docs/decisions/` for WK-10d; read that first.
- **The fix:** make the report and the refusal atomic under the authority's lock, or under whatever ordering the leases module already uses. After `report` returns, no claim, whether in flight or new, gets that key.
- **Owns:** `crates/cluster/src/leases.rs`, plus one line at the sink's call site in `crates/cluster/src/server.rs` if needed.
- **Tests:**
  - a deterministic concurrency test, for example with barriers or a loom-style interleaving if the crate already uses one; no fixed sleeps, condition waits with deadlines of 10 s or more;
  - plus a test showing a reported key is refused to a claim that was already waiting.
- Update WK-10d's decisions text only by adding a new decisions file for this slice that says the window is closed. Don't edit the old file.

## Rules
- No fixed sleeps. No skipped tests.
- Never add tokio `test-util` or a `testing` feature to the `lodestar` package (CI's guard refuses it).
- No model identifiers.
