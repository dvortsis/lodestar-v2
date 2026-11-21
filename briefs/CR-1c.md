# CR-1c

Runner brief (2026-11-21). Branch `slice/CR-1c`; PR title `CR-1c: Lookup cap sized to the intensity preset, behind dht.preset_slots`. Core, Opus.

## The switch (fixed by the runner, 2026-11-20; don't rename)
`dht.preset_slots`, env `LODESTAR_DHT_PRESET_SLOTS`. Default **off**. Off must be byte-identical: the CR-SIM goldens are unchanged and no new state is allocated.

## Why
Read build-plan §0.5 "Path to 100", item 5 and the table above it; `docs/decisions/2026-11-20-path-to-100.md`; and `docs/decisions/2026-11-20-path-to-100-cr-spec.md` A1. CR-0 showed lookup cost limits the crawler. CR-LOOKUP-END (#977, `dht.lookup_end`) now ends a `get_peers` request as soon as it has enough values or its closest set has answered, so `q` (queries per lookup) and `W` (slot residence) fall. Once they do, the concurrent peer-request slot count (today `PEER_REQUEST_CAPACITY = 32` in `crates/dht/src/node/mod.rs`, confirm this is the binding cap) becomes the limit before the Balanced 200 queries a second does. CR-1c sizes the slot count to the intensity preset, so the **preset's query rate binds first**, not the slot count.

CR-AB-LC tests CR-1c together with CR-LOOKUP-END as one treatment (A1). Its mechanism check needs, from CR-LEDGER's series: lookups ending on values above 0, **occupied slots above 32 at least once** in the window, and the query rate within the preset. Make sure that series exists, or name what the collector must read.

## Do
1. **Find the binding caps.** Map every limit on concurrent peer lookups along the path from the pool to the wire: the pool's `max_lookups` (256), the DHT's `PEER_REQUEST_CAPACITY` (32), `MAX_LOOKUPS` (16, maintenance), the outstanding cap (`Intensity::max_outstanding`) and the rate. Record in the decisions file which one binds today, at each preset.
2. **Size the slots to the preset when the switch is on.**
   - Derive the slot count from the preset's query rate and outstanding cap, so that at Balanced the rate, not the slot count, is the binding limit once `q` has fallen.
   - Each preset (Low, Balanced, Aggressive) gets its own value. State the formula, the numbers and why in the decisions file. CR-AB-ADM's result only informs the default count; use the CR-LEDGER figures in build-plan §0.5 if it isn't decided yet.
3. **Never raise the query rate or the outstanding cap** (owner, r5). Those stay exactly as they are. Also unchanged: the 25 s `LOOKUP_DEADLINE`, the 64-query `MAX_PEER_QUERIES`, `ALPHA`, the per-IP guards and the T1 hint caps.
4. **Bounded state.** Any queue or per-slot state is bounded by the preset's slot count. State and measure the memory at Aggressive.
5. **WK-4 review item carried here:** in `actor.rs` `on_external_ip`, `fits` should reject duplicate slots among kept saved ids. Fix it under its own test. It isn't behind the switch, because it's a correctness fix; say so in the decisions file.
6. **Tests,** per build-plan §0.5's table, on tokio's paused clock against the real actor:
   - off is byte-identical, CR-SIM goldens included;
   - on, occupied slots can exceed 32 but never the preset's count;
   - every permit is released exactly once on every end (values, closest set, deadline, cancel, shutdown);
   - no packet exceeds the rate or outstanding cap;
   - the deadline and per-request query cap still hold.
   Condition waits of at least 10 s, no fixed sleeps.
7. **The CR-SIM screen.** In X-crawler-sim-calibrate's live-lookup scenario, run 8 seeds, off against on, each with `lookup_end` on. Report `W`, `q`, `y` and stored per minute. Keep it only if output per slot-second and per query both rise. Record it in the decisions file as Inconclusive or a lead, never a gain. A screen never replaces the live run.
8. **Metrics:** add a series only if CR-LEDGER lacks one the A1 check needs. Name it in `docs/metrics.md`, and send the runner the name before you push.
9. **The decisions file:** `docs/decisions/2026-11-21-CR-1c.md`.

## Owns (refine at launch, per the slice's note)
- `crates/dht/src/node/actor.rs`, `crates/dht/src/node/mod.rs` (the config field and the capacity), `crates/dht/src/node/*_tests.rs` files you add or need, `crates/dht/tests/**` only for new files
- `crates/sources/src/pool.rs` only if the pool's `max_lookups` must follow the preset; say so before you edit it
- `crates/lodestar/src/config.rs` (the switch and env), `crates/lodestar/src/metrics.rs` and `docs/metrics.md` only per step 8
- `crates/crawler-sim/**` only for a screen test or scenario flag; goldens must not change
- the decisions file, `runner/slices.yaml` (your entry's `owns` refinement only)

Send the runner your refined owns list before you edit anything outside `actor.rs`, `mod.rs` and `config.rs`.

## Not
- No change to the rate, the outstanding cap, the deadline, `MAX_PEER_QUERIES`, `ALPHA` or the guards. No tokio `test-util` or `testing` features on the `lodestar` crate. No `unsafe`. No new dependency.
- Don't touch `crates/peer-wire/**` (#1008 is open there) or `crates/filter/**` (#1009).

## Checks
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar-dht -p lodestar-sources -p lodestar -p lodestar-crawler-sim` (match the real package names), the CR-SIM goldens, `cd tools/status-connector && npm test`. Run `/security-review` on the diff before the PR.

When CI is green on your head, send the runner `ready <pr> <sha>`.
