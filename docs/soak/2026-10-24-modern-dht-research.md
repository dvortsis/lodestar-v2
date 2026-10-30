# Modern DHT crawler research

Recorded 2026-10-24, America/Denver. Research and recommendations only; no application, operating-system or soak-runtime changes. Repository examined at `ad0ec5a84555b8b36e7ae94ed515b84ad6d46e02`. Upstream links refer to the versions inspected on this date; moving branches may change.

Companion records: [soak evidence](2026-10-24-v0.5.0-metadata-throughput.md), [throughput proposals](2026-10-24-throughput-strategy.md), and [comparison with the supplied strategy](2026-10-24-strategy-gap-analysis.md).

## Conclusion

The most relevant improvements are better use of information already received, early peer delivery, bounded request lifetimes and adaptive concurrency. They do not require a new DHT protocol or database. Modern transport implementations offer additional headroom, but the observed Lodestar bottleneck is conversion of harvested hashes into usable peer connections, not demonstrated CPU or storage saturation.

This is a review of primary documentation and source, not a live comparative benchmark. No claimed speedup below has been reproduced on Lodestar.

## Established techniques worth adopting first

### Preserve and use the sampling node

Bitmagnet retains the source node with sampled hashes and can ask that node for peers directly. This avoids starting every hash with a fresh iterative search. Lodestar currently emits sampled hashes without that endpoint. See Bitmagnet's [sample handling](https://github.com/bitmagnet-io/bitmagnet/blob/main/internal/dhtcrawler/sample_infohashes.go) and [peer requests](https://github.com/bitmagnet-io/bitmagnet/blob/main/internal/dhtcrawler/get_peers.go).

