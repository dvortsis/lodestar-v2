# G-m5a: M5 exit, part 1: the analytics-validation matrix and the reporting-overhead benchmark
**Model:** Opus (numeric correctness across store, search and rollups; benchmarks). An Opus reviewer follows. Runner brief, 2026-11-01. It's the first half of the M5 exit gate (`runner/slices.yaml` → `G-m5`). Every dependency of G-m5 is merged. Part 2 (G-m5b: the `design-review` pass) runs separately.

## Why
`docs/architecture.md` §14 M5 row and build-plan P2 "M5 exit": the milestone exits only when:
- the `analytics-validation` matrix (§10.17, incl. Insights reconciliation) passes on the fixture set;
- reporting overhead is within the §3 budget: with Insights open and the digest job running, search p95 +≤ 5%, ingest throughput −≤ 2%, rollup storage ≤ 50 MB/year.

Many of these checks probably exist already in crate tests. Nobody has mapped §10.17 item by item to them.

## Read first
- `docs/architecture.md` §10.17 (the matrix, every bullet), §3 (the overhead row), §7 (rollups, cohorts, `index_watermark_hour`), §10.9 (Insights); `docs/metrics.md` (metric contracts).
- `bench/` (`fixture.rs`, `synth.rs`, `src/bin/*`), the CI jobs that run benches (`.github/workflows/ci.yml`: "search latency (§3)", "size budget (§3)", "filter budget (§20.8)"), and how they gate.
- Existing tests: `crates/store/tests/**`, `crates/search/tests/**` (facets, histograms), `crates/api/tests/insights.rs`, the rollup/repair tests, and `crates/lodestar/tests/**`.

## Do
1. **Audit, then fill.** In your decisions file, make a table: each §10.17 analytics bullet → the test(s) that prove it on a fixed fixture, or "gap".
   - Result count equals the disjunctive-facet expectations, computed independently from SQL and from Tantivy.
   - Histogram bins reconcile to source counts.
   - Every Insights reconciliation bullet: stored and searchable rollups vs `COUNT(*)`; searchable = the Tantivy doc count at the last commit; funnels never increase; harvested = Σ terminal + in-progress; compaction loses nothing; Discover/Fetch counters vs `/metrics` within one flush; `rollup --repair` after a simulated crash; drill-through counts match the clicked mark (the API side; the web side may be an e2e that already exists, so cite it).
   - Filter, brush and reset sequences restore the baseline (cite the web specs that cover it).

   Fill each gap with a deterministic test on a fixed fixture. Prefer one `analytics_validation` integration test file that builds the fixture once over scattering new tests. "Independently" means the expected numbers come from plain SQL or a hand count, never from the code under test.
2. **The reporting-overhead benchmark.** Extend `bench/` (follow `search latency (§3)`):
   - search p95 on the bench corpus with and without an Insights load (overview + a stage page polled at the UI's rate) plus a digest run in parallel: the delta ≤ +5%;
   - ingest throughput with and without that load: ≤ −2%;
   - rollup storage per year, projected from the measured bytes per hour/day row at the fixture's rate: ≤ 50 MB/year.

   Report median and spread over enough runs to be stable on CI. If the deltas sit inside run-to-run noise, say so and set the gate with a stated noise allowance (runner decides, §2.26; flag it). Add it as a CI job only if it fits in about 5 minutes; otherwise as a documented local gate plus a nightly job.
3. **If any check fails,** stop and report to the runner with the numbers. Don't loosen a check or a budget. A failing reconciliation is a bug to fix in its own slice.
4. **Decisions file:** `docs/decisions/<date>-G-m5a.md` with the audit table, the bench numbers and how each gate runs.

## Own
New tests (`crates/*/tests/analytics_validation*.rs` or the closest existing test module per bullet), `bench/**` additions, a CI job entry in `.github/workflows/ci.yml` (only that job), your decisions file.
**Don't change** production code, except a test-only hook if a check can't otherwise see a number; say so in the PR.

## Size
≲ 1,200 changed lines. If it grows past that, split the matrix and the bench into two PRs.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and the new bench passing locally twice with similar numbers.

Open the PR (add the `full-ci` label if you touch `ci.yml`), follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head, the audit summary (covered / filled / failing) and the bench table. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes. Don't background long jobs with `&`.
