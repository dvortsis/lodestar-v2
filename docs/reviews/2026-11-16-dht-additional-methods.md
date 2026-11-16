# Additional crawler performance methods — 2026-11-16

Three routing-table techniques pass the screen for credible measured lookup improvements and no prior proposal or test found in this project's inspected records. None establishes an end-to-end Lodestar throughput gain. This extends the [strategy assessment](2026-11-16-dht-throughput-strategy-review.md) without changing production code or running network experiments.

## Primary evidence

Jimenez, Osmani and Knutsson, **Sub-Second Lookups on a Large-Scale Kademlia-Based Overlay**, IEEE P2P 2011: [university-hosted paper](https://www.diva-portal.org/smash/get/diva2:436670/FULLTEXT01.pdf), Table I.

| New candidate | Live Mainline DHT median lookup latency; standard / aggressive lookup |
|---|---|
| Prefer lower-RTT routing contacts | 510 → 459 ms / 284 → 185 ms |
| Enlarge distant routing buckets, after RTT preference | 459 → 286 ms / 185 → 164 ms |

The enlarged buckets hold 128/64/32/16 contacts, then eight. Maintenance increases. RTT preference worsened standard-lookup p99: 5,488 → 5,737 ms. The workload used 3,078 popular hashes; latency ends at the first response containing peers, not metadata completion. These are historical measurements, not contemporary forecasts.

Salah, Roos and Strufe, **A Lightweight Approach for Improving the Lookup Performance in Kademlia-type Systems**, 2014 [technical report](https://arxiv.org/pdf/1408.3079), §VI-C: maximizing within-bucket ID diversity reduced live **eMule KAD** mean hops 3.39862 → 3.23172 (about 4.9%). Mainline results were modeled/simulated, not live-tested.

## Why these are additional methods here

The audit covered checked-in DHT source/tests, the October 24 research, October 25 implementation plan, build plan, crawler decision/soak records and the earlier review, plus reachable Git history searched for the techniques and their terminology. No previous proposal, implementation or test of these specific policies was found. This cannot rule out experiments outside the repository or under substantially different terminology.

Current implementation evidence:

- [Routing table](../../crates/dht/src/routing.rs): `K = 8`; entries track contact health and times, with no RTT estimate. Full buckets retain good contacts, replace bad contacts or cache replacements; only the local-ID bucket splits. `closest` orders by XOR distance.
- [Lookup](../../crates/dht/src/node/lookup.rs): `ALPHA = 3`; candidates are selected by XOR distance. Its fanout differs from the research configurations, which limits direct transfer of the reported numbers.
- [Earlier research](../soak/2026-10-24-modern-dht-research.md) already covers aggressive lookup branching and other tuning. Changing which contacts a routing bucket retains is distinct from adjusting lookup deadlines, metadata peer racing or request concurrency.
- [Prefix traversal experiment](../decisions/2026-11-01-CR-1b.md) changes exploration targets. Within-bucket diversity changes the retained contact set used to reach any target; the already-tested traversal result does not test this policy.
- Larger routing buckets retain more forwarding contacts. They are separate from enlarging the BEP 51 cooldown ledger proposed in the original survey.

The history check included `git log --all -G` over routing/lookup code and research/decision/plan records for `NRTT`, `NR128`, `Kadabra`, proximity, identifier diversity, bucket diversity and low RTT. It returned no matching commits before this document was added. Broader source/document searches likewise found no relevant prior candidate beyond the already-known techniques above.

## Applicability and experiment order

**Start with nonuniform routing capacity.** This is the most direct initial experiment against the existing uniform-capacity implementation. Keep the peer-selection algorithm unchanged so the result isolates routing-table capacity. Define capacity from each bucket's actual prefix/range, including partially split tables; never infer it from a vector position that can change after splits. Separate internal capacity from response-size and lookup-termination constants rather than raising the shared `K` globally. Preserve endpoint validation, response verification, contact health and aggregate query limits. Charge any added refresh work to that same budget. The study's enlargement comparison already included RTT preference; testing enlargement alone here is a new hypothesis.

**Then test RTT-aware retention independently.** Collect timing from existing validated transactions and prefer healthy, consistently responsive eligible replacements. Account for retransmissions and local queue delay so the score measures useful network behavior. Use aging and replacement hysteresis; an instantaneous RTT win should not cause contact churn. Preserve ID/IP diversity and existing protections against unverified contacts. A conservative policy that only chooses among eligible replacements differs from unrestricted replacement of good contacts, so its gain needs measurement. Geographic concentration and tail latency are explicit rejection criteria, not just median speed.

**Keep bucket diversity as a smaller follow-up.** Measure current subprefix coverage first. Prefer verified candidates that fill coverage gaps when admitting or promoting contacts, while preserving stability and IP restrictions. This is lower-confidence for BitTorrent throughput because the live evidence is from KAD and measures hops. Do not claim a percentage latency or throughput improvement from a percentage hop reduction.

For each experiment, use the existing verified/searchable-output definition and compare at equal aggregate DHT packet budgets, logical lookup slots and metadata socket limits. Record first-peer latency separately from full occupied lookup time, timeout/no-peer outcomes, new verified inserts/min, bytes per new insert and routing maintenance traffic. Include cold/rare sampled hashes and unsuccessful lookups, not only popular swarms. Use matched repeated periods, with routing warmup and catalog growth handled explicitly. Preserve cooldown state across arms. No live experiment is authorized or performed by this research note.

These changes plausibly help when DHT lookup service limits admission. Faster first-peer responses cannot improve final throughput if failed metadata connections, duplicates or storage are the limiting stage. The earlier streaming screen already demonstrates that large first-result latency gains can translate into small stored/min gains. Existing coordinated-admission evidence remains the stronger local priority; these are additional experiments, not replacements for that work.

## Exclusions

Already-discovered approaches were excluded even when untested: trackers, PEX, IPv6, uTP/hole punching, hints, streaming, freshness/admission, shared lookup caching, deduplication, adaptive timeouts/concurrency, peer racing, socket/UDP batching and target-prefix traversal. Simulator-only or cooperative-overlay proposals were not promoted to the live-evidence shortlist. The list deliberately contains no claimed multiplier for verified torrents/minute.

Validation: source and experiment-history inspection, primary-paper/table checks and documentation diff checks only. No production tests were rerun for this documentation-only addition; no fleet, configuration or runtime was changed.
