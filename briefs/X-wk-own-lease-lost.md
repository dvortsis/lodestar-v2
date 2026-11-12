# X-wk-own-lease-lost (Opus, concurrency): a worker that re-sights a key it holds never ends its own lease as lost

Runner brief, 2026-11-12 (UTC). Same session as WK-20 (#712, merged as 46b2a5dd), which found the bug. Deps: WK-11 (#699) and WK-20 (#712), both merged. Use the steward skill; Auto mode.

## The bug (from `docs/decisions/2026-11-11-WK-20.md` ~86)
The holder's repeated claim gets its own lease back (`crates/cluster/src/server.rs`: `Claimed::Held` by the holder). The worker runtime puts that into its pool as a new sighting, outside a reply. If the pool fetched the key lately, it turns the sighting away (`on_discovery`, `recent`), and the worker's `Funnel::duplicate` override ends the lease as lost. Another worker then harvests and fetches the key again. Seen with three workers: harvested 6, stored 3, lost 3, two workers fetching each key.

## Do
1. **Fix it in `crates/lodestar/src/worker/runtime.rs`:** a `Held`-by-self answer for a key this worker already holds (or has just fetched) must never end that lease as lost and never cause a second fetch. Decide the right behaviour (treat it as a no-op, or refresh the lease without re-queuing) and say why in the decisions file. Keep the holder's lease intact, and keep the funnel's identity (harvested = terminal − late + in flight) true.
2. **Tests:**
   - a unit test at the runtime level for the `Held`-by-self path (this is the optional `claim()` test that X-spool-damage-log left for you);
   - a WK-20 chaos scenario (or an extension of one) that reproduces the 3-worker case on main and passes after the fix, asserting `lost == 0` from this cause and no `fetch_violations`. Show in the PR that it fails without the fix.
   - **rv-712 nit 2:** in `crates/lodestar/tests/` (your harness's `scenarios.rs` ~432), add the case where the hand-back is recorded after both fetches, which `a_recorded_hand_back_excuses_only_later_fetches` promises. Shared edit to your own merged harness, approved.
   - No fixed sleeps; condition waits with deadlines of at least 10 s.
3. Decisions file `docs/decisions/2026-11-12-X-wk-own-lease-lost.md`, and a one-line pointer from WK-20's decisions file to it.

## Owns
`crates/lodestar/src/worker/runtime.rs`, the harness files from #712 (for the test additions only), the decisions files. **Ask the runner first** for anything in `crates/cluster/**` (WK-11b's next) or `crates/pipeline/**`. Don't touch `crates/pipeline/src/spool.rs` (X-spool-damage-log) or `crates/lodestar/src/services.rs`.

## Bar
- runner/standards.md; `cargo fmt --check`, `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Branch `slice/X-wk-own-lease-lost` from fresh `origin/main`; PR `X-wk-own-lease-lost: A worker that re-sights a key it holds never ends its own lease as lost`. Aim under 600 lines.
- Self-review; push once per round; reply "ready" to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with PR number and full head SHA when ci-ok is green.
