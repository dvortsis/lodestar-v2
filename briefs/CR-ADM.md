# CR-ADM: bounded admission, wakeup and backpressure (DUR-02–04), behind its own switch
**Model:** Opus (concurrency, crawler core). Runner brief, 2026-10-30. Master plan Part A2, candidate H2.

## Base and timing
- **Starts after CR-0f and CR-SIM are on `main`.** CR-0f owns the pool's pause path and bootstrap; CR-SIM owns the replay harness and the actor seam. The runner launches this slice then. Branch `slice/CR-ADM` from `origin/main`; never an older clone.
- It merges to `main` behind a runtime switch that is **off by default**. It is not in v0.5.1 unless the release delta review says so.

## Contract (read all of these first)
- `docs/plan/2026-10-27-crawler-experiment-protocol.md` ("Admission accounting model", "Candidate backpressure contract", "Independent experiment records"): the contract.
- `docs/plan/2026-10-27-baseline-execution.md` (DUR-02, DUR-03, DUR-04, DUR-08, DUR-09).
- `docs/decisions/2026-10-30-master-plan.md` (Part A: one switch per candidate, measured alone, CR-SIM screen first).
- CR-0 evidence (#189): 95.5M capacity rejections against 111k admissions over 24 h; 98.86% of evictions before an accepted lookup; `dht_active` pinned near 30 of 32 (CR-SIM's H1 baseline).

## Phase 1: design, then the runner's OK (no code before it)
Write `docs/decisions/<date>-CR-ADM.md` specifying, from the current pool/finder/actor interfaces:
1. **The permission unit:** a logical request or an address-family leg. Show why, with both families accepted.
2. **Bounds:** the maximum number of waiters, the wakeup and coalescing rule, and bounded interactive bursts plus a fair background queue. New arrivals get no indefinite advantage over retained waiters.
3. **Who returns permission at every terminal boundary:** timeout, completion, fetch success, eviction, cancellation (waiting or admitted), pause/resume, a closed finder, shutdown and drain. No leaked or duplicate credit. A late acknowledgment from an old generation can't attach to a new entry with the same hash.
4. **Waiting is not an attempt:** cancellation while waiting uses no network attempt and removes its waiter. Retries stay distinguishable from arrivals. No spin loop, no permanent back-off after recovery, no permit kept after eviction.
5. **The pause-order race reported by CR-0f:** `crates/pipeline/src/control.rs` pauses the pool before the DHT and resumes in the same order over separate channels, so one key can wait a refusal back-off. Resolve it inside this design.
6. **Metrics:** new counters for waits, wakeups, grants, refusals by reason and permit returns by boundary, kept apart from the existing counters, which keep their meaning. Specify the `docs/metrics.md` text.
7. **The switch:** its name, default (off), the config/settings surface (no UI in this slice), and the exact behaviour when off, which is today's, byte for byte in the CR-SIM goldens.

Message the runner a short summary and wait for the OK.

## Phase 2: code and tests
- Implement behind the switch. With it off, CR-SIM's goldens are unchanged.
- **Fixtures (DUR-04):** saturation, recovery after saturation, pause/resume while waiting and while admitted, cancel while waiting and while admitted, drain and shutdown with waiters. Plus the protocol doc's three replays (A/B/C/A rejection churn, retained A through its retry deadline, accepted-and-held A under arrivals), all with the switch both on and off. Assert ownership drains in every case.
- **CR-SIM screen:** add a scenario with the switch on and report the multi-seed numbers against the baseline: rejections, accepted lookups, useful service before eviction, stored/searchable. Use the measured noise floor (20–28% spread over 8 seeds) to say whether the difference is real. No live run in this slice.
- No fixed sleeps: tokio paused time, or conditions with deadlines ≥ 10 s.
- Keep `crates/cluster` and its chaos and transport tests green. The §19 worker protocol is unchanged.

## Own
`crates/sources/src/pool.rs` (admission parts), `crates/dht/src/node/actor.rs` (admission parts), `crates/pipeline/src/control.rs` (pause ordering), the switch's config plumbing, CR-SIM scenario files, `docs/metrics.md` (the new counters only), `docs/decisions/<date>-CR-ADM.md`.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; the CR-SIM report is in the PR; the fresh-reviewer self-review (steward §1) and an Opus protocol review are done; the runner OK'd Phase 1.
