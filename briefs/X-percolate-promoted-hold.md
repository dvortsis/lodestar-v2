# X-percolate-promoted-hold: promoted clones of `sig:` searches wait for the backfill instead of being missed

**Runner brief, 2026-11-12 (UTC).** Opus, core (concurrency and the durable promotion queue). About 250 lines with tests.
- **Branch:** `slice/X-percolate-promoted-hold` from fresh `origin/main`.
- **PR title:** `X-percolate-promoted-hold: Percolation: hold promoted clones of sig: searches until the payload_sig backfill is done`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Why
This is rv-754 I1's residual (`git show origin/runner-state:reviews/rv-754-29215524.md`, lines 72–90).
- **What's already right:** a saved search's forward run is held while a `sig:<info hash>` can't resolve every reference (`forward_due`, `hold_unresolved: true`, `crates/lodestar/src/percolator.rs` ~388).
- **What isn't:** a **promoted** torrent's one-off pass (`promoted_due`, `hold_unresolved: false`, ~404) runs anyway, because its promotion row is consumed either way. So during GS-0's `payload_sig` backfill, a promoted clone of a `sig:` search alerts only if its reference is already backfilled. Otherwise the alert is lost for good.

## What to build
1. **A held list in the promotion run.**
   - When a promoted row is due for a search that would be held (the same `hold_unresolved` test the forward run uses), `run_promoted` leaves that row waiting: it's not recorded as done.
   - Other searches still run on the row.
   - The row is recorded done only when every search it's due for has run.
   - Pick the smallest durable design that fits `Store::promoted_due` and `record_promoted`. For example, record per search, or don't record the row while any due search is held. Say which in the decisions file.
2. **Bounded and live.**
   - A held row never blocks the queue for other rows. `PROMOTED_BATCH` still makes progress: skip held rows and take the next.
   - A held row is retried each run until the field is Ready, then runs once.
   - Nothing spins: a run where every due row is held does no extra work beyond the batch read.
3. **General, not `sig:`-only.** Use the same readiness signal the forward path uses. GS-2 (`group_minhash`), RX-1 and ENR-11 will add fields that are partial during backfill, so the hold should follow "the query needs a field that isn't Ready", not the `sig:` keyword itself, where the existing `hold_unresolved` machinery allows it. If it doesn't, keep `sig:` and say so.
4. **No migration.** If the store needs a new column or table, stop and ask the runner (V15 is reserved).

## Owns
- `crates/lodestar/src/percolator.rs` and its tests
- `docs/decisions/2026-11-12-X-percolate-promoted-hold.md`

Ask the runner before touching `crates/store/**` or `crates/search/**`. If you need a new store method, give the runner its exact signature first.

## Tests
- During the backfill, a promoted clone of `sig:<H>` (H not yet backfilled) is not alerted and not lost: once the field is Ready, the next run alerts it exactly once.
- A promoted row also due for an unrelated search: that search runs and alerts at once, and the held search runs later.
- A held row doesn't stall the batch: other rows are processed.
- Restart while a row is held: it's still pending and runs once after Ready, with no duplicate alert.
- No fixed sleeps (waits are conditions with ≥ 10 s deadlines). No tokio `test-util` or `testing` feature on `lodestar`; use the existing percolator test harness.

## Done
- `cargo fmt --check`;
- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`;
- `cargo test -p lodestar` (at least the percolator tests).

Send the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green.

**Session window (owner, 2026-11-12):** sessions are cut back at 00:00Z on 2026-11-13. If the runner keeps this session after that, carry on. If not, push your work in progress to the branch, open the PR as a draft, and report its state.
