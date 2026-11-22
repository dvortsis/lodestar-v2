# X-routing-table-metric decisions (#945)

An operator on the X-test-vm runs couldn't tell from `/metrics` whether the DHT routing table had filled, or had collapsed after a VPN restart, so a slow first half hour looked the same as a dead network (runner's brief, 2026-11-15). This adds the operator gauges in `docs/metrics.md`'s CR-0 DHT table (architecture §5, the DHT).

## The size was already there

- **`dht_routing_nodes`** has been on `/metrics` since CR-0. It's the sum of the IPv4 and IPv6 tables, stored by the node actor at the end of each maintenance tick (1 s) and read from the stats snapshot. The HTTP handler sends no command.
- **It had no row in `docs/metrics.md`,** and the Linux soak collector's allowlist (`docs/soak/tools/linux/collect.py`) dropped it, so it never reached a soak snapshot. Both are fixed. Its name and shape don't change, so earlier evidence and the API's `routing_nodes` still match it.

## A second gauge for good nodes

- **The size can't show a collapse.** The actor only replaces an entry, never drops one (`RoutingTable::remove` exists, but the actor doesn't call it). A node that stops answering turns bad after two missed queries but keeps its slot until a verified node from the replacement cache takes it (`RoutingTable::failed`). After the network goes away, `dht_routing_nodes` stays near its peak.
- **`dht_routing_good_nodes`** counts the entries that are good at the tick, by BEP 5 (`Status::Good`): answered us within 15 minutes, or answered before and queried us within 15 minutes. It falls to 0 within 15 minutes of the node no longer hearing from its peers and climbs back as they're heard again. A peer that answered once and keeps querying us keeps counting, so the gauge can stay above 0 while only our outbound queries are lost (the runner's review). This is the runner's call, as operator UX inside the brief's scope (`docs/owner-decisions.md` §2.19).
  - **A separate series, not a `{status}` label on the size.** A label would change `dht_routing_nodes`' shape and break sums over earlier evidence. Questionable and bad aren't split out: the operator's question is whether the table is alive, and size minus good answers the rest.
  - **The cost** is one pass over the main buckets per tick, alongside the existing `len()` pass. That's at most 160 buckets × 8 entries per family. It runs on the node task and touches no query path.
  - **Not on the status page or in the API yet.** `/crawler`'s `routing_nodes` stays the size. Showing good nodes to users is a UI decision for a later slice.
- **Good never reads above the size.** They're two atomics stored one after the other on the tick, so a scrape between the stores could pair different ticks. No store and load order prevents that both while the table grows and while it shrinks, so `Stats::snapshot` clamps good to the size it read (the runner's review asked for the order; the clamp holds in both directions).
- **Both gauges go in the collector's allowlist,** unlabelled and not required, since a run with the DHT off has neither.

## Tests

- `routing::tests::good_counts_only_nodes_that_answered_recently`: good drops for a bad node and after 15 minutes of silence, comes back with a reply, and never counts a node that only queried us (it's in the replacement cache).
- `node::actor::tests::routing_gauges_follow_the_table_on_each_tick`: both gauges change on the tick, not on insert; a bad node leaves good but not the size; on a controlled clock (`publish_routing(now)`, not real time) good falls to 0 after 15 minutes with no reply and comes back when a node answers; removing every node takes both to 0 (runner, 2026-11-15).
- **The tick's pass allocates nothing** (runner): `publish_routing` sums `len()` and `good(now)` per family in one loop.
- `node::routing_gauge_tests::good_never_reads_above_the_size`: the snapshot clamps good to the size.
- `metrics::tests::the_routing_gauges_render_and_are_documented`: both lines render, 0 is rendered rather than left out, and each has its `docs/metrics.md` row.
- The collector tests: a snapshot keeps `lodestar_dht_routing_nodes`, drops a labelled variant, and doesn't require it.
