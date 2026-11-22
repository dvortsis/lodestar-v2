# X-pool-first-request-docs (runner brief, 2026-11-22Z)

Kind: core (the shared fetch pool, its budget model and peer-wire's reservation docs). Dep CR-UTP-FIRST-WINDOW (#1054) is merged. Read `docs/decisions/2026-11-22-CR-UTP-FIRST-WINDOW.md` (especially "Follow-up") and r11 (`docs/decisions/2026-11-22-runner-records-r11.md`) first.

## Background
Since #1054, a metadata fetch's first request is:
- for a dict over 5 pieces: 5 pieces, reserving 80 KiB plus the bound up front;
- for a dict of exactly 5 pieces: the 4-piece window, reserving 64 KiB, then growth;
- for 4 pieces or fewer: the whole dict, reserved whole.

No pool rule assumes a size, but the pool's docs, fixtures and budget model still describe a 64 KiB first request.

## What to change
1. **Docs, to match #1054:**
   - `crates/sources/src/pool.rs:250-251` ("a dict over 64 KiB reserves in two steps…"), `:458` (`SMALL_CLAIM`'s doc), and the test comment at `:5940-5941`;
   - `docs/metrics.md:935`;
   - `crates/peer-wire/src/fetch.rs:153-156` (`FIRST_PIECES`' doc, including "pins only this much") and `:893-894` (`Assembly.limit`);
   - `crates/peer-wire/tests/utp.rs:406`, `:427` and `:458` ("4 pieces" with a reserve; now 5 for that 8-piece dict).
2. **Fixtures:**
   - `crates/sources/tests/pool.rs:80-86` (`WINDOW = 64 << 10`, documented as peer-wire's FIRST_WINDOW);
   - `:2874-2877` (silent claimers pin "only their first 64 KiB window"; it's about 87 KiB in the pool's units now).
   Each fixture uses the size peer-wire actually reserves for the dict it models. The test still proves what it claims.
3. **The budget model** (`crates/sources/src/pool_budget_model.rs`):
   - The invariant at `:104-110`, `held <= share + SMALL_CLAIM`, uses SMALL_CLAIM as "one first window". Write its slack as `share + one first request`, sized from peer-wire's first request. It must still hold.
   - The model reserves `64 << 10` by hand (`:166-176`, `:243`, `:266`). Make it reserve peer-wire's sizes, either:
     - through a public re-export in `crates/peer-wire/src/lib.rs` (that one line only; `fetch` is a private module), or
     - with local constants that a test pins to peer-wire's.
     Pick one at step 0 and say why.
   - Run the model at its usual seeds and at 10x, and report the counts before and after.

## Rules
- **No pool rule changes.** No admission rule, bound or budget may change, and nothing is loosened. If a fixture fails because a rule really depends on 64 KiB, stop and tell me.
- `unsafe_code` stays forbidden. No tokio test-util on the lodestar crate. No fixed sleeps; condition waits need deadlines of at least 10 s.
- Never beside another slice that owns `pool.rs`, `fetch.rs` or `docs/metrics.md`.
- **Mutations:** show that the rewritten invariant fails if the first request's reservation is made larger than peer-wire's (one mutation in a scratch copy).

## Owns
- `crates/sources/src/pool.rs` (docs and test comments only)
- `crates/sources/src/pool_budget_model.rs`
- `crates/sources/tests/pool.rs`
- `crates/peer-wire/src/fetch.rs` (docs only)
- `crates/peer-wire/src/lib.rs` (one re-export only, if chosen)
- `crates/peer-wire/tests/utp.rs` (comments only)
- `docs/metrics.md` (line 935's paragraph)
- `docs/decisions/2026-11-22-X-pool-first-request-docs.md`
- this slice's `runner/slices.yaml` entry

## Step 0 (before building)
1. The re-export or the pinned-constants choice.
2. Each fixture's new size and what its test proves.
3. The invariant's new slack.
4. Files and an estimate.

## Branch and PR
Branch `slice/X-pool-first-request-docs`, PR title "X-pool-first-request-docs: Pool docs, fixtures and its model follow peer-wire's first request". Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test -p lodestar-sources -p lodestar-peer-wire`. Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
