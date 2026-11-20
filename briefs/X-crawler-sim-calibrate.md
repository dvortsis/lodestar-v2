# X-crawler-sim-calibrate: a CR-SIM scenario matching CR-0's live lookup

**Model:** Opus. **Branch:** `slice/X-crawler-sim-calibrate`. **PR title:** `X-crawler-sim-calibrate: <title>`.

## Why
The owner adopted r5 "Path to 100" on 2026-11-20 (PR #970: `docs/build-plan.md` §0.5, `docs/decisions/2026-11-20-path-to-100.md`; read them on branch `docs/dht-throughput-plans-20261119` if #970 isn't merged yet). Every lookup-cost change (CR-LOOKUP-END, CR-HINT-FIRST, CR-1c, X-sampler-cooldown-split) gets a calibrated CR-SIM screen, 8 seeds, off against on, before it takes host time. Today's scenarios end lookups in 5.5–16 s, so they can't judge changes to lookup length either way. Live CR-0 (24 h) showed about 24 s slot residence per admitted lookup against the 25 s deadline, about 38 % of queries unanswered (4 s timeout, 3 in flight), 30.3 of 32 slots occupied on average, 1.286 admitted lookups/s, 0.160 stored per admitted lookup, and about 76 DHT queries/s of the Balanced 200.

## What to build
1. A new scenario in `crates/crawler-sim` whose network model reproduces those figures within stated tolerances: unanswered rate, reply-latency distribution, how many closest-set candidates answer, and the share of hashes with any peers. Calibrate it, don't hard-code the outputs.
2. The scenario reports `W` (mean slot residence), `q` (DHT queries per admitted lookup) and `y` (stored per admitted lookup) beside stored per minute, plus slot occupancy. Use the same definitions CR-LEDGER uses (CR-LEDGER is being built in parallel; it owns the DHT side, and the runner will pass you its metric names when fixed). If the sim can only observe some of these at the network edge, say so.
3. A short README section in `crates/crawler-sim/tests/sim/README.md`: the calibration targets, the tolerance each meets, 8-seed results, and how a later slice runs an off/on screen with it.
4. A decisions file `docs/decisions/2026-11-20-X-crawler-sim-calibrate.md` with the calibration evidence.

## Bounds
- **Existing goldens stay byte-identical.** Show it.
- Deterministic per seed. Runtime of the new scenario fits the existing CI job's budget (state its time); if it can't, add it as an opt-in run and tell the runner.
- No changes to `crates/dht` or production code. If the sim can't express the live lookup without a production hook, stop and tell the runner.

## Tests
- The scenario lands within tolerance on 8 seeds, asserted in a test.
- Goldens unchanged.
- No fixed sleeps (the sim uses its own clock).

## Rules
- Owns: `crates/crawler-sim/**` and your decisions file. Running alongside you: CR-LEDGER (`crates/dht/src/node/{mod,actor,lookup}.rs`, metrics), CR-COL-5 (`docs/soak/tools/**`), GS-3. Don't edit their files.
- Before the PR: fmt, clippy `-D warnings` (Linux and `x86_64-pc-windows-gnu`), the crawler-sim tests.
- Your `runner/slices.yaml` entry comes with #970.
- ≲ 1,200 changed lines.
- Use the steward skill. Report `ready <sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) when CI is green.
