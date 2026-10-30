# CR-SIM-restart: a simulator scenario for CR-0d's restored restrictions
**Model:** Opus. The DHT restart proof, the sampler and the simulator. Runner brief, 2026-10-30. **Launch after #239 merges**: the simulator moves to `crates/crawler-sim`. This is the follow-up CR-0d-2a named (`docs/decisions/2026-10-30-CR-0d.md`, about line 475).

## Why
CR-0d's gate for honouring restored restrictions is an in-crate, wire-level replay. A CR-SIM scenario adds the end-to-end check, with the real actor and pool against a scripted BEP 51 network:
- after a clean restart with `[dht] safe_restart` on, **no node is sampled before its restored obligation expires**;
- sampling resumes at once for every other node;
- the restart counters add up.

It also gives the crawler program a baseline for restart cost: stored per minute in the first hour after a safe restart, against a fresh 6 h suppression.

## Do
1. **Two legs on one scenario.** Extend `Scenario` with an optional restart: run leg 1 for N minutes, shut down cleanly, then run leg 2 from the same data dir and witness dir.
   - Both dirs are temp dirs.
   - Both legs share the seed's network, and leg 2 starts at leg 1's end time plus a configured gap.
   - Use the existing seams: `RestartConfig { clock, inline_io: true }`. `clock` is a sim clock whose `mono` is tokio's `Instant::now()` and whose `wall` is a fixed base plus elapsed simulated time, carried across the legs. Also `Config.state_path` / `restart` and `safe_restart`.
2. **The net records sample requests:** `sample_infohashes` per remote node, with virtual timestamps. Leg 2 asserts that no node is sampled before `obligation expiry = leg-1 response time + the node's interval`, with the cooldown floor as the code defines it.
   - Report the counts: `restart_obligations_restored`, `refused`, `honoured` and `pending`.
   - Report the conservation of the sampling counters.
3. **A new golden, `restart-honoured.txt`.** Bless it with `CRAWLER_SIM_BLESS=1`. The existing three goldens must stay byte-identical.
4. **Negative control:** a variant with `safe_restart` off. It must show the full suppression (`sample_suppression_reason`), with no samples in the suppression window. It can be an ignored test or a second golden; your choice.
5. **README:** update the CR-SIM README. Also fix its conservation paragraph (about line 91), which still says the actor drops closed-caller requests without counting them. Since #232 they're `peer_queued_dropped`, and the bound remains only because of the 1 s gauge lag (#239 review, optional). **Decisions:** add a section to a new decisions file.

## Own
`crates/crawler-sim/**`, plus test-only seams in `crates/dht` only if one is missing. No production behaviour change.

## Done when
- fmt, workspace clippy `-D warnings` and `cargo test --workspace` pass;
- the production-config clippy and the cargo-tree guard pass;
- the goldens are deterministic: run twice and get identical bytes.

Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Never merge. Work frugally: the account is under a usage warning.
