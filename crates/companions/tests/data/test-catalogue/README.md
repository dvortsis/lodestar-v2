# Test catalogue (CMP-G)

Test-only. `crates/companions/tests/gate.rs` reads `catalogue.json` with
`include_str!` and loads it through `Catalogue::from_json`, which exists only
under the `testing` feature. Nothing a release builds reads it; the gate
checks that.

It holds the shipped qBittorrent, Prowlarr and gluetun entries with test
image digests and no pins. The gate pins Prowlarr's build for the running
platform to its own test companion (the gate's test binary, packed as the
artifact) at run time.
