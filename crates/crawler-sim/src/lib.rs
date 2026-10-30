//! The deterministic crawler simulator (CR-SIM) lives in `tests/sim`: it
//! runs the real DHT node actor and fetch pool against a scripted network,
//! and asserts on goldens. This crate holds no code of its own; it exists
//! so the dht and sources `testing` seams are enabled only here, never in
//! those crates' own tests or in the `lodestar` binary.