The protocol basis is that a sample represents a key for which the responding node holds peer values. The node itself is not necessarily a torrent peer, and its records may be stale. Preserve endpoint and timestamp, try a bounded direct request, then fall back. [BEP 51](https://www.bittorrent.org/beps/bep_0051.html).

### Deliver useful responses before completing traversal

Libtorrent invokes its peer callback when a response provides peer values; the whole traversal need not finish first. Its aggressive lookup option also allows additional queries as closer nodes become available. These are separate controls: early delivery reduces latency without inherently increasing concurrency, whereas aggressive branching consumes more network budget. [Peer lookup implementation](https://raw.githubusercontent.com/arvidn/libtorrent/RC_2_0/src/kademlia/get_peers.cpp), [settings reference](https://www.libtorrent.org/reference-Settings.html).

For Lodestar, pair early delivery with explicit request acceptance, start, completion and cancellation. Otherwise an application timeout can leave obsolete work consuming the DHT's smaller active budget. The existing progress failure must be repaired before increasing that budget.

### Measure useful work and tune by outcome

Expose per-stage queue delay, active work, first-peer latency, timeout causes, source provenance and unique verified completions. Libtorrent's lookup diagnostics include outstanding requests, responses, timeouts, remaining nodes and branching factor, illustrating the visibility needed to distinguish starvation from network failure. [API reference](https://www.libtorrent.org/single-page-ref.html).

Use fresh endpoint evidence, expiring negative caches and bounded parallel peer attempts. Reserve exploration capacity so ranking does not permanently exclude uncommon content. Compare bytes and CPU per verified record as well as records/minute.

## Recent work and how much confidence to place in it

The Python crawler LIF reports a September 2026 experiment improving verified metadata from 15 to 825 per minute using origin-first queries, streaming peers, shorter connection waits and other scheduling changes. However, these were short, author-run tests, with concurrency increasing from 32 to 256 and outgoing traffic also increasing. This supports testing the techniques; it is not independent evidence of a 55-fold gain at equal resource cost or long-run stability. [LIF repository and benchmark notes](https://github.com/J4GL/LIF).

Bitmagnet's FAQ reports 100–1,000 torrents/minute depending on resources and network. Treat this as a comparator target to reproduce, not a guaranteed baseline or controlled result. Lodestar's roughly 0.38/minute soak average is plainly inadequate, but the nominal ratio is not an apples-to-apples benchmark. [Bitmagnet FAQ](https://bitmagnet.io/faq.html).

Pubky's Rust Mainline implementation emphasizes prompt DHT responses and supports mutable/immutable record use cases. Pubky's broader architecture uses DHT records for discovery, with application data hosted elsewhere. This is useful evidence that a modern DHT implementation can serve additional discovery roles, not a drop-in global torrent metadata cache. [Mainline implementation](https://github.com/pubky/mainline), [Pubky architecture](https://pubky.org/mainline-dht/).

## Transport improvements: useful after profiling

Aquatic demonstrates a modern multithreaded, in-memory Rust UDP tracker architecture and compares event-loop and io_uring approaches. Tracker request handling is not equivalent to crawling remote DHT nodes and downloading metadata. Its request-rate results cannot be read as metadata/minute expectations. [Aquatic UDP](https://raw.githubusercontent.com/greatest-ape/aquatic/master/crates/udp/README.md), [2024 load test](https://raw.githubusercontent.com/greatest-ape/aquatic/master/documents/aquatic-udp-load-test-2024-02-10.md).

Torrust provides both microbenchmarking and end-to-end load-test methods. Benchmark configurations and enabled operational services matter; comparisons must match logging, statistics, persistence and cleanup costs. [Torrust benchmarking documentation](https://github.com/torrust/torrust-tracker/blob/develop/docs/benchmarking.md).

Lodestar could later evaluate batched UDP syscalls, socket sharding and platform-specific backends if profiles show syscall or actor CPU saturation. The current Windows soak and low measured CPU do not justify treating Linux tuning as the immediate remedy. Batching cannot make a stalled lookup progress by itself.

## Protocol and reachability opportunities

- **IPv6:** IPv4 and IPv6 DHT routing are distinct. Complete peer-discovery and connection paths for both families, rather than counting an IPv6 socket as full coverage. [BEP 32](https://www.bittorrent.org/beps/bep_0032.html).
- **uTP and hole punching:** Finish the application transport integration before evaluating hole punching. BEP 55 uses a mutually connected peer to arrange a connection; it cannot create that introducer when no usable peer exists. Treat it as additional reachability, with incremental yield measured separately. [BEP 55](https://www.bittorrent.org/beps/bep_0055.html). DHT-native hole punching remains a proposal to monitor, not an assumed deployed capability: [upstream discussion](https://github.com/bittorrent/bittorrent.org/issues/178).
- **Tracker discovery and scrapes:** A UDP tracker announce can return peers; a scrape supplies counts. Use counts as incomplete ranking evidence, not proof that a hash is dead. DHT scrape counts are probabilistic estimates. [BEP 15](https://www.bittorrent.org/beps/bep_0015.html), [BEP 33](https://www.bittorrent.org/beps/bep_0033.html).
- **BitTorrent v2:** SHA-256 identities and the v2 file-tree format are compatibility requirements to assess, not a general shortcut around peer discovery. [BEP 52](https://www.bittorrent.org/beps/bep_0052.html).

## Recommended order and acceptance

1. Repair lookup progress, deadlines, acknowledgement and cancellation; expose stage outcomes.
2. Preserve source endpoints, try direct peer queries and stream peer results immediately.
3. Tune admission, endpoint retry policies and concurrency against successful completions.
4. Complete uTP/IPv6 and evaluate early PEX and optional tracker discovery.
5. Profile productive load before introducing UDP sharding, a replacement filter or storage backend.

Use deterministic replay for lifecycle correctness, then repeated network comparisons with pinned versions, matched budgets and comparable indexing scope. Require sustained unique verified metadata/minute, searchable additions/minute, latency, resource cost and 24–72-hour stability. Published headline rates and short experiments cannot establish that Lodestar matches industry leaders.
