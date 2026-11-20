# CR-LOOKUP-END brief (runner, 2026-11-20)

**Slice:** CR-LOOKUP-END. Opus, core, crawler. Branch `slice/CR-LOOKUP-END`, PR titled `CR-LOOKUP-END: <title>`. Follow `.claude/skills/steward/SKILL.md`. Send the runner `ready <sha>` once CI is green.

**Spec:** build-plan §0.5 "Path to 100" item 3 and its test table; `docs/decisions/2026-11-20-path-to-100.md`; `docs/decisions/2026-11-20-path-to-100-cr-spec.md` (r5.1); `docs/reviews/2026-11-20-path-to-100-review.md` §3; the slices.yaml entry.

## What to build
Behind the switch `dht.lookup_end` (environment variable `LODESTAR_DHT_LOOKUP_END`), default **off**:
1. **End on values.** A `get_peers` request ends, and frees its slot, as soon as either:
   - it holds enough values, or
   - its closest set has answered,

   whichever comes first.
   - **Enough values:** a named constant. Justify its value in the decisions file against what the peer fetch needs, and cite the CR-0 or ledger numbers.
   - **Closest set has answered:** define it exactly. For example, every node in the current K-closest set has replied or timed out, and no closer unqueried node remains.
2. **Start on admission.** An admitted request starts dispatching at once, not on the next tick. It still goes through the query-rate limiter and the outstanding cap, so it can never exceed them.

## What must not change
These stay exactly as they are, with or without the switch:
- `LOOKUP_DEADLINE` (25 s);
- the 64-query cap;
- `ALPHA`;
- the query rate;
- the outstanding cap;
- the 32 lookup slots. Slot sizing is CR-1c's job.

With the switch **off**, behaviour must be identical to today, and the existing dht and crawler-sim tests and goldens must pass unchanged.

## Untrusted input: hostile replies (`/security-review`)
- Only validated, de-duplicated compact peers count toward "enough values". Use the existing peer validation; port 0 and martian addresses don't count.
- Spoofed or late replies (wrong tid or address) count for nothing.
- **A single responder can't end a lookup on its own.** "Enough values" needs values from at least two distinct responding nodes, or the closest-set condition. Without this rule, one hostile node could end lookups early for free.
  - When one node answers with values, the lookup simply runs on as it does today: to the closest-set condition or the deadline. That is no regression.
  - Name the distinct-responder count as a constant, and state the rule in the decisions file.
- Memory stays bounded: the existing caps on values per reply and per request hold.
- Add property or fuzz tests on the reply path, covering empty, huge, duplicate and spoofed values. They must show that none of these ends a lookup early, inflates counts or grows memory.

## Ledger (CR-LEDGER, merged as #975 at dac1468a)
- Every new end goes through `finish_lookup` with exactly one `dht_lookup_ended_total{reason}`.
  - If you add a reason value (for example `enough_values` or `closest_answered`), keep the label set a closed enum.
  - Update its `docs/metrics.md` row, and keep the render test covering it.
  - Purposes must still sum exactly to `dht_queries_out_total`.
- **Fold in #975's optional review items.** For these, `crates/dht/src/node/ledger.rs` and `ledger_tests.rs` join your owns:
  1. a unit test that `ALL[i] as usize == i` for `LedgerEnd`, `QueryPurpose` and `HintOutcome`;
  2. extend the gauge test: end the running request, drain the queue, call `publish_slots()`, and assert (occupied, queued) = (0, 0);
  3. zero `slots_occupied` and `queued` in `end_ledger_at_shutdown`.

## Screen (CR-SIM)
- **Dependency:** X-crawler-sim-calibrate (#974) owns `crates/crawler-sim/**` until it merges. Don't edit anything under `crates/crawler-sim` before then. Do the dht-side work first; the runner tells you when #974 merges.
- **Then:** in the calibrated scenario, run 8 seeds with the switch off against on. Report `W`, `q` and `y` beside stored per minute.
- **Keep the change only if both output per slot-second and output per query rise.** Record the numbers in the decisions file. A screen never replaces a live run (CR-AB-LC does that).

## Owns
- `crates/dht/src/node/lookup.rs`, `actor.rs`, `mod.rs`, `ledger.rs`, `ledger_tests.rs`
- `crates/dht/tests/**`
- `crates/lodestar/src/config.rs` (the switch)
- `crates/crawler-sim/tests/sim/**` (after #974 merges)
- `docs/decisions/2026-11-20-CR-LOOKUP-END.md`
- `docs/metrics.md` (your rows only)
- `docs/configuration` rows for the switch, if the config doc lists switches

Ask the runner before touching anything else.

## Tests and rules
- Unit tests for each end condition and for start-on-admission, with the rate and outstanding cap respected.
- An equivalence test with the switch off.
- No fixed sleeps. In dht tests, use tokio's paused clock as the existing tests do; anywhere else, condition waits with deadlines of at least 10 s.
- Never add tokio `test-util` or `testing` to the `lodestar` crate.
- No `unsafe`.
- Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`.
- **Size:** keep it under ~1,500 changed lines. Say why if it goes over.
- **Decisions file:** the constants and their justification, the end definitions, the hostile-reply rule and the screen numbers. Name any refinement of the plan there (§2.19).
