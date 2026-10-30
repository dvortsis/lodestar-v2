# Crawler investigation and experiment protocol

Status: **specified**, pending the next integrated baseline (DUR-02–03, DUR-12).
This is an investigation model and candidate contract, not authorization to change
the running baseline. The interrupted run's approximately 16 hours establish leads,
not a complete reference or a measured improvement.

## Admission accounting model

Follow one entry lifetime through these boundaries:

1. Discovery is deduplicated against retained/recent entries. A missing key can
   enter, evict an idle key, or itself be lost when no existing entry is evictable.
2. A peerless entry waits for one of the pool's 256 lookup positions. A queued
   wait is not an accepted network attempt.
3. Each configured finder channel may accept, be full, or be closed. Multiple
   finders belong to one logical pool request.
4. The DHT actor admits at most 32 active+queued peer requests. Its rejection
   precedence is caller cancellation, pause, capacity. A lost acknowledgment is
   separate from those rejections.
5. The pool consumes one attempt when it observes the first accepted finder.
   Other accepted families do not consume additional logical attempts. Rejection
   leaves attempts unchanged; no-finder/fully rejected requests back off.
6. Timeout, terminal completion, successful fetch, eviction and shutdown release
   ownership. Eviction followed by rediscovery creates a new lifetime and loses
   the old lifetime's retry/backoff state. Neither event proves a new unique hash.

The 256/32 mismatch can expose rejected work, but lowering a number alone does not
establish a good solution. Arrival pressure, the speed of rejection/release, entry
eviction, rediscovery and genuine retries can all produce different patterns.
Existing aggregate observations cannot distinguish them retrospectively.

Prepare a deterministic replay with a two-entry pool and a finder that rejects:
send A, wait for rejection; send B, wait; send C, wait; send A again. Expected:
four actual finder requests, no consumed attempts, no entry-lifetime retry, and
two pre-service evictions. Contrast with retaining A through its retry deadline:
one later request, one retry-without-acceptance event, no eviction. A third replay
accepts A and holds completion while new arrivals fill the bounded pool. Assert
ownership drain after cancellation and shutdown in all three cases. These are
prepared scenarios; a prose description is not an executed reproduction.

The first two scenarios now execute in the pool regression
`rejected_rediscovery_is_distinct_from_a_retained_entry_retry`: A/B/C/A produces
four rejected requests, two pre-service evictions and zero lifetime retries;
retaining A produces two rejected requests, no eviction and one lifetime retry.
Both leave attempts at zero and drain finder ownership on shutdown. The focused
Windows pool suite passes all 18 cases (`target/rust-pool-r11.log`). This proves
the mechanism in a controlled fixture, not its contribution to the previous run.
The accepted-and-held contrast now passes in
`churn_evicts_an_accepted_lookup_and_cancels_its_ownership`: after observed admission
of A, rejected B and incoming C evict A; its terminal receiver closes, late peers
cannot arrive, the eviction is classified as after lookup, and ownership drains.
The updated pool suite passes all 19 cases (`target/rust-pool-r15.log`).

Read the next run's reason partitions with total rejections, finder full/closed,
unaccepted requests, retry-without-acceptance and eviction service stage. Account
for private-source deferrals and missing metrics. Never sum these nested boundaries
as independent failed torrents or infer a rejection cause absent from the data.

## Candidate backpressure contract

Keep existing resource caps. The candidate must coordinate bounded permission to
request ownership, not repeatedly ask a saturated actor with unbounded waiters.
Specify the permission unit (logical request versus address-family leg), maximum
waiters, wakeup/coalescing rule, pause/resume, closed-finder behavior, and which
owner returns permission at every terminal boundary. Cancellation while waiting
must use no network attempt and remove its waiter. Cancellation after admission
must release both family legs without a duplicate completion or leaked credit.

Do not grant new arrivals an indefinite advantage over retained waiting entries.
Specify bounded interactive bursts and a fair background queue before selecting a
policy. A receiver's newly available capacity must wake useful work promptly;
there must be no spin loop, permanent backoff after recovery, or retained permit
after eviction. A late acknowledgment from an old generation cannot attach to a
replacement entry with the same hash. Keep retries distinguishable from arrivals.

The design is incomplete until admission ownership, a full wakeup path and all
terminal/error paths can be drawn from the current pool/finder/actor interfaces.
Implement and measure one candidate only after baseline review selects it.

## Independent experiment records

| Candidate | Hypothesis | Primary evidence | Required guardrails |
| --- | --- | --- | --- |
| Admission coordination | Reduce rejected handoffs without starving retained work | Accepted requests, rejection categories, useful service before eviction, stored/searchable throughput | Ownership caps, queue fairness, cancellation and drain |
| Peer streaming | Start useful fetches before lookup terminal completion | Accepted batches before completion, successful conversion, fetch/lookup overlap | Reliable terminal completion separate from bounded batches, dedup, stale generations |
| Responder hints | Recent qualified responders reduce empty lookup work | Hint age/use/fallback and conversion | Public endpoint checks, TTL, bounded endpoints and shared resource accounting |
| Retention fairness | More cold entries receive useful service | Pre-service eviction and service distributions by defined workload class | Interactive burst bound, background progress, memory cap |
| Prefix/cooldown experiment | Selected targeting improves useful discoveries | Matched time windows, stored/searchable yield and cost | Separate gate; no baseline target/cooldown change |
| Transport change | A different peer transport improves conversion | Failure kinds, bytes, CPU and successful metadata | Independent protocol/security acceptance; no combined timeout tuning |

For each experiment freeze: hypothesis, source/binary/configuration identities,
changed variables, workload/network context, suppression/warmup policy, required
coverage, analysis exclusions, primary metric, guardrail metrics, and practical
improvement threshold. Choose the threshold from observed baseline variance and
operational value *before* running the candidate; do not select it after seeing a
favorable result. If variance or byte/CPU attribution is missing, record that limit.

Compare elapsed measured windows with per-metric coverage and hourly variation.
Report stored throughput, searchable throughput and completed-cohort yield
separately. Include invalid samples, resets, phase boundaries, interrupted periods
and unsuccessful work. Compare resource use alongside throughput; a higher stored
rate alone is not an efficiency result. A live network has time-of-day and peer
population variation, so even matched sequential runs cannot prove causality alone.

Keep a candidate separate until its own fast/medium tests and sustained acceptance
pass. Do not combine backpressure, streaming, hints, fairness and transport changes
into one result that cannot identify which change helped or regressed behavior.
