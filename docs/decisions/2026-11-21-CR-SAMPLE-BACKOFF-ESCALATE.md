# CR-SAMPLE-BACKOFF-ESCALATE decisions (#PRNUM)

X-sampler-frontier-starve (#1022) traced the BEP 51 frontier starving to one rule: a single failed sample cools a live node for 6 h. This slice adds a per-node escalation, a short first backoff, behind a new switch that is off by default. With the switch off, behaviour is byte for byte today's: every sim golden is unchanged, and `fail` is `complete` for every backoff (`with_the_switch_off_fail_is_complete`).

## The switch
- **`dht.sample_backoff_escalate`, environment variable `LODESTAR_DHT_SAMPLE_BACKOFF_ESCALATE`, default off.** The runner fixed the name. It's wired as `dht.sampler_cooldown_split` is: a struct field, a default, an env parse and `dht_config()` in `crates/lodestar/src/config.rs`, `Config::sample_backoff_escalate` in `crates/dht`, and the sim's `Switches`. It's not in the Settings UI.
- **With it on:**
  - **A node's first failed sample in a row** (a timeout, an error reply other than 204, or an inconsistent reply) cools it for `FIRST_FAILURE_BACKOFF`, 300 s.
  - **Every later failure in a row** cools it for the existing 6 h.
  - **An answer ends the streak.** So does a 204, which cools for 6 h at once (see below).
  - **The streak is remembered** for 6 h after each failure's cooldown ends. A node that keeps failing therefore stays at one sample per 6 h, as today, after its first short backoff.
- **Rate and outstanding caps, `NODES_PER_REPLY` and every other limit are unchanged.**

## The first backoff: 300 s, from the sim screen
`sampler_starve` gains arm **D**: arm A (split on, 2 % loss each way, 0.5 % error replies, the remote rate limit) with the switch on. Each value is the same seed and network: 20,000 nodes, 90 simulated minutes.

| Minutes | A: samples/min | C (no loss): samples/min | D 300 s | D 600 s | D 900 s | D 1800 s | D 3600 s |
|---|---|---|---|---|---|---|---|
| 1–10 | 2,912 | 2,950 | 3,264 | 2,912 | 2,912 | 2,912 | 2,912 |
| 11–20 | 1,544 | 1,729 | 1,759 | 1,998 | 1,906 | 1,544 | 1,544 |
| 31–40 | 1,287 | 1,720 | 1,718 | 1,651 | 1,585 | 1,739 | 1,287 |
| 51–60 | 1,092 | 1,728 | 1,710 | 1,647 | 1,573 | 1,420 | 1,092 |
| 81–90 | 860 | 1,732 | 1,673 | 1,608 | 1,566 | 1,411 | 1,185 |

Whole-run totals. "Repeats" are samples after the first to nodes that never answered a sample with an interval, split by whether any reply came back from them.

| Arm | Samples | Failed samples | Timeouts | Never-answering nodes | Repeats (silent, errors only) |
|---|---|---|---|---|---|
| A | 122,923 | 14,762 | 63,384 | 10,177 | 0 (0, 0) |
| C | 168,575 | 9,711 | 47,754 | 9,711 | 0 (0, 0) |
| D 300 s | 169,916 | 19,977 | 67,563 | 9,730 | 3,358 (3,065, 293) |
| D 600 s | 164,357 | 19,694 | 67,116 | 9,729 | 3,357 (3,064, 293) |
| D 900 s | 159,489 | 19,503 | 67,142 | 9,732 | 3,360 (3,063, 297) |
| D 1800 s | 147,722 | 19,109 | 66,858 | 9,733 | 3,361 (3,068, 293) |
| D 3600 s | 133,180 | 18,434 | 66,607 | 9,728 | 3,353 (3,065, 288) |

What it shows:
- **300 s holds the frontier best:** about 1,670–1,760 samples a minute throughout, against C's 1,730 without any loss, and A's fall to 860. Each step longer holds less, and 3600 s barely helps within 90 minutes.
- **The repeat cost is the same at every value:** about one extra sample per never-answering node (about 3,360, nearly all to the 15 % dead). Each such node is asked once more, then sits at 6 h. So the value doesn't trade against repeats, and the shortest allowed one wins.
- **The runner's bounds hold:** 300 s is the sim's sample interval, never shorter than an answer's cooldown there, and under an hour (`the_first_backoff_is_within_the_runners_bounds`). In the long run a dead node costs one extra sample per streak, not per 6 h, because the streak is remembered across its 6 h cooldowns.
- **Reproduce:** `cargo test -p lodestar-crawler-sim --test sim sampler_starve -- --ignored --nocapture` (about 90 s; the other values by editing `FIRST_FAILURE_SECS`).

## A 204 cools for 6 h at once (runner's call under §2.26)
- **The call:** a KRPC 204 ("method unknown") to `sample_infohashes` completes with the full 6 h at once, and it ends any streak. It isn't a failure that escalates. A consistent reply without the BEP 51 fields was already an answer that cools for 6 h (`UNSUPPORTED_BACKOFF`), and that's unchanged.
- **Why, from the code:** a 204 is the node itself saying it doesn't do BEP 51. Asking again in 5 minutes gets the same answer. Loss can't make one, and neither can a busy node (that's a 202). Any other error (201, 202, 203) or an inconsistent reply may pass, so it escalates like a timeout.
- **Why, from the sim:** 40 % of the sim's live nodes don't do BEP 51 and answer 204, about 6,800 nodes here. Escalating them too (arm D at 300 s, with 204 counted as a failure) leaves samples a minute about the same (1,677–1,793 after the first 10 minutes). But it sends 6,105 more samples (176,021) and 6,349 more failed samples (26,326), and errors-only repeats rise from 293 to 6,659: every non-BEP 51 node asked twice, for nothing.
- **What it costs:** `sample_failures` still counts a 204, as it did.

## Bounded state: the streak lives in the sampler's table entry
- **No new map.** The streak is a flag on the record `crates/dht/src/node/sampler.rs` already keeps per node (`InFlight` and `Cooling`). When a failure's cooldown ends, the record becomes `Struck { forget }`. `Struck` means the node may be sampled and the streak is remembered until `forget`, 6 h later. Struck records sit in an index of their own (`struck`, one entry per record, soonest forgotten first), in place of the expiry entry they had while cooling. So every record is in exactly one of `expiry` and `struck`, or in flight.
- **Bounded by the existing caps.** A struck record holds a table place and its IP's and block's shares, as the 6 h cooldown it replaces did with the switch off. A full table drops the struck record forgotten soonest before it refuses anyone, with the split on or off. A struck record restricts no one, so dropping it costs that node at most one more short backoff, and the table is never fuller than with the switch off. `saturated()` counts struck records as room. A short cooldown moved to the spill filter (split on) loses its streak in the same way.
- **Memory per entry:** the record grows from 24 to 32 bytes (`a_streak_costs_no_more_than_the_record`), so 8 bytes more per table entry, about 1.6 MB at the 200,000-entry cap. A struck record's index entry (56 bytes plus the B-tree's overhead) replaces its expiry entry of the same size, so it adds nothing. With the switch off there are no struck records.
- **Gauges:** `known` counts struck records (they hold table places). `cooling` and `inflight` don't. `inflight` was `known − cooling` and is now `known − cooling − struck`, so with the switch on `dht_sample_inflight` stays the number of samples in flight. With the switch off the gauges are unchanged.

## Never shorter, except behind this switch
X-sampler-cooldown-split's "Never shorter" rule stands for everything else: a cooldown, once set, is never shortened, and no spill, seal, restore or capacity path shortens one. **This switch is the one deliberate exception, and only for the length a node's first failure in a row is given.** It doesn't shorten a cooldown already set. It sets a shorter one, because one lost datagram isn't evidence that a node is dead. A second failure in a row gets the full 6 h, as today. That's why it needs its own switch, default off, and doesn't ride on `dht.sampler_cooldown_split`.

## The restart re-check (CR-0d)
- **The proof's bound holds.** The restart proof bounds restrictions from above: H = `MAX_INTERVAL` + `query_timeout`, the first-start suppression, and S8 sealing in-flight samples at now + H. A 300 s cooldown is well inside H. The snapshot lists it with its true end (`restrictions`), and leg 2 honours it. Struck records aren't restrictions, so they aren't listed (`the_snapshot_lists_cooldowns_not_streaks`). Sealing ends a streak: a sealed record is a plain restriction.
- **The sim's restart model, with the switch on** (`restart.rs`, `earliest_resample`; the switch-off model is unchanged):
  - A failure cools for 300 s, unless the node's previous leg-1 sample failed too and was sent within 6 h of it. Then the sampler still held the streak, so 6 h.
  - A 204 cools for 6 h. The network now records whether a reply was a 204 (`SampleAsk::unknown`).
  - An unanswered sample sent less than a query timeout before the stop can't have timed out, so it was sealed: the stop plus 6 h.
  - That's still a lower bound. The sampler can only have escalated where the model says it didn't (a late answer the model reads as answered), never the other way. A dropped streak (a full table or a spill move) would break that, and the restart scenario's 2,000 nodes never fill the 200,000-entry table.
  - Unit-tested in `earliest_resample_follows_the_escalation`.
- **The restart with the switch on** (`restart_with_escalation_honours_the_bound`, no golden) passes every check the golden run asserts. No node was sampled before its bound, sampling resumed 1 s after the start, and every node due during leg 2 was sampled. 596 re-samples came within 6 h of a failure, so the switch was exercised.

| | Switch off (golden) | Switch on |
|---|---|---|
| Leg 1 samples (answered, errors, unanswered) | 2,979 (1,931, 654, 394) | 3,383 (1,987, 670, 726) |
| Restored, pending, honoured | 1,995, 1,048, 947 | 1,973, 982, 991 |
| Restricted at restart, free at restart (re-sampled) | 1,995, 5 (5) | 1,972, 28 (28) |
| Expiring in leg 2 (re-sampled), delay median / max | 947 (947), 11 s / 375 s | 990 (990), 12 s / 216 s |
| Sampled before expiry | 0 | 0 |
| Leg 2 stored in its first 60 min | 1,760 | 1,818 |

- **The streak isn't in the snapshot,** so a restart resets it. That costs at worst one extra short backoff per node after a restart: a node whose streak was live is asked once more 300 s after its restored restriction ends, before it escalates again. At the Balanced rate that's well under one restart's worth of samples. It never lets a node be sampled before a restored restriction, so it's acceptable, and it keeps the snapshot format unchanged.

## Not in this PR
- **The fleet stack's switch pass-through** (`infra/fleet/steps/vm/compose.yaml`, `block-lib.sh`) and the soak tools don't list the new variable. They're outside this slice's paths, so the runner places that (as X-fleet-switch-passthrough did for the others).
- **A cooling-reason metric** stays with X-sample-cooling-metric. With the switch on, `dht_sample_known` includes struck records and `dht_sample_cooling` doesn't. That slice's `docs/metrics.md` entry should say so.
