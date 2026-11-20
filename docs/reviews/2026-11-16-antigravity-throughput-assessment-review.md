# Review of the Antigravity throughput assessment

Reviewed branch state: `167c232af9bc8265e68ae221915f918e02c256b8`, including follow-ups `5fb8b349d`, `225da7259`, and `4370fcd9d` to the original review. These are the assessment changes present in the checkout for this request. This review supersedes conflicting recommendations in the three earlier review documents; they remain available as the reviewed record.

**The assessment improves the plan, but it does not demonstrate a path that will necessarily exceed 100 new verified searchable torrents/minute.** Keep coordinated admission, freshness, stage attribution and controlled conversion experiments as priorities. Correct the claims below before turning the assessment into implementation tasks. No production implementation or deployment was requested or performed in this review.

## Findings, ordered by effect on the plan

### 1. High: the proposed hint-state repair does not repair the described behavior

Opportunity A and experiment 3(a) in the [strategy assessment](2026-11-16-dht-throughput-strategy-review.md) propose `State::Skipped` to avoid routing damage and unblock termination. In [lookup.rs](../../crates/dht/src/node/lookup.rs), `on_failure` only settles a candidate in that lookup, decrements its in-flight count, and sets `State::Failed`. `slot_filter` already excludes `Failed`; `is_done` uses that filter. It does not change `RoutingTable::Entry::failures`. The hint guard branch in [actor.rs](../../crates/dht/src/node/actor.rs) never calls `table.failed`.

A separately named skipped state could improve diagnostics, but with the same exclusion semantics it does not recover a route or free any additional slot. The guard does reduce usable candidates across lookups; its skip count alone cannot establish the cause of a throughput regression. Do not narrow its scope or remove it without preserving the existing cross-lookup IP restriction contract.

**Useful alternative:** when a batch is skipped locally and no RPC is sent, immediately consider the next eligible candidates under a bounded local-work budget. Currently `drive_lookup` selects one batch and may defer remaining fresh candidates to `stalled` until the next maintenance tick. Test an all-guarded first batch followed by eligible farther contacts, ensuring no prohibited packet and no spin. This addresses actual scheduling delay rather than renaming an equivalent state.

### 2. High: the 67-minute queue argument is a risk scenario, not a measured p95 or invariant of this pool

`200,000 / 49.7 ≈ 4,024 s` is valid arithmetic for a full, homogeneous FIFO whose arrival rate is 49.7 distinct admitted items/s. The actual [pool](../../crates/sources/src/pool.rs) holds multiple states, protects interested/ready/fetching/permitted entries, rotates protected entries during eviction, handles retries, and uses separate admission queues. A lookup grant does not remove the entry from `Pool::entries`. Harvested-event rate is not necessarily distinct pool-entry arrival rate. The cited CR-0 run also predates enabling coordinated admission.

Consequently, this calculation does not prove that every hint expires or that successful-output p95 exceeds 600 seconds. Little's law relates mean population, arrival rate and mean residence for a consistently defined system; it does not determine a latency percentile. Keep the queue-freshness concern, but measure class-specific queue population, admitted arrivals, residence at grant/eviction, retry counts and successful-output latency. Apply CR-2's freshness/fairness design without treating the model as observed data.

### 3. High: 99.47% is total eviction/harvest, not the fraction evicted without service

The [CR-0 table](../soak/evidence/2026-10-30-cr0-final/cr0-final.md) reports 4,273,028 evictions, but `fetch_evicted_service_total` separates **4,224,261 unaccepted**, **9,543 lookup**, and **39,224 fetch** events. The assessment calls all 4,273,028 unattempted. Even the unaccepted/harvest ratio (about 98.33%) is an event ratio, not a matched distinct-hash cohort probability.

The upstream suppression concern is real but narrower: [harvest_hash](../../crates/dht/src/node/actor.rs) records sightings before `try_send`, and repeated sample/get-peers sightings return early; announces and lookup results still pass. [SeenFilter](../../crates/dht/src/node/seen.rs) rotates on accepted insertions, not elapsed time. Its estimated 5.6–11.2-hour residence assumes a sustained insertion rate, and false positives and `remember` renewals further limit that estimate. Instrument suppression after failed delivery/eviction; do not globally disable deduplication or count repeated observations as independent corroboration.

### 4. High: separate measured performance evidence from reference designs and security benefits

