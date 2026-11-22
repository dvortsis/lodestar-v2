# X-pool-past-cap-room (runner brief, 2026-11-22Z)

Kind: core (the pool, a concurrency-sensitive budget). Dep X-pool-first-request-docs (#1061) is merged. Recorded in `docs/decisions/2026-11-22-runner-records-r12.md` (PR #1068) and in #1061's decisions file.

## The question
- peer-wire's first request is now 88,448 B for a dict over 5 pieces (80 KiB plus the bound, after #1054).
- At the pool's test-only 1 MB budget (`LIMIT` in `crates/sources/src/pool_budget_model.rs`), at 10× seeds (`POOL_MODEL_SEEDS=640`), the model's 30 s `MAX_WAIT` fails. Seven first reservations in five seeds waited 30–44 s; all were granted, and none went past `CHURN_WAIT`.
- The cause: a past-cap growth (583,641 B) no longer fits beside one other first request. 88,448 + 88,448 + 583,641 = 760,537 B, over 750,000 B.
- Production budgets are 64 MiB for the pool (`MAX_HELD_BYTES`) and 4 MiB for a worker (`HELD_BYTES`), and neither is configurable. The 4 MiB budget passes at 10×.
- **The question:** should a past-cap growth at small budgets leave room for two first requests, rather than one? Or is the 1 MB budget simply below what the pool is designed for, so the model should stop claiming 30 s there?

## Step 0 (before building): send me, then wait for "go"
1. The rule as it stands, with pool.rs file:line: where a past-cap growth is admitted, and what it must leave room for.
2. Options with the model's numbers at LIMIT and WORKER, at 64 and 640 seeds: waits over 30 s, the worst wait, slow waits, and grown and held bytes.
   - (a) Leave room for two first requests.
   - (b) Scale the room with the budget.
   - (c) No rule change; the model drops the 30 s claim below some budget, and says why.
   - Anything better you find.
3. **For each option, what a slow or malicious peer can hold, and for how long.** No option may loosen a bound against slow peers; that is the owner's (see owner_queue n 34). Say if one would.
4. Files and an estimate.

## Also in this slice (#1061's review nits)
- `crates/sources/tests/pool.rs` around :5960 (or the pool.rs test at that line): make the lower bound `>= 80 << 10`, so the test proves the 80 KiB first request.
- pool.rs :261 and :268: "first window" should read "first request" where the partial claim's first reservation is meant.
- Re-wrap the over-long comment lines in the `pool_budget_model.rs` `Counts` doc and at `crates/peer-wire/tests/utp.rs:459`.

## Rules
- No change to `MAX_HELD_BYTES`, `HELD_BYTES`, the per-network share, the floor or `SMALL_CLAIM` without saying so at step 0.
- Model tests stay deterministic, with no fixed sleeps. `#[ignore]` is fine only on a new measurement test.
- Opus throughout. A fresh Opus review follows.

## Owns
- `crates/sources/src/pool.rs`
- `crates/sources/src/pool_budget_model.rs`
- `crates/sources/tests/pool.rs`
- `crates/peer-wire/tests/utp.rs`
- `docs/decisions/2026-11-22-X-pool-past-cap-room.md`

## Branch and PR
- Branch `slice/X-pool-past-cap-room`. PR title "X-pool-past-cap-room: Pool: a past-cap growth leaves room for a second first request at small budgets", or retitle it to match the option I approve.
- Gates: `cargo fmt --check`, `cargo clippy -p lodestar-sources -p lodestar-peer-wire --all-targets -- -D warnings`, `cargo test -p lodestar-sources -p lodestar-peer-wire`, plus the model at 640 seeds.
- Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
