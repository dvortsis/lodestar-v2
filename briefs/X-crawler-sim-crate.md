# X-crawler-sim-crate: move the simulator into its own crate
**Model:** Opus (feature unification in the DHT and sources crates). Runner brief, 2026-10-30. **Launch after CR-0d-2a merges**: both touch the dht/sources Cargo and actor seams.

## Why
CR-SIM (#209) put its simulator in `crates/sources/tests/sim/`, with a `testing` feature on lodestar-dht and lodestar-sources, and a self-dev-dependency in `crates/sources/Cargo.toml`. Because of that self-dev-dep, `cargo test -p lodestar-sources --lib` always builds the deterministic `RequestMap`. The pool's production map/hasher configuration is never unit-tested. #209's review (I1) found this; #209's CI added production-config clippy plus `cargo test -p lodestar-dht`.

## Do
1. Create `crates/crawler-sim` (`publish = false`). It depends on lodestar-dht and lodestar-sources with `features = ["testing"]` and holds the harness, scenarios and goldens moved from `crates/sources/tests/sim/`. `CRAWLER_SIM_BLESS=1` keeps working.
2. Drop sources' self-dev-dep. `cargo test -p lodestar-sources --lib` and `cargo test -p lodestar-dht` must then build production types. Add `cargo test -p lodestar-sources --lib` to the CI step #209 added.
3. The cargo-tree guard stays and still passes: no `testing` or `test-util` reaches `-p lodestar`.
4. The goldens must be byte-identical after the move. Prove it.
5. Update the CR-SIM README and decisions; CR-ADM's brief points at the new crate.

## Own
`crates/crawler-sim/**`, `crates/sources/Cargo.toml`, `crates/sources/tests/sim/**` (removed), `crates/dht/Cargo.toml` (only if needed), the workspace `Cargo.toml` members, and the CI step in `.github/workflows/ci.yml`.

## Done when
fmt, workspace clippy `-D warnings` and `cargo test --workspace` pass; production-config clippy and tests pass for dht and sources; the guard passes; and the goldens are unchanged. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg). Never merge.
