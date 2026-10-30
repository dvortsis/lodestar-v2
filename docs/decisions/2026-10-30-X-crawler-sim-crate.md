# X-crawler-sim-crate decisions (#239)

Refines [CR-SIM](2026-10-30-CR-SIM.md) on where the simulator lives. The simulator, its scenarios and its goldens are unchanged.

- **The simulator moves to its own crate, `crates/crawler-sim` (`lodestar-crawler-sim`, `publish = false`),** from `crates/sources/tests/sim`. It holds no library code: the simulator is still the `sim` test.
  - The goldens are byte-identical after the move, with the same blob hashes.
  - The commands now name the new package: `cargo test -p lodestar-crawler-sim --test sim`, with `CRAWLER_SIM_BLESS=1` as before.
- **Only this crate enables the dht and sources `testing` seams.** `lodestar-sources` drops its self-dev-dependency (`features = ["testing"]`) and its `testing` dev-dependency on `lodestar-dht`.
  - So `cargo test -p lodestar-sources` and `cargo test -p lodestar-dht` now build the production types: the pool's randomly keyed request map, and the actor's hash tables and UDP sockets.
  - This answers #209's review finding I1.
  - CI's "dht and sources tests without test seams" step runs both.
- **Unchanged:** a workspace build still unifies `testing` into dht and sources, through this crate's dev-dependencies. So the production-config clippy step, and the `cargo tree` guard that no test-only feature reaches `-p lodestar`, stay as they were. Both pass.
