# CR-HINT-FIRST

Opus, core crawler work on untrusted network input. Needs /security-review. Its deps CR-LOOKUP-END (#977) and X-crawler-sim-calibrate (#974) have merged, and so have X-sampler-cooldown-split (#981) and X-sec-S1-18 (#998).

## Spec
- docs/build-plan.md §0.5 "Path to 100", item 4. A hash captured from a BEP 51 reply gets one direct `get_peers` to that responder at once: CR-2's T1 tier at the pool, one hash per responder. The request finishes if that returns values. The per-IP hint guard stays as it is.
- The gates table, same section:
  - Fuzz or property tests on the reply path: hostile replies (empty, huge, duplicate, spoofed values) can't end a lookup early for free, inflate counts or grow memory.
  - A calibrated CR-SIM screen, 8 seeds, off against on, reporting `W`, `q` and `y` beside stored per minute. Keep the feature only if output per slot-second and per query both rise. A screen never replaces a live run.
- docs/decisions/2026-10-30-CR-1a-H.md, the contract this sits beside. Its screen found:
  - about 94% of hints expired before the pool asked for the hash, because keys wait longer than HINT_TTL;
  - about half of the rest were throttled by the per-IP guard, because one responder lists about 20 hashes while the guard allows one hinted send per IP per minute.
  CR-HINT-FIRST targets both: dispatch at capture, and one hash per responder.
- docs/plan/dht-crawler-implementation-plan-2026-10-25.md: CR-2, the T1 tier.
- docs/decisions/2026-11-20-runner-records-r5.md: the switch is fixed as `dht.hint_first`, env `LODESTAR_DHT_HINT_FIRST`, off by default.

## The runner's calls (§2.26; flagged under §2.19)
1. **"Finish on values" goes through the existing end rule.**
   - A hinted reply's values count toward CR-LOOKUP-END's end condition exactly like any reply's: `Actor::has_enough_values` with its per-source spread rule from #998.
   - Hint-first adds no path where a single responder ends a request, and makes no exception for the hinted responder.
   - If a single responder's values can't end a request, the request carries on through ordinary routing within its unchanged deadline.
   - Record this in the decisions file as the runner's refinement of item 4's "finishes if it returns values".
2. **Off is today's behaviour exactly.** With `dht.hint_first` off: no new state is allocated, sends are byte-identical, and the CR-SIM goldens are unchanged. That matches CR-1a-H's proof style.
3. **Composition with `dht.responder_hints`** (CR-1a-H). Define and test the combinations. Hint-first may reuse CR-1a-H's capture, checks and guard; it must not weaken them. The guard's 60 s interval and its 20,000-IP capacity stay unchanged.
4. **Unchanged caps.** The 32 owned requests, the 3-RPC in-flight budget, the 64-query cap per request, LOOKUP_DEADLINE, the query rate and the outstanding cap, the admission permits, and pool fairness for non-hinted keys.
   - The T1 tier must be bounded: one hash per responder, and a cap on T1 keys waiting. Name both constants.
   - It must not starve the other tiers. Test this.

## Owns
These come from slices.yaml; refine them in your own entry if needed:
- crates/dht/src/node/hints.rs
- crates/dht/src/node/hint_tests.rs
- crates/dht/src/node/actor.rs
- crates/sources/src/pool.rs
- crates/sources/src/pool/admission.rs
- crates/lodestar/src/config.rs

These may be added to your entry:
- crates/crawler-sim/tests/** (the screen only)
- docs/metrics.md (the new series' section only)
- docs/decisions/<date>-CR-HINT-FIRST.md

Never runs with CR-1c, which shares actor.rs, pool.rs and config.rs.

## Tests and gates
- Unit and paused-clock actor tests, plus a property test over hostile hinted replies: empty, huge, duplicate, spoofed or private endpoints, and a responder rotating addresses within one /24.
- Off-switch tests showing nothing is allocated and sends are identical.
- A test that T1 doesn't starve the other tiers.
- No fixed sleeps; condition waits have deadlines of at least 10 s.
- Mutations on the one-hash-per-responder rule, the T1 cap, the end-rule path and the off switch must die.
- The CR-SIM screen goes in the decisions file, CR-1a-H style: 8 seeds, off against on, with W, q, y, stored per minute and the noise floor.
- fmt, clippy -D warnings, and cargo test for dht, sources, lodestar and crawler-sim. Run the status-connector npm test if you edit slices.yaml.
- CR-COL-5's collector needs a TREATMENT_SWITCHES and STACKED entry for stage 3. That's not this slice; the runner tracks it.
