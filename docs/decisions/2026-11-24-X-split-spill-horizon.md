# X-split-spill-horizon decisions

With `dht.sampler_cooldown_split` on, runner records r17 saw the spill filter grow about 360k slots an hour on X-test-vm's split lane (2026-11-23) and extrapolated that it would reach its fallback, 15/16 of its 8,388,608 slots, after about 20 h. This slice states what the fallback does, makes ended cooldowns stop counting towards it at once, and measures how full the filter gets in steady state. It refines `docs/decisions/2026-11-20-X-sampler-cooldown-split.md` ("The structure", "Occupancy") and corrects one line of `docs/decisions/2026-11-24-X-split-resume-gate.md`.

## What the fallback does today
- **The fallback is the sampler's `saturated()`:** the table is full, and the filter has no room (`Spill::has_room`, under 15/16 of its slots). The actor then sends no sample that tick (`sample_tick` returns early and counts `dht_sample_deferred_total`), and `dht_sample_full_seconds_total` counts the time.
- **Admission comes back when a table place frees or the filter drops under its line,** whichever is first. A freed table place admits one node into the table, as with the switch off, and nothing more moves out until the filter's live count drops under the line. So at the fallback the sampler is throttled to the rate cooldowns end, not stopped: about 8.06M live cooldowns (filter and table) divided by the cooldown length.
- **It is safe, and needed no fix.** No cooldown is shortened and no node is sampled early: `saturated()` only stops sending, a refused insert leaves the cooldown in the table (`make_room`), and nothing is evicted to make room. `at_the_fallback_the_sampler_waits_and_nothing_ends_early` fills a real filter to the line and checks `saturated()` (on which `sample_tick` defers), the full-time count, that every spilled and every listed node stays refused until its own end, that a freed table place admits one node and the next waits, and that the filter's ended cooldowns free it.
- **What wasn't right was what it counted.** `has_room` judged slots in use, and an ended cooldown stayed in use until the sweep reached its slot, up to a pass (about 17 minutes) later. Inserts already reused ended slots, so the filter had more room than the fallback judged, and the gauge read high by the same amount.

## The ageing rule
- **A spilled cooldown stops counting in the tick its minute ends,** before its slot is swept. The filter keeps a count of used slots for each of its 4,096 end minutes (`Spill::ends`, 16 KiB, allocated with the slots). Each sweep recounts the live slots from it: the minutes from the first live one to `HORIZON_MIN` ahead, read exactly as `Spill::live` reads a slot. Each insert adds one. So the count is exact at every sweep and never low between them.
- **`has_room` and the gauge read live cooldowns,** not slots in use. The fallback's line is unchanged: 15/16 of the slots (`spill::FALLBACK`, 7,864,320).
- **The line is hard.** `make_room` checks it before each move, so one tick's samples can't carry the filter past it; before, it was checked once a tick. A cooldown that can't move stays in the table, as a refused insert always did.
- **Ended slots are still cleared by the sweep,** 8,192 a tick, and an insert takes an ended slot as readily as a free one, as before.
- **Nothing live leaves early.** An entry goes only once its own stored minute has passed, which is at or after its cooldown's end (rounded up to the minute, as before). No cooldown is shortened, so no node needs the spec's lost-record handling: none is ever lost.
- **Tested:** the proptest `ageing_never_frees_a_live_cooldown_and_counts_exactly` runs random inserts (crowded into a few /24s so blocks fill and refuse), steps of time up to 6 h and sweeps. Every stored cooldown is found until its end, the live count equals the stored cooldowns not yet ended at every sweep and is never lower in between, and a full pass leaves exactly those. `ended_cooldowns_stop_counting_before_they_are_swept` checks the count before any sweep reaches the slots, and an ended slot's reuse in a full pair of blocks.

## Steady state
- **The filter can't grow without bound.** Every cooldown that moves in ends within its cooldown length, and stops counting when it does. So in steady state it holds the cooldowns moved out over the last cooldown length, less the table's 200,000: at most sample rate × cooldown length. At WK-10e scale 1 the longest is 6 h.
- **r17's 360k an hour is the first 6 h.** The filter grows at the sample rate for one cooldown length, then holds level. The 20 h extrapolation doesn't hold at scale 1.
- **Measured** on the real sampler at the production table (200,000 places), every sample timing out and cooling 6 h, the fullest a given rate makes it at scale 1, read at every tick (`steady_state_memory`, release):

| Rate | Live cooldowns in the filter | Slots in use (ended included) | Filter heap | Sampler-full time |
|---|---|---|---|---|
| 100 a second (r17, 360k an hour) | at most 1,966,300, level from the 6th hour on (25 % of the line) | at most 2,012,673 | 33,570,816 bytes | 0 s in 24 h |
| 200 a second (2×) | at most 4,132,600, level from the 6th hour on (53 % of the line) | at most 4,222,074 | 33,570,816 bytes | 0 s in 24 h |

