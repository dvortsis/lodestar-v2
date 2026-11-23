# CR-SOFT-TIMEOUT (runner brief, 2026-11-23Z)

Core crawler slice, Opus. It runs `/security-review` before "ready", and every result gets a fresh Opus review before merge. Placed by `docs/decisions/2026-11-23-runner-records-r14.md` (the owner's 2026-11-23 delegation, `docs/owner-decisions.md` §2.82). Its slices.yaml entry is `CR-SOFT-TIMEOUT`.

## Goal
Build-plan §0.5's bounded soft timeout, now rather than waiting for CR-5.
- **The problem:** CR-0's ledger shows a lookup holding its slot for about 24 s of the 25 s `LOOKUP_DEADLINE`. A lookup ends only when its closest live candidates have all answered: 3 in flight, a 4 s `query_timeout`, and about 38 % of queries unanswered. Unanswered queries keep a lookup waiting.
- **The change:** a lookup stops waiting on an unanswered DHT query sooner.

## Fixed by the runner
- **The switch is `dht.soft_timeout`** (config field `soft_timeout: bool`, default `false`, beside `lookup_end` and `hint_first` in `crates/lodestar/src/config.rs`). With the switch off, behaviour is byte-identical: CR-SIM goldens unchanged, and no new state allocated.
- **It only shortens waits.** It never:
  - lengthens one;
  - raises the rate, the outstanding cap, `ALPHA` or the 64-query per-request cap;
  - moves `LOOKUP_DEADLINE` or `query_timeout`;
  - loosens any bound against slow peers. Those bounds are the owner's; ask before touching one.
- **A late reply** (after the soft timeout, before `query_timeout`) is still accepted and settles exactly once. The query keeps counting against the global outstanding cap until it answers or reaches `query_timeout`.
- **It works with `dht.lookup_end` both on and off.** Test all four combinations.

## Step 0 (send me, then wait for "go")
1. **The design you propose.** Name the mechanism. For example:
   - (a) a candidate unanswered after the soft timeout no longer blocks the closest-set end;
   - (b) it frees an `ALPHA` lane so the next candidate is queried. This can raise `q`, so say how the 64 cap and the rate keep it bounded;
   - (c) both.
   Give the soft timeout value and where it comes from: CR-LEDGER's time-to-answer series, or the calibrated CR-SIM scenario. Recommend one, and say why it lowers `W` without raising `q` per stored.
2. **file:line** for where the lookup waits today (`actor.rs` around 2537 and 2918, `lookup.rs`) and where your change goes.
3. **The test plan, from build-plan §0.5's table:**
   - unit and property tests on tokio's paused clock in `crates/dht` (never tokio test-util on the `lodestar` crate), covering:
     - each permit released exactly once on every end;
     - the deadline and cap holding;
     - no packet over the rate or outstanding cap;
     - late and duplicate replies settling once;
   - a fuzz or property test on the reply path: hostile replies can't end a lookup early for free or grow memory;
   - the calibrated CR-SIM screen, 8 seeds, off against on, under §0.5's standing rule: **stored per slot-second and stored per query both rise** (item 32's CR-1c rule doesn't apply);
   - the memory bound.
4. **Owns** (refine them here): `crates/dht/src/node/lookup.rs`, `crates/dht/src/node/actor.rs`, `crates/dht/src/node/mod.rs`, new test files in `crates/dht/src/node/` or `crates/dht/tests/`, `crates/lodestar/src/config.rs` (the switch only), `crates/crawler-sim/tests/sim/**`, `docs/metrics.md` (if a counter is added) and `docs/decisions/2026-11-23-CR-SOFT-TIMEOUT.md`. Name anything else, with a reason.
5. **Metrics:** if you add a counter (for example, soft-timeout expiries by purpose), it goes in `docs/metrics.md` and is additive.

## Rules
- Untrusted network input: think adversarially. A spoofed or slow responder must not gain anything from the soft timeout.
- `unsafe_code` stays forbidden. No fixed sleeps in tests; condition waits get deadlines of at least 10 s.
- Never beside X-capacity-rejection-source (API lane), which also owns `actor.rs` and `mod.rs`. The runner keeps them apart.
- About 1,500 changed lines; split it if it's larger.

## Branch and PR
- Branch `slice/CR-SOFT-TIMEOUT`. PR title "CR-SOFT-TIMEOUT: lookups stop waiting on an unanswered DHT query sooner (dht.soft_timeout, off by default)".
- Gates: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (at least `-p lodestar-dht -p lodestar-crawler-sim -p lodestar`), the CR-SIM screen output in the PR, and `/security-review`.
- Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
