# X-sampler-frontier-starve

Runner brief, 2026-11-21Z. Slice id `X-sampler-frontier-starve`, branch `slice/X-sampler-frontier-starve`, PR title `X-sampler-frontier-starve: <title>`. The plan entry is in `runner/slices.yaml` (#1012). Opus builds it.

## The lead
The X-test-vm sampler screen ran with `dht.sampler_cooldown_split` on (X-sampler-cooldown-split, #981). It was a works-as-designed run, not A/B evidence, so treat its numbers as a lead only:
- The split worked as designed: full-seconds 0, spill about 1.2M entries, about 35 h to the cap.
- **The sampler frontier sat at 0 for about 3.3 h,** with every known node in cooldown. Samples/s fell from about 100 to 48–58, and the reply ratio fell from 0.82 to 0.47. Then it recovered on its own.
- Anonymous memory grew more than the split's expected step. That's a separate lead and not this slice's, unless the cause turns out to be shared.

## What
1. **Reproduce it in the simulator first** (`crates/crawler-sim`), with the split on: a scenario where the cooldown table fills until no candidate is eligible. Find the smallest scenario that shows the frontier empty while nodes are cooling down.
2. **Write down the cause** in the decisions file before any fix. Is the frontier refilled only from replies, which stop when every node is cooling? Is there no path that brings in new candidates (lookups, routing-table refresh, BEP 51 `nodes`) while the frontier is empty? Or is the cooldown length the only lever? Name the code path, with file:line references.
3. **A fix only if the cause is the sampler's,** and only behind the same switch (`dht.sampler_cooldown_split`, env `LODESTAR_DHT_SAMPLER_COOLDOWN_SPLIT`). With the switch off, behaviour is byte-for-byte as now; test that.
   - A fix never shortens a cooldown (X-sampler-cooldown-split's rule).
   - It must stay inside the preset's rate and outstanding caps, and keep CR-0d's restart proof.
   - Include a sim test that fails without the fix.
   - If the cause is elsewhere (lookups, routing), stop after step 2 and tell me. I'll place the fix.
4. **A decisions file** `docs/decisions/2026-11-21-X-sampler-frontier-starve.md`, heading with `(#<this PR>)`: the reproduction, the cause, the fix or why there's none, and a metric that would show the starvation in a real run, if one is missing (name it; adding it to `docs/metrics.md` waits for X-metrics-prefix-doc, so tell me rather than editing it).

## Owns (refined at launch)
- `crates/dht/src/node/sampler.rs`, `crates/dht/src/node/sampler/**`
- a new sim scenario or test under `crates/crawler-sim/tests/` (name the file)
- the decisions file, and this slice's `slices.yaml` entry

Nothing else under `crates/dht/src/node/**` without asking first. CR-1c has merged, so main has its `running_slots` change.

Send me the exact list before you edit anything else.

## Bars
`runner/standards.md`. `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar-dht -p lodestar-crawler-sim` (check the package names), and the workspace before review. No fixed sleeps; condition waits with deadlines of at least 10 s. No cloud runs: the X-test-vm lane runs any screen, and I'll ask it. Push once per round, and send "ready <pr> <sha>" when CI is green.