- **The memory bound:** the filter's heap is fixed once it's allocated, 33,570,816 bytes (32 MiB of slots and 16 KiB of end counts), at any rate. The table stays at most 200,000 records. Only the restart snapshot's spill section grows with the live count: 128 KiB plus 4 bytes a live slot, about 8 MiB at r17's rate and about 16 MiB at twice it.
- **In the test suite,** `a_day_at_r17s_rate_never_reaches_the_fallback` runs 24 h of paused time at 100 samples a second through the real sampler with a 2,000-place table. It is never full, sends at the full rate all day, holds level from the 7th hour within two minutes of samples, stays under a third of the line, and keeps at most a sweep pass of ended slots in use. It takes about 40 s on a debug build.
- **Why not CR-SIM:** the simulator can't hand out 100 new sampleable nodes a second for 24 h in test time (as `docs/decisions/2026-11-20-X-sampler-cooldown-split.md` "The screen" found for 35 minutes). The paused-clock test drives the real `Sampler` in the actor's order instead.

## Where the fallback is still reached
- **When sample rate × cooldown length is more than about 8.06M** (the line plus the table). At scale 1 that's above about 373 samples a second; Aggressive (about 500) reaches it.
- **With WK-10e's scale 8** (a fifth answered, cooling 48 h), the mean cooldown is about 14.4 h, so above about 155 a second. Measured:

| Rate, scale 8, over 60 h | Live cooldowns in the filter | Sampler-full time | Samples sent |
|---|---|---|---|
| 100 a second | grows for 48 h, then holds at about 4,988,300 (at most 4,990,300, 63 % of the line) | 0 s | 21,600,000, the full rate |
| 200 a second | reaches the line in its 32nd hour and holds there, never past it (at most 7,864,320 live, exactly the line, and 7,929,704 in use) | 32,399 s, all after the 31st hour | 36,581,043: about 135 a second once at the line, against 200 |

- **There it throttles, safely,** as above. Shortening or evicting live cooldowns to stay under the line is the only way to avoid it, and the brief rules both out. The split and WK-10e's scale are both off by default, so no shipped default reaches it. The runner's call on this is below.

## Unchanged
- **The split-off path:** no filter is made, and nothing in it changed. `the_split_off_day_is_unchanged` pins its day at r17's rate with a 2,000-place table: exactly 8,000 samples (one table a cooldown), full for all but about 80 s, nothing allocated. The existing split-off tests and the crawler-sim goldens pass unchanged.
- **The restart (CR-0d and X-split-resume-gate):** the snapshot's format and contents are unchanged. `restore` fills the end counts as it places each slot, so a rebuilt filter counts its restored cooldowns at once. Every restart test passes unchanged.
- **The crawler-sim restart check** reads the gauge, which now counts live slots only, so `held ≥ restricted` can no longer be met by ended slots (rv-1118's nit). Only its comments changed.

## rv-1118's items
- **Restored spilled cooldowns stay out of `restart_obligations_*`,** and a filter refusal doesn't add to `restored_refused`. The filter doesn't record which slots came from the snapshot, and once new cooldowns move in it can't tell them apart, so the counters can't keep `restored = pending + honoured` with them in. With the split on, `dht_restart_obligations_restored` counts the listed cooldowns only; the spilled ones are in `dht_sample_spill_slots`.
- **Keys are redrawn only when the sweep empties the filter.** With the split on in steady state the filter almost never empties, so keys restored from a snapshot can live across many restarts. This corrects X-split-resume-gate's "restored keys don't live forever". rv-1118's security judgment holds: the keys only spread addresses across blocks.
- **Not in this slice's paths, for the runner:** `restart.rs` `seed` clones the spill section instead of moving it, and `proof.rs`'s commit path calls `apply_seed(sampler, Instant::now())` where the rest uses `clock.mono()`.

## For the runner's metrics docs PR
- **`dht_sample_spill_slots`:** "Gauge, cooldowns: live cooldowns in the spill filter with `dht.sampler_cooldown_split` on (or restored from a snapshot with it off), of 8,388,608 slots (32 MiB, allocated when the table first fills or a snapshot's filter is restored); otherwise 0. A cooldown stops counting in the minute it ends, before its slot is cleared. At 15/16 (7,864,320) the sampler stops moving cooldowns out and admits only as table places free, so a value near that line means the filter is the limit. In steady state it holds about the sample rate times the cooldown length, less the table: about 2.0 million at 100 samples a second."
- **`dht_restart_obligations_*`:** add "Cooldowns the restart keeps in the spill filter (split on) aren't counted here."

## §2.19 refinements, for the runner to flag to the owner
- **The filter holds 16 KiB more** than the 32 MiB its decision stated, for the end counts.
- **The gauge's meaning changes** from slots in use to live cooldowns; its name, `dht_sample_spill_slots`, stays. Near the line the two differ by at most a sweep pass of inflow (about 100,000 at r17's rate).
- **The fallback engages on live cooldowns,** so it engages later than before by that same amount. No cooldown changes.

## Scaled cooldowns at the line (the runner, 2026-11-24)
- **They stay as they are.** At WK-10e's scale 8 and above about 155 samples a second, the filter reaches its line and the sampler throttles there, safely: about 135 samples a second at 200 offered, as measured above. No cooldown is shortened. Capping a scaled cooldown in the filter would shorten it, which the split's spec and WK-10e rule out, and any loosening is the owner's call.
- **Sizing the filter by scale** is a later performance call if fleet runs show the throttle costs much. Not in this slice.
