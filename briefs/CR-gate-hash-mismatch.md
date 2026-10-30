# CR-gate-hash-mismatch: `failed_verification` counts hash mismatches only
**Model:** Opus. It touches the fetch pool, a persisted funnel code and the CR gate's exclusion counters. Queued 2026-10-30; not launched yet (usage warning). This is a follow-up from CR-gate-metrics (state.json, 10:41Z).

## Why
Today `lodestar_metadata_excluded_total{reason="failed_verification"}` counts every key the pool gave up with `FailureKind::Invalid` on its last attempt. That covers a hash mismatch, but also a protocol or handshake violation (`peer-wire/src/lib.rs` `Error::kind`).
- The gate metric (owner G2, build-plan §0) reports failed verification separately from other losses, so a protocol violation shouldn't be counted as a verification failure.

## Current path (read these first)
1. `peer-wire/src/lib.rs`: `Error::HashMismatch`, `Handshake` and `Protocol` all map to `FailureKind::Invalid`.
2. `sources/src/pool.rs`:
   - `Fetcher::fetch` returns `Result<Fetched, FailureKind>`;
   - `last_failure: Option<FailureKind>`, with `Loss::from` at about lines 795 and 1185;
   - `funnel.lost` at about line 1436;
   - the `failures` stats array is indexed by `failure_index`.
3. `sources/src/funnel.rs`: `Loss` is **persisted and append-only** (`cohort_hour.reason`). `From<FailureKind>` maps `Invalid` to `Loss::Invalid`.
4. `pipeline/src/worker.rs` (about line 69): `WorkerFunnel::lost` counts `Excluded::FailedVerification` when `loss == Loss::Invalid`.
5. Also look at:
   - `lodestar/src/metrics.rs`: the label lists for `FailureKind` and `Loss`;
   - `insights.rs`: the reason codes and labels;
   - `peer-wire/src/log_events.rs`: the failure codes and the `logs_catalog` golden;
   - the CR-SIM goldens (`failures:` and `lost:` lines);
   - the web Insights loss labels and their zh strings.

## Decide first (contract note to the runner before code)
**Option A (preferred):** carry the cause without a new persisted code.
- Add `FailureKind::HashMismatch` in peer-wire. Keep the funnel mapping to `Loss::Invalid`, so there's no new `cohort_hour` code and no rollback risk.
- The pool records the last failure's kind, and passes the gate a separate signal: a `Funnel::lost` variant, or a flag next to `Loss`. That signal makes `FailedVerification` count only on `HashMismatch`.
- The pool's `failures` array and the fetch-failure metric gain a `hash_mismatch` label. That's additive; update docs/metrics.md.
- The log failure code gets a new, append-only value. Regenerate the catalog golden with its tool; never by hand.

**Option B:** a new persisted `Loss::HashMismatch = 11`.
- An older binary (a v0.5.1 rollback) must read an unknown reason code without failing. Prove that with a test before choosing B.
- The Insights UI and the zh strings need the new label.

**Either way:**
- Protocol and handshake violations stay `Invalid` for the funnel.
- Write a decisions file naming the metric's narrower meaning. It's a runner refinement under §2.19, so flag it to the owner.

## Tests
- Unit tests in peer-wire (the mapping) and in the pool. A key's last attempt fails with a hash mismatch, and the gate counts 1. It fails with a protocol violation, and the gate counts 0 but the funnel still says `Invalid`.
- Regenerate the CR-SIM goldens with `CRAWLER_SIM_BLESS=1` and explain every changed line.
- Workspace fmt, clippy and tests, as in CLAUDE.md.

## Size
Estimated at 200–400 lines across peer-wire, sources, pipeline, lodestar metrics and docs, plus the web labels for Option B.
