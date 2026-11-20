# X-crawler-sim-calibrate decisions (#974)

A CR-SIM scenario, `live-lookup`, calibrated to CR-0's live lookup, so lookup-cost changes get a screen that can judge them (r5 "Path to 100", `docs/decisions/2026-11-20-path-to-100.md`; build-plan §0.5 step 3). Test code only: no change to `crates/dht` or any production code. The existing goldens are byte-identical, and their sha256 hashes match `main`.

## The model
- **Stale contacts are the one new network mechanism.** `NetConfig::stale_permille` gives each node a fixed set of addresses beside it in id space that never answer, 1.75 on average in `live-lookup`.
  - Replies from inside a target's neighbourhood list them next to the nodes they list.
  - Each one in a lookup's closest set must time out (4 s, at most `ALPHA` = 3 in flight) before the lookup can end, and each closer responder lists more. Most lookups therefore run to the 25 s `LOOKUP_DEADLINE`.
  - That's the shape CR-0 showed: `W` ≈ 23.9 s against the 25 s deadline.
  - **Why near the target only:** the first version listed them in every reply. The sampler's frontier then filled with them, and sampling took 54 of 90 queries a second at 66% unanswered.
  - **Why fixed per node, not per reply:** keying them per (replier, node) made the set unbounded, with the same result. Per node, the network holds a bounded set, as departed nodes linger in many tables.
- **Default off.** At 0, no reply changes and no random number is drawn, so every baseline is unchanged. The edge counters (`net::Edge`), the outstanding gauge and the ledger fields aren't printed unless `Scenario::ledger` is on, which only `live-lookup` sets.
- **A hop-divisor knob was tried and dropped:** in the runs tried, slower convergence (each hop halving the rank distance) didn't lengthen `W`. It isn't in the code.

## Calibration evidence
- **Source:** CR-0's 24 h measurement window (86,440 s), `docs/soak/evidence/2026-10-30-cr0-final/`. The counter deltas are in `cr0-final.md`. `L` and outstanding are means of the 73 baseline samples in `samples-hourly.jsonl`, which are about 20 minutes apart.
  - **`L` is 30.68, not the review's 30.3,** so `W = L / λ` is 23.86 s, not 23.6 s. The review's figures also averaged 3 warmup samples.
- **Results on every one of the 8 `SCREEN_SEEDS`,** asserted by `live_lookup_is_calibrated`:

| Target | Live | Tolerance | 8-seed mean | Seed range |
|---|---|---|---|---|
| Queries unanswered (`timeouts / queries_out`) | 0.382 | ±0.03 | 0.385 | 0.378–0.391 |
| `L`, slots occupied | 30.68 | ±1.0 | 30.82 | 30.80–30.85 |
| `W` = `L / λ` | 23.86 s | ±1.5 s | 23.11 s | 22.53–23.51 s |
| Admitted lookups/s | 1.286 | ±0.10 | 1.334 | 1.311–1.369 |
| `y`, stored per admitted | 0.160 | ±0.04 | 0.164 | 0.149–0.181 |
| Fetch attempts per admitted | 3.66 | ±0.6 | 3.84 | 3.65–4.18 |
| Stored per minute | 12.34 | ±3.1 | 13.12 | 11.78–14.56 |
| DHT queries/s | 76.1 | ±7.6 | 77.4 | 76.5–78.5 |
| `q_all`, every query per admitted | 59.2 | ±6.0 | 58.0 | 57.1–58.6 |
| Mean queries outstanding | 160 | ±24 | 151.9 | 150.9–153.1 |

- **How the knobs were set:** dead share, loss, stale contacts and latency were set against unanswered, `W`, outstanding and the query rate. The BEP 51 interval was set against the query rate. The share of hashes with a swarm and the swarm size were set against attempts per admitted lookup. CR-0's per-attempt fetch outcomes are scripted directly, and `y` follows from those. No output is written in.
- **Tolerances:** about ±8–10% on the DHT figures. ±25% on `y` and stored per minute: the 9 measured minutes hold about 110–130 stores per seed, about 9% Poisson noise.

## What isn't calibrated
- **`q`, lookups' own queries per admitted lookup: 24.4 here.** CR-0 has no queries-by-purpose counter, so only `q_all` (59) is observed. The review's 40–55 was an estimate. In this model sampling and maintenance take the other 34 queries per lookup. CR-LEDGER's per-purpose counter will settle it. If live `q` is far from 24, the scenario needs recalibrating: fewer stale contacts, and slower sampling to keep the total. Flagged for the runner.
- **Lookups that return peers, and how many of the K closest answer:** CR-0 has no live figure for either.
  - The sim reports `peers_found/admitted` (0.57 on seed 1) for comparison with CR-LEDGER's `lookup_without_values_total`.
  - The 70% share of hashes with a swarm is set against fetch attempts per admitted lookup.
- **Fetch duration:** about 6.3 s per attempt here, against CR-0's about 3.8 s. A screen that leans on fetch-socket use, such as CR-1c, should lower `timeout_ms` first.
- **The reply-latency distribution:** CR-0 kept no histogram. The scripted one (60–460 ms uniform, 15% at 3 s) is constrained through the mean hold time (outstanding over queries/s: 2.1 s live, 1.96 s here) and `W`.
- **The sampler's 200,000-state cap,** which CR-0 sat at all day: a 12-minute run never reaches it. The 120 s BEP 51 interval stands in for its share of the query rate. X-sampler-cooldown-split screens that need the cap will need their own scenario knob.
- **CR-LEDGER's counters** aren't on `main` yet. Its metric names reached this slice through the runner, and the ledger names each figure after them, read from the existing counters and the network edge (README, "The ledger").
  - `W` is calibrated as `L / λ`, CR-0's method.
  - CR-LEDGER's residence ratio is reported beside it: 22.96 s on the 8-seed mean.

## Run time
- **The calibration test** takes about 12 s of wall time on 8 threads (about 40 s of CPU, debug build).
- **The whole `sim` test** went from about 9 s to about 20 s.
- **`screen` now runs each arm's seeds on threads** and returns each seed's `[off, on]` ledgers. One runtime per run keeps every run byte-identical.
