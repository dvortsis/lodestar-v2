# CR-1a-S: streaming peer batches (LookupSession), behind their own switch
**Model:** Opus: a coordinated pool/plugin/actor change. This is concurrency-heavy and carries ownership and cancellation invariants. Runner brief, 2026-10-31. Lane C, the crawler program's third candidate. Its dependencies have merged: CR-1a-H (#246), CR-gate-metrics (#199) and CR-ADM (#228).

## Why
Today a lookup hands the pool its peers only when it finishes: `sources::PeerRequest`'s one-shot carries the final peer vector. So a fetch can't start until the whole lookup is done. Streaming hands peers over in bounded batches as they arrive, so the fetch overlaps the lookup.

**Expected effect:** better conversion and overlap, **not** freed slots, because lookup ownership is kept until the lookup terminates (the spec's "Independent lifetimes"). The screen measures whether it helps; it may not.

## Read first
- `docs/plan/2026-10-27-streaming-and-hints.md`: "Current boundary and required replacement", "Bounded peer delivery", "Independent lifetimes" and "Prepared regression cases" (the streaming rows). **That's the contract,** including the event/ownership table.
- `docs/decisions/2026-10-30-CR-ADM.md`: the permits, which travel with the request. Streaming must keep CR-ADM's one-return-per-request guarantee.
- `docs/decisions/2026-10-30-CR-1a-H.md`: the switch and screen pattern, and the actor hint path, which must keep working with streaming on.
- `runner/standards.md`.

## Do
1. **The switch: `dht.streaming_peers`, a bool, off by default.**
   - Set it in `config.toml` or with `LODESTAR_DHT_STREAMING_PEERS=true`, read at start. A bad value is a config error.
   - Add a config test.
   - With the switch off, behaviour is byte-identical to today. The CR-SIM goldens stay unchanged.
2. **`LookupSession`.** An accepted finder returns a bounded batch receiver plus a separate terminal one-shot.
   - The terminal is a fixed enum: `Complete`, `Deadline`, `Cancelled`, `SourceFailed`, with no peer list. Rejection stays `None`.
   - Allocate the session only after actor admission.
   - A dropped terminal sender means source failure, never success.
   - Terminal delivery never waits on batch-channel space.
3. **Bounds:**
   - 8 endpoints per batch, 2 queued batches per finder, and one actor-pending batch while the channel is full.
   - A per-session seen set capped at 32. Once it's full, ignore further endpoints for that session.
   - Validate every endpoint with the existing public-endpoint policy before buffering. Normalize mapped forms, and dedupe against the session's seen set and the pool entry's retained or tried peers.
   - On a full channel, merge into the pending batch; past that, drop with an aggregate backpressure counter.
   - On termination, do one non-blocking flush, count the discards, close the sender, then resolve the terminal.
   - These are the spec's proposed bounds: make them named constants and record them as candidate defaults (§2.19 flag).
4. **Pool ownership follows the spec's event table exactly:**
   - lookup ownership is separate from fetch readiness;
   - a batch makes the key socket-eligible but never releases a slot, adds an attempt or extends the deadline;
   - multiple families make one logical attempt;
   - a verified success cancels every finder;
   - after a terminal, drain the at-most-two queued batches;
   - generation fencing makes stale batches or terminals no-ops;
   - success racing a terminal never decrements ownership twice.
5. **Metrics,** with no infohash or endpoint labels:
   - batches delivered;
   - peers dropped to backpressure;
   - peers rejected by the entry cap (separate from backpressure);
   - terminal outcomes by kind;
   - time from first batch to first fetch start.

   Document them in `docs/metrics.md`.
6. **Tests.** Implement every streaming row of the spec's "Prepared regression cases": Early batch, Full channel, Success race, Failed first peer, Stale generation, Dual family, Source failure and Final drain.
   - Use scripted finders, paused clocks and condition deadlines of at least 10 s, with no fixed sleeps.
   - Add a property or loom-style interleaving test for success-versus-terminal ownership if it's feasible. If it isn't, say why.
   - Keep CR-ADM's permit-return tests green with streaming on, and keep `crates/cluster` and its chaos and transport tests green.
7. **The CR-SIM screen:** add `Switches { streaming_peers }` and an ignored screen like CR-1a-H's, with arms off and on over seeds 1..=8, against the noise floor, on all three scenarios. Report stored/min, conversion and time to first fetch. Put the table in the decisions file. A null result is a valid result.
8. **Decisions file:** `docs/decisions/<date>-CR-1a-S.md`.

## Own
`crates/dht/**`, `crates/sources/**`, `crates/pipeline/src/dht.rs`, `crates/crawler-sim/**`, the `streaming_peers` lines in `crates/lodestar/src/config.rs`, `crates/lodestar/src/metrics.rs`, `docs/metrics.md`, and your decisions file.

## Size
This will likely exceed 1,500 lines. Plan the split up front:
- **part 1:** the `LookupSession` boundary, actor batching, pool ownership and the regression tests, with the switch off by default;
- **part 2:** the CR-SIM support, the screen and the metrics docs.

Each part must be green and reviewable on its own.

## Done when
- fmt, workspace clippy `-D warnings` and `cargo test --workspace` pass;
- `cargo test -p lodestar --bins` passes;
- the production-config clippy and the cargo-tree guard pass;
- the goldens are byte-identical, and the screen is deterministic.

Open the PR(s), follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with each PR's number and head. Never merge. Work frugally: the account is under a usage warning. Push once per round of fixes.
