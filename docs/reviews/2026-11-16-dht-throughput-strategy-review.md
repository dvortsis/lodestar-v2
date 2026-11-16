# DHT throughput strategy assessment — 2026-11-16

**Yes: improving admission, freshness and peer conversion can plausibly produce a material throughput gain. The strongest local evidence favors coordinated admission. The proposed sampling scheduler alone has a much weaker case, and 100 newly verified searchable torrents/minute remains an unproven target.** This is the strategy follow-up to the [crawler survey and scheduler review](2026-11-16-dht-crawler-survey.md). It incorporates the earlier review findings in the same review-branch commit; no production source, configuration or runtime was changed.

The assessment below distinguishes recorded experiments, arithmetic and proposed experiments. The existing simulation screens and historical soak were inspected, not rerun here. The scheduler counterexamples were reproduced offline. No current process configuration or live throughput was measured.

The follow-up [additional methods research](2026-11-16-dht-additional-methods.md) identifies three routing-table techniques with credible external measurements and no prior proposal or test found in the inspected project records, while distinguishing lookup gains from crawler throughput.

## Evidence that changes the priority order

The original survey acknowledged existing hints and streaming code but omitted their checked-in performance screens. That omission made the ranking too optimistic about hints alone. The repository already contains a stronger basis for choosing the first experiment:

| Candidate | Recorded simulated stored/min change | Interpretation |
|---|---|---|
| Coordinated admission, including protection of admitted lookups from eviction | +26.1% high latency; +28.7% admission pressure; +30.1% pressure at production pool defaults; +49.5% healthy | Strongest local signal. Every paired seed improved in all four screens; changes exceeded the documents' baseline-range criterion. |
| Responder hints alone | −3.7% to +5.1% across three scenarios | Inconclusive. About 94% of hints expired unused, and about half of found hints were throttled. |
| Streaming peer batches | +2.9% to +7.7% | First fetch started 4.6–16 times sooner, but conversion barely changed. Healthy improved in all eight seeds; throughput changes did not clear the documents' range criterion. Latency gain is not the same as throughput gain. |
| Bit-reversed target-prefix traversal | −1.0% to +4.9% | Inconclusive. More exploratory contact novelty did not establish more stored torrents. |