The additional-methods note expands three candidates to “seven proven methods.” A mature implementation establishes feasibility, not an isolated throughput gain. The two-stage timeout and subnet diversification proposals therefore belong in **reference-backed experiments**, not the measured-performance tier. No controlled benchmark for their individual improvement was established in the reviewed record. The pinned libtorrent files could not be independently fetched through the web tool in this review; their detailed line claims remain unverified here.

The [2011 Mainline paper](https://www.diva-portal.org/smash/get/diva2:436670/FULLTEXT01.pdf) measures the NICE maintenance/quarantine bundle, not quarantine alone or Lodestar's proposed replacement-only adaptation. Its latency ends at first values; neither metadata completion nor our occupied-permit time is measured. The [2014 report](https://arxiv.org/pdf/1408.3079) compares differently configured DHT systems, so the KAD-versus-MDHT gap does not isolate a zone-splitting patch. A multi-ID routing layout is also a separate hypothesis. Preserve those distinctions and do not multiply their headline improvements.

### 5. Medium: the timeout proposal must preserve two different counts

Method 5 suggests decrementing lookup and owner in-flight counters while leaving RPCs pending. That needs an explicit state machine: **progress-blocking requests** and **actual outstanding transactions** are different quantities. Each real transaction must retain an owner and consume the appropriate resource budget until reply, hard timeout or cancellation. A late response must settle each count at most once. Otherwise speculative dispatch can exceed intended per-request concurrency or corrupt completion accounting.

Specify bounded speculative slots, total sent-query cap, aggregate query rate, family fairness, late-response ownership and logical completion rules before coding. Add response/short-timeout races, repeated timer ticks, cancellation, both-family replies and hard-timeout-after-soft-timeout cases. The old network RTT distribution is insufficient to select a universal one-second threshold for this deployment.

### 6. Medium: subnet filtering is a coverage tradeoff, not an unconditional low-risk correctness fix

Exact endpoint duplication and many aliases from one IP can waste work. However, a hard one-contact-per-/24 or /64 policy can also exclude independent useful nodes, and exempting all initial seeds weakens the stated eclipse guarantee. Replacing fresh candidates requires carefully bounded provenance and candidate lifetime rules.

Test same-IP aliases separately from subnet caps. Measure collision prevalence, useful contacts rejected, timeouts and final yield; preserve a bounded fallback policy if strict subnet diversity exhausts candidates. Report any security benefit separately from a throughput effect. The current evidence does not demonstrate how much of Lodestar's traffic encounters the hypothesized attack.

### 7. Medium: tick scheduling is a useful opportunity, but its benefit and starvation claim are overstated

Accepted requests wait in `get_peers_queue` until maintenance, and throttled lookups can wait for `scrape_tick`. Immediate dispatch or a token-refill wakeup is worth testing. A mean 0.5-second saving assumes uniformly distributed arrivals and an on-time one-second tick; it was not measured. Equal packets per completed lookup also does not imply unchanged packets per second after throughput rises.

`maintain` already starts queued peer requests and calls `scrape_tick` **before** `sample_tick`; timeout-driven continuations are also handled earlier. Thus the blanket claim that the order violates the plan's continuation-before-sampling requirement is incorrect. Shared tokens and maintenance pings can still create contention. Measure queue-to-first-send delay by cause and query-purpose consumption; add bounded priority only where needed, keeping exploration live.

### 8. Medium: avoid premature routing CPU work and overgeneralized regressions

`RoutingTable::closest` collects **references** to entries, sorts them, then copies selected `NodeInfo` values; it does not clone every entry. A full-table sort may become worth optimizing, but profile first and preserve exact XOR ordering. Pairing it with short-timeout changes would confound attribution. Initial farther contacts are fallback candidates, not necessarily wasted wire queries.

The scheduler harness reproduced all ten expected counterexamples, but their implications differ. The group-commit case injects an external stale sequence into SQLite and demonstrates batch fault coupling; it does not establish normal single-owner frequency. The AS112/AMT case establishes divergence from the project's conservative network policy, not that every special-purpose address is globally unroutable. Hot-record saturation blocks admission of new endpoints; it does not prove all existing hot-node service stops. Detached-thread lifecycle and restart-clock safety deserve separate assertions. Keep positive acceptance tests separate from this historical harness that intentionally expects failures.

### 9. High for the goal: complete the capacity budget and acceptance definition

The owner-accepted Bitmagnet result is useful evidence that the host has supported a much higher **stored** rate. Its [recorded analyzer verdict remains invalid](../decisions/2026-11-08-cr-ab-comp-1-accepted.md), with an explicit owner exception for the checklist. Different sampling policy, indexing work and output definitions prevent treating it as proof that this pipeline can achieve the same rate within Lodestar's contracts. Likewise, 74.1% metadata timeouts do not identify NAT/transport as their cause; uTP remains an experiment.

Use a measured budget for every stage. For the lookup-dependent cohort, approximate saturated bounds are:

- Logical lookup service: `60 * L * y / W`, where `L` is occupied logical-slot capacity, `W` is mean full permit residence including queueing/failure/cancellation, and `y` is new searchable outputs per admitted lookup, assigned without double counting retries.
- DHT packet service: `60 * Q_available * y / q`, where `Q_available` is queries/s left after other traffic and `q` is mean wire queries per admitted lookup, including failures and speculation.
- Metadata service: `60 * S * p_new / T`, where `S` is occupied fetch-slot capacity, `T` is mean slot-seconds per attempt, and `p_new` is new searchable outputs per attempt rather than raw handshake or metadata success.

Direct-peer discoveries need separate attribution and consume downstream capacity too. Storage/index visibility and duplicate/exclusion losses must also be measured; the minimum applicable stage capacity constrains the result. These formulas are consistency checks, not forecasts.

For illustration only, `L=32`, `y=0.20` requires **W < 3.84 seconds** to exceed 100/min with no direct-peer contribution. With `Q_available=150/s` and `q=20`, the packet bound is only **90/min**, even if lookups finish faster. At the historical 4.38% raw fetch success, 100 successful fetches/min requires about **38 attempts/s**, versus the historical 4.71/s; new-searchable conversion can require more. More sockets or faster first peers alone does not establish that capacity.

The existing [CR-SPEC](../decisions/2026-11-02-CR-SPEC.md) acceptance gate is a valid 24-hour net mean **at least 100/min**, every hour at least 70/min, p95 at most 600 seconds and at least 98% latency coverage. The user's current goal is **over 100/min**: report strict exceedance separately rather than silently changing the frozen gate. A practical engineering target should include headroom above 100, chosen before the experiment; no particular margin has yet been measured or accepted.

## Revised execution plan

The subsequent [path-to-100 plan](2026-11-17-path-to-100-plan.md) expands these priorities into a capacity model, implementation boundaries and experiment decision rules. It retains the corrections above and does not claim the throughput target has been demonstrated.

1. Establish the effective baseline and remaining capacity in all stages. Record unique network-origin searchable output, loss stages, queue ages, full permit occupancy, packets/lookup, metadata slot-seconds and index lag. Preserve source provenance, intervals and restart proofs.
2. Complete the already-defined CR-ADM and CR-4 isolated live comparisons in the approved order when their run prerequisites are met. This review does not start those runs. Use CR-SIM to screen local lifecycle/scheduling work first.
3. Prioritize bounded freshness/fairness and removal of measured scheduling idle time. Keep the hint guard; drop the proposed throughput claim for a behaviorally equivalent `Skipped` state. Measure whether rejected sample evidence can be reoffered without an unbounded retry loop.
4. Select the next routing/timeout experiment from the measured limiting budget. Keep reference-backed and simulator-backed ideas explicitly experimental. Test changes separately before a predeclared interaction experiment; interactions require a recorded design extension because the frozen single-switch blocks hold other switches off.
5. Run the combined acceptance soak only after the chosen treatments and their lifecycle invariants pass. Report whether it actually exceeds 100/min and meets the other gates. If it does not, report the remaining measured bottleneck; further review or confidence is not a substitute for this result.

## Validation and review of this review

- Read the relevant actor, lookup, routing, sampler, seen-filter and pool state transitions and the CR-0, comparator, CR-SPEC and implementation-plan records.
- Reran `verify-proposal.ps1` with its pinned offline harness: original build failure reproduced; ten supplied tests and Clippy passed after mechanical borrow repairs; formatting failed as expected; two original and eight follow-up counterexamples reproduced. This is not a passing scheduler or a throughput test.
- Challenged the initial counter-review: retained queue freshness as a real risk while withdrawing a proven-p95 claim; distinguished candidate failure from routing failure; checked the actual eviction breakdown; recognized existing continuation-before-sampling order; retained quarantine as a measured bundle; separated causal evidence from implementations and simulation.
- Remaining uncertainty is explicit: no current live baseline, no measured combined gain and no guarantee of exceeding 100/min. The subsequent [research extension](2026-11-16-throughput-research-extension.md) records the additional primary-source search, conditional candidates and exclusions after review commit `3f6dbc3ba`.
