# X-routing-table-metric (Opus, crawler/metrics)

Runner, 2026-11-15. Follow-up from the X-test-vm runs: an operator had no way to tell from /metrics whether the DHT routing table had filled or collapsed (for example after a VPN restart), so a slow first half hour could not be told apart from a dead network.

## Goal
Expose the routing table's size on /metrics, so the operator (and the soak and test-VM collectors) can see the table fill, hold and recover.

## Scope
- A gauge for the routing table's node count (`crates/dht/src/routing.rs` already has `len()`), read without holding any lock across an await and without adding work to the hot query path. If the table lives behind the node task, publish the count from that task on its existing tick or on change; never poll it from the HTTP handler by sending a command.
- If a per-bucket or "good vs questionable" split is cheap and already tracked, one extra label is fine; otherwise node count only. Keep label cardinality fixed.
- The metric's row in `docs/metrics.md` (name, type, unit, meaning, when it is 0), following the contract's naming rules. If the UI glossary or crawler status page names routing-table size, use the same words.
- Optional, if it is a one-line add: the collector `KEEP` list in `docs/soak/tools/linux/collect.py` and the shared sanitizer, with the analyzer test fixture updated. Skip it if it pulls in more.

## Owns
`crates/dht/src/**` (only what the gauge needs), `crates/lodestar/src/metrics.rs`, `docs/metrics.md`, the optional collector lines above, tests, and your own `runner/slices.yaml` entry and placement rule. No decisions file unless you make a design call the brief leaves open.

## Tests
- A unit test that the gauge follows inserts and evictions (including the table emptying).
- The /metrics render test shows the new line.
- No fixed sleeps; condition waits have deadlines of at least 10 s.

## Done
fmt, clippy (Linux and `--target x86_64-pc-windows-gnu`), workspace tests green; PR titled `X-routing-table-metric: Expose the DHT routing-table size on /metrics`; tell the runner "ready <sha>". Follow the steward skill. Never merge.
