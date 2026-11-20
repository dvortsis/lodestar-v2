# Additional performance research after the assessment review

This search began after the assessment review was committed as `3f6dbc3ba`. It screened primary research and first-party engineering evidence against the current source, prior research and reachable Git history. **No newly found method proves that Lodestar will exceed 100 new verified searchable torrents/minute.** Two additional externally measured techniques merit conditional consideration; neither should displace the baseline, admission and conversion work in the [revised plan](2026-11-16-antigravity-throughput-assessment-review.md).

## 1. Selective retransmission of unanswered DHT lookup RPCs

**Evidence:** Liu et al., *Revisiting Why Kad Lookup Fails*, [author-hosted paper](https://ix.cs.uoregon.edu/~lijun/pubs/pdfs/liu12kad.pdf), Table II and §V. Live KAD experiments raised search success from **91.19% to 95.97%** using up to three retransmissions at a 1.5-second threshold. This is about **5.24% relative improvement**, not 4.78 times improvement. The paper's larger combined result also includes other changes and cannot be attributed to retransmission alone. KAD search success is not Mainline metadata throughput.

**Project novelty:** no selective wire-retransmission policy for DHT lookup RPCs was found. [Actor maintenance](../../crates/dht/src/node/actor.rs) takes expired pending transactions, updates failure state and advances the lookup. Retrying a locally throttled send is different: the original packet was never sent. uTP retransmission is also different: it protects the metadata transport, not a `get_peers` datagram. Earlier notes mention retransmissions as an RTT-measurement caveat, but contain no proposed DHT retransmission policy or test. Novelty is therefore about this specific mechanism, not the word “retry.”

**Applicability gate:** first separate unanswered queries by purpose and observed contact history. No response can mean packet loss, peer departure, filtering or overload; the aggregate DHT timeout ratio cannot distinguish them. A controlled replay can inject request loss and response loss independently from dead endpoints. A new same-endpoint transmission is useful only when its marginal value exceeds a different eligible candidate's query.

**Proposed local experiment, not the published implementation:** allow at most one additional lookup request to a recently responsive endpoint, within the existing logical deadline, total-query cap and aggregate packet budget. Keep sample RPCs out of this experiment: a lost `sample_infohashes` response may already have established a remote interval. Retain all hint/IP restrictions; if they disallow the retry, skip it. Do not retry bootstrap, announce or other purposes by default.

Define one logical attempt with bounded wire attempts. Preserve endpoint/ID validation and monotonic transaction IDs. If new transaction IDs are used, keep a bounded relationship so either valid reply can settle useful work exactly once; late/duplicate replies must not double-count peers, release permits twice or penalize the routing node after success. Specify cancellation and hard-deadline cleanup before implementation. No timer or retransmission should extend the lookup's lifetime indefinitely.

**Decision criterion:** evaluate new searchable output per minute, per packet and per occupied lookup-second at a fixed aggregate budget. As a simplified warning, if useful output/lookup improved by 5.24% but packets/lookup rose by more than 5.24%, a purely packet-limited pipeline would lose throughput. This arithmetic is not a forecast from KAD success to Lodestar output. Compare retransmission alone, fresh-candidate speculation alone, and their predeclared interaction only if individual tests justify it. Reject retries that merely amplify dead-peer traffic.

**Status:** new, externally measured reliability mechanism; untested here and **not proven to improve this crawler's throughput**. Worth a small controlled loss experiment if purpose-specific evidence shows opportunity.

## 2. Profile-guided compilation of the actual release executable

**Evidence:** the Rust compiler team's [PGO study](https://blog.rust-lang.org/inside-rust/2020/11/11/exploring-pgo-for-the-rust-compiler/) reports **10–16% shorter build times** across its repeated benchmark cases when optimizing the compiler. Those are CPU-workload measurements on one machine, not crawler results. The [rustc manual](https://doc.rust-lang.org/rustc/profile-guided-optimization.html) documents collecting representative execution profiles and rebuilding with them. This optimizes the produced executable; merely using a PGO-built compiler does not apply PGO to Lodestar.

**Project novelty:** searches found no PGO training workflow, `profile-generate`/`profile-use` configuration, recorded experiment or proposal in the inspected source, packaging, CI, documents and relevant reachable history. The root [release profile](../../Cargo.toml) sets debug information but contains no PGO workflow. This is distinct from profiling before a manual source optimization, mimalloc, or existing release optimization defaults. Environment-only build flags outside the repository remain unknown.

**Applicability gate:** do this only if per-thread/actor profiles demonstrate CPU service limiting the pipeline. Low whole-process CPU does not rule out one busy actor, but neither does a large routing table prove a CPU bottleneck. Record CPU/record, actor scheduling delay and instructions/cache behavior where available. If network waits, cooldowns or peer availability dominate, PGO cannot recover the missing service capacity.

**Proposed experiment:** train on a bounded representative replay covering successful and unsuccessful lookups, peer handshakes, metadata verification, duplicate filtering and store/index work. Validate on separate workloads, including rare/large metadata and failure-heavy traffic. Compare ordinary and PGO release binaries from the same source/toolchain/target, with the same runtime limits. Hold unrelated compiler changes constant. Keep instrumentation out of the measured optimized binary, preserve build provenance, and reject output changes or unrepresentative-profile regressions. Foreign-library compilation may need separate treatment; do not assume Rust PGO optimizes SQLite or every dependency.

**Status:** externally measured CPU-performance technique, no prior project experiment found, **low priority until CPU evidence justifies it**. Do not add the reported compiler percentage to any DHT gain or use it to claim a six-to-ninefold crawler improvement.

## Methods screened out

| Method/source | Why it is not an additional proven throughput recommendation |
|---|---|
| Multiple nearby lookup targets, Kang et al., [Why Kad Lookup Fails](https://www-users.cse.umn.edu/~hoppernj/kad.pdf), §5 | The reported Fix2 result improves replica-root coverage, changes publishing behavior as well as search, and uses roughly 1.45–1.5 times as many GET messages. A unilateral Mainline rescue lookup is a different experiment; a crawler cannot impose the study's publishing policy on remote publishers. Do not present the coverage percentage as crawler output or cost efficiency. |
| Neighborhood lookup, Liu et al., paper above | Its KAD benefit addresses separate routing/content phases. Lodestar already issues `get_peers` during iterative traversal; Mainline's combined response can carry peer values. The same phase-removal improvement is not available as a new patch here. |
| mimalloc, [Microsoft Research](https://www.microsoft.com/en-us/research/publication/mimalloc-free-list-sharding-in-action/) | Already configured in `crates/lodestar/Cargo.toml` and covered by memory/soak work, including the musl C allocator override. Excluded as already implemented/tested. |
| Automatic cooperative yielding, [Tokio documentation](https://docs.rs/tokio/latest/tokio/task/coop/index.html) | The actor already uses Tokio 1.54.0 sockets/channels and the runtime's cooperative machinery. Specific long synchronous loops may still need a profile and bounded work; enabling the runtime feature anew is not a new optimization. |
| Metadata request pipelining and TCP_NODELAY | Already present in `crates/peer-wire/src/fetch.rs`: four-piece initial reservation, then a request window up to 64; outbound TCP enables NODELAY. Generic pipelining advice would duplicate existing work. |
| BOLT, [Meta's production report](https://engineering.fb.com/2018/06/19/data-infrastructure/accelerate-large-scale-applications-with-bolt/) | Credible CPU-performance evidence, but the instruction-cache bottleneck, target/toolchain compatibility and value over a simpler PGO experiment are unestablished here. Retained as a screened source, not a second counted version of profile-based compilation. |
| Caching, larger/varied routing buckets, RTT preference, quarantine, shortlist diversity, timeout speculation, batching, sharding, trackers, PEX and transport changes | Already discovered in this project. Being untested does not make them newly discovered. |

## Search scope and self-review

Searches covered the seven-method note, earlier crawler surveys/plans/soaks, DHT and peer-wire implementation, release configuration, packaging and CI. Reachable-history searches used `git log --all -G` for PGO flags, profile-guided optimization, multiple targets, replica roots, retransmission, loss recovery and packet loss. Existing matches for the latter were RTT caveats rather than a wire-retransmission implementation. This is an evidence-limited novelty audit; inaccessible or unrecorded experiments are not ruled out.

I reviewed the findings again against four failure modes:

1. **Wrong denominator:** retained lookup success, replica coverage and CPU time as their actual outcomes; none is labeled verified torrents/minute.
2. **Hidden extra traffic:** added the fixed-packet-budget decision test; retry gains can lose to fresh-candidate work.
3. **Unilateral deployment assumptions:** excluded the KAD publishing/search bundle from the shortlist and did not transfer its result to Mainline.
4. **Double counting or existing work:** excluded allocator, pipelining, runtime cooperation and previously discovered routing techniques; PGO remains conditional, not a substitute for networking work.

The revised recommendations are internally consistent with those limits. They are not unanimous proof of future performance: only a valid combined live experiment can establish strict exceedance of 100/min while satisfying the existing latency, hourly-rate and protection gates. No production changes, live runs or new performance benchmarks were made during this research.