Sources: [CR-ADM screen](../decisions/2026-10-30-CR-ADM.md#cr-sim-screen-switch-on-against-off-screen_seeds-18), [hints screen](../decisions/2026-10-30-CR-1a-H.md), [streaming screen](../decisions/2026-10-31-CR-1a-S-2.md), [prefix screen](../decisions/2026-11-01-CR-1b.md). These screens run the real actor and pool against scripted peers. They model IPv4, approximated routing and no NAT/churn; “stored” means reaching the writer, not verified searchable persistence. They are evidence of mechanism, not forecasts for this VPN. The documents' min–max “noise spread” rule is a screening heuristic, not a confidence interval; assess paired effects and uncertainty in a new experiment. [Simulator limits](../../crates/crawler-sim/tests/sim/README.md)

The [later CR-0 baseline](../soak/evidence/2026-10-30-cr0-final/cr0-final.md#baseline) is also more useful than extrapolating from the broken October 24 run. At commit `744b84d68c7609d8220430ffc9938c05c6d6a527`, it recorded 17,783 writer stores over 86,440.4 covered seconds: **12.34 stored/minute**. It recorded 407,475 fetch attempts and 17,829 successes, about **4.38% success per attempt**. Sampling state was full in 1,284 of 1,428 baseline observations, while the run also recorded very large capacity-refusal and unaccepted-request counters. Repeated refusals are not distinct hashes or wire traffic. This supports investigating both admission churn and sampler capacity; it does not attribute an exact fraction of lost yield to either.

That baseline predates the candidate screens, is not today's runtime, and explicitly does not prove searchable yield. Reaching 100/min would require roughly **8.1 times its stored rate**, before accounting for the different outcome definition. The evidence supports pursuing a substantial improvement; it does not establish an eightfold gain.

## Where the gain could come from

1. **Coordinate admission and keep admitted work alive long enough to finish.** Remove repeated submissions to a full finder, distinguish waiting from an attempted lookup, and preserve cancellation/deadline ownership. This reduces wasted local work and premature backoff without spending more network budget. It is already implemented behind `dht.coordinated_admission`, off by default. Confirm the deployed binary and effective switch before counting it as new work. The recorded first version actually collapsed from 29.8 to 0.3 simulated stores/min when FIFO admission selected entries that oldest-first eviction immediately removed; the corrected screen includes bounded protection for permitted lookups. A “small fresh queue” must retain that protection. [Admission design](../decisions/2026-10-30-CR-ADM.md)

2. **Spend scarce lookup service on fresh, actionable evidence.** Keep a small ready set and a bounded deferred set; preserve source, observation time, hints, peer batches and attempt state. Give exploration and old/rare hashes an explicit fair share. Novel hashes alone are a poor productivity score: rank sources by eventual new verified output per query or occupied lookup-second, using bounded aggregation and delayed attribution. A token-valid announcement or syntactically valid peer address is still untrusted evidence of a live swarm. Preserve endpoint policy checks and cap work contributed by one source.

3. **Make hints usable before expecting hints to help.** Existing hint TTL and destination-IP spacing are both 60 seconds. A sample containing many hashes from one responder cannot give every hash an immediate hinted query; ordinary routing also honors that IP restriction. Select a bounded representative fresh candidate per responder and measure capture-to-admission age, expired hints, guard skips, peers returned and verified output. Keep the guard. Simply enabling hints on an aging queue reproduced little benefit in the existing screen. BEP 51's stored-key association supplies a sound reason to try the responder, but no guarantee of a reachable metadata peer. [Hint contract](../decisions/2026-10-30-CR-1a-H.md), [BEP 51](https://www.bittorrent.org/beps/bep_0051.html#sampling-rpc)

4. **Relieve the next measured limit.** If cooldown capacity blocks useful discovery while lookup service is hungry, expand safe retention incrementally. If admitted lookups hold all slots, reduce occupancy or test bounded concurrency under the same aggregate query budget. If metadata sockets are busy with failures, test timeout policy and selective peer racing. If sockets are idle, adding sockets or racing peers cannot supply missing candidates. Streaming can overlap stages and sometimes cancel a successful lookup earlier, but does not itself increase the 32-request actor cap.

## The missing lookup-capacity calculation

For work that requires a DHT lookup, an approximate saturated steady-state bound is:

`new searchable/min ≤ 60 × active logical lookup slots × new searchable yield per admitted lookup / mean occupied lookup seconds`.

The current actor allows **32 logical peer requests**; its deadline is 25 seconds. The pool's 256 logical slots and 400 metadata sockets do not override that actor limit. Mean occupancy includes terminal failures and cancellations, not just successful lookups. Direct announced-peer work can bypass this stage, so measure it separately. [Actor limits](../../crates/dht/src/node/actor.rs), [request capacity](../../crates/dht/src/node/mod.rs)

| Illustrative mean occupancy | Illustrative new searchable yield per admitted lookup | Lookup-stage capacity at 32 continuously occupied slots |
|---|---|---|
| 25 s | 20% | 15.4/min |
| 8 s | 20% | 48/min |
| 5 s | 30% | 115.2/min |

These are scenarios, not predictions. At 20% yield, 100/min requires mean occupied time at most **3.84 seconds**, or more lookup capacity / work bypassing lookups. Even perfect yield cannot reach 100/min if all 32 slots remain occupied for 25 seconds. Conversely, a 25-second deadline does not mean every request lasts that long. Measure the mean and tail before deciding to raise the cap.

This makes the strategy concrete: increase *useful lookup completions and their conversion*, alongside the socket-occupancy calculation in the original survey. Do not multiply isolated percentage gains; the stages share bottlenecks and may benefit the same hashes.

## What the proposed scheduler can and cannot contribute

Separating the safety ledger, ready candidates and productive-node retention is sensible. Preserving cooldowns while dropping low-value scheduling work can sustain discovery after the current ledger fills. However, the current sampler already uses an indexed `BTreeSet` with one expiry entry per restriction; a heap or timing wheel is not automatically an algorithmic improvement. The proposal adds SQLite writes and potential heap-compaction pauses, so benchmark its admission latency and memory/disk use against the existing implementation. [Current sampler](../../crates/dht/src/node/sampler.rs)

Under the simplified assumption that every new endpoint occupies a record for exactly six hours, 200,000 records sustain 9.26 new endpoints/s. The proposal's 25/s sample cap would need about **540,000 live restrictions**, plus queued/in-flight state, hot retention and headroom. Four million is a capacity choice, not a demonstrated requirement. Increasing the table twentyfold does not imply twentyfold sampling: 25/9.26 is only about **2.7 times** the old simplified steady admission bound, and even that does not predict metadata yield. Fleet scaling can lengthen retention; retained hot nodes, failed cleanup and database/WAL growth need separate limits.

Productive-node preference should decay after failures and account for duplicates and downstream conversion. Historical novelty can keep unreachable nodes “hot”; concentrating on familiar productive nodes can reduce discovery diversity. Retain exploration capacity and exact live restrictions independently. The scheduler's default 25/s and per-IP buckets also require explicit composition with all other DHT traffic and existing fleet guards; they are not a replacement for the aggregate limiter.

The [four reproduced scheduler counterexamples](dht-survey-evidence/verification.md) are integration blockers. Fixing them establishes correctness, not a speedup. Prefer improving admission and evidence handling in the existing pipeline before a wholesale scheduler replacement.

## Experiments that can justify implementation choices

1. **Establish the current baseline.** Record binary commit/hash, effective opt-ins, reachable address families, VPN egress, restored queue/catalog state and restart suppression. Measure new verified searchable inserts/min, attempt success, duplicates, lookup occupancy and yield, waiting age, hint expiration, sampler-full time, packet rate, socket occupancy, RSS and WAL growth. Give startup/warmup a separate window.
2. **Screen coordinated admission first if it is not already enabled.** Hold other settings and aggregate network limits fixed. Its predicted signature is fewer refused/unaccepted submissions and more useful completed service, not merely fewer log messages. If it is already enabled, use it as baseline and avoid claiming its existing gain again.
3. **Test freshness, hints and streaming with attribution.** Start with isolated changes, then a predeclared 2×2 freshness × hints experiment to test the specific interaction: fresher admission should let hints execute before expiry. Add streaming as its own comparison. Preserve anti-starvation rules and admitted-work ownership; record per-source outcomes and total output.
4. **Change one limiting resource next.** A retention experiment should reduce sampler-full time and increase useful output at a matched query budget. A lookup-capacity experiment should increase completed useful lookups without worsening timeout/drop rates. A timeout/race experiment should improve verified yield per socket-second and per byte, not just first-success latency. Stop escalating a resource once another stage limits the result.
5. **Validate the selected combination over multiple cooldown cycles and a restart.** Use repeated matched periods, such as ABBA, with a predeclared practical improvement threshold and uncertainty estimate. Control catalog-growth/deduplication and carryover between arms; ABBA alone does not remove them. Never reset cooldown protection to equalize an arm or run independent same-egress workers that bypass shared restrictions. Compare against the unchanged baseline under the same budgets. A sustained 20–30% improvement would be material even if 100/min remains out of reach; this is a proposed decision threshold to set against baseline variance, not a forecast.

Pursue the conversion strategy, beginning with the strongest admission evidence, while treating the replacement sampler and the 100/min target as hypotheses to test.
