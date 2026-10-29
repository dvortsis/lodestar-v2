# Metadata throughput strategy

Recorded 2026-10-24, America/Denver. Status: proposals, not approved implementation or demonstrated speedups. No application/runtime changes.

This records the brainstorming following the [soak investigation](2026-10-24-v0.5.0-metadata-throughput.md). The owner requires throughput at least competitive with leading implementations. A modern, low-overhead stack provides headroom, but network strategy and scheduling must realize it.

## Benchmark context

[Bitmagnet's FAQ](https://bitmagnet.io/faq.html) reports 100–1,000 torrents/minute depending on resources, network and configuration. Lodestar's observed average was approximately 0.38/minute, a nominal 260–2,600-fold gap. This is context, not a controlled comparison or an independent verification of Bitmagnet's claim.

Proposed milestones: 100 verified metadata records/minute sustained as an initial qualification target, 1,000/minute as a stretch target. These are suggested engineering targets, not new owner-approved budgets or achievable-yield guarantees. Final acceptance should require matching or exceeding a pinned comparator under equivalent conditions.

## Prioritized proposals

1. **Preserve sampling-node provenance.** BEP 51 samples represent hashes for which the responding DHT node stores peer values. Keep that node's endpoint and observation time with each hash, ask it directly for peers, and use bounded iterative discovery as fallback. The node is a DHT contact, not necessarily a metadata-serving peer. Bitmagnet retains this association in [sample_infohashes.go](https://github.com/bitmagnet-io/bitmagnet/blob/main/internal/dhtcrawler/sample_infohashes.go) and uses it in [get_peers.go](https://github.com/bitmagnet-io/bitmagnet/blob/main/internal/dhtcrawler/get_peers.go). Lodestar currently loses the endpoint in `crates/dht/src/node/actor.rs`'s sample handling and the `Harvest` structure in `node/mod.rs`. This is the strongest newly identified architectural candidate for reducing network work. Sampled entries may be stale; direct-query failure must have a fallback. See [BEP 51](https://www.bittorrent.org/beps/bep_0051.html).
2. **Repair forward progress and request lifetimes.** Fix stalled ordinary lookups; provide acceptance/start/completion/empty/error outcomes, deadlines and cancellation. Eviction must cancel obsolete queued work. Coalesce duplicates. Distinguish queue wait from network execution. Raising limits without fixing leakage merely delays degradation.
3. **Stream peers immediately.** Send newly discovered peers to metadata fetching as responses arrive, instead of waiting for full iterative traversal. Stop redundant lookup/fetch work after verified success.
4. **Coordinate stage budgets and admission.** When sockets are idle and peerless work accumulates, prioritize peer discovery; when fetch capacity is occupied, reduce peerless admission. Adjust concurrency to observed latency/loss and useful conversions. Prefer fresh peer evidence, retain exploration capacity, and expire stale work. Larger queues do not create service capacity.
5. **Reduce dead-peer waiting.** Use recent endpoint outcomes, separate connect/handshake/transfer deadlines, bounded delayed second-peer attempts when capacity is available, cancellation after first success, and expiring failure caches. Benchmark hedging against sequential fetching; duplicated connections can harm aggregate throughput.
6. **Complete transport and reachability coverage.** Wire application-level uTP, assess IPv6 and inbound DHT reachability, and measure transport selection. Consider tracker peer discovery and streaming PEX afterward. Their gains are unmeasured and do not fix starvation by themselves.
7. **Optimize CPU/storage only after productive networking.** Profile parsing, allocations, batching, persistence and indexing under realistic load. The current writer keeps up with successful fetches. Metadata-piece requests already use a 64-request window, so generic advice to add pipelining would duplicate existing functionality.

## Scheduling signals

| Observation | Proposed response |
|---|---|
| Idle fetch sockets and a peerless backlog | More peer-finding budget, less sampling |
| Fetch capacity occupied | Bound additional peerless admission |
| Rising timeout/loss with increased concurrency | Reduce concurrency |
| Fresh peer-bearing announcement | Prioritize the work |
| Repeated failure without new evidence | Bounded cooldown |
| New peer evidence for an unsuccessful hash | Reconsider promptly |

Keep content-safety verification, hash verification, per-IP limits and bounded memory. Score sources by useful verified completions per network/CPU cost, not hash count alone.

## Validation before claiming parity

- Controlled replay: identical hash/contact inputs and scripted peers, with dead addresses, packet loss, throttling and malicious input. This isolates implementation efficiency.
- Repeated real-network comparisons: pinned versions, matched hardware/network allowance, comparable processing/filter scope and initial corpus. Alternate runs to reduce competition and time-of-day bias.
- Measure unique verified metadata/minute and searchable additions/minute separately; CPU, memory, bytes/record, p50/p95 discovery-to-search latency and 24–72-hour stability. Separate new records from rediscovery, and cold-start from steady-state performance.
- Acceptance includes no progressive loss of lookup capacity, obsolete work or unbounded queue growth. Published competitor numbers alone are not acceptance evidence.

Illustrative capacity calculation: 100 successful fetches/minute at a 7.5% attempt success rate requires about 22.2 attempts/second. With an assumed four-second mean attempt duration, Little's-law occupancy is approximately 89 concurrent attempts. Neither the future success rate nor mean duration has been measured; this only demonstrates why feeding the existing 400-socket default may matter more than raising it.

Recommended first implementation package: lookup progress/cancellation, sampling-node provenance, direct peer requests, early peer delivery and stage telemetry. Tune concurrency after measuring that package. Follow-up research is recorded in [modern DHT techniques](2026-10-24-modern-dht-research.md).
