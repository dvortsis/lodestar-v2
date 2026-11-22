# X-sampler-cooldown-split decisions

The BEP 51 sampler keeps remembering who not to ask without that memory stopping it from asking anyone new (build-plan §0.5 "Path to 100", item 6; `docs/decisions/2026-11-20-path-to-100.md`). On the 2026-11-16 AWS crawl the sampler's 200,000-place table filled with 6 h timeout cooldowns about 35 minutes after suppression ended, and sampling then idled for about 5.5 h.

## The switch

- **`dht.sampler_cooldown_split`, a bool, off by default.** It's set in `config.toml` (`[dht] sampler_cooldown_split = true`) or with `LODESTAR_DHT_SAMPLER_COOLDOWN_SPLIT=true`, read at start, and a bad value is a config error (`sampler_cooldown_split_is_off_unless_asked_for`). It isn't in the Settings UI. The collector already records it (`docs/decisions/2026-11-20-CR-COL-5.md`).
- **Off is today's sampler.** No filter is allocated (`Option::None`), every call takes today's path, and the existing sampler, proof and actor tests pass unchanged. `below_capacity_the_switch_changes_nothing` checks that on and off answer every call the same while the table isn't full, so on changes behaviour only when the table would otherwise stop admission.
- **Unchanged either way:** the 200,000-place table (`restart::MAX_ENTRIES`), the per-IP and per-block shares (with the switch on, moved cooldowns keep counting against them; see "Shares"), the cooldown lengths, WK-10e's scale and clamp (`MAX_SCALE`, 48 h), the query rate and the outstanding cap.

## The structure

- **When the table is full and a node needs a place, the cooldown that ends soonest moves out** to a spill filter (`sampler.rs`, `spill::Spill`). In-flight records never move. Up to 16 of the soonest are tried, so one record that doesn't fit doesn't stop the rest.
- **The filter is a fixed table of 2^23 32-bit slots in blocks of 64.** Each slot holds a 20-bit tag of the address and the minute its cooldown ends, modulo 4,096 minutes (68 h).
  - **Where a slot goes:** an address may sit in either of two blocks, chosen by a SipHash with a per-process random key over its widest share (its IPv4 /24 or IPv6 /48). It goes in the emptier block.
  - **The tag:** 8 bits of that share's key, 4 bits of the IPv6 /64 (none on IPv4), 4 bits (IPv6) or 8 bits (IPv4) of the IP, and 4 bits of the address.
  - **One slot per cooldown;** nothing is merged.
  - **Why not a Bloom filter per time bucket (the brief's example):** a lookup has to ask every live bucket's filter, so false positives add up. With the 100-plus segments Balanced's steady state needs, most strangers would read as "maybe cooling" and sampling would stall again. Here a lookup reads two blocks. False positives run about 128 in 2^20 (0.012 %) for an address whose /24 or /48 has nothing moved, measured below 0.1 % at 90 % fill (`the_filter_is_bounded_keeps_every_key_and_rarely_misfires`). In a /24 holding its full 64 moved cooldowns, about 64 in 2^12 (1.6 %).
  - **Why minutes:** a moved cooldown ends at most 60 s late, against up to a bucket's width with coarse buckets.
- **The bound: 32 MiB, stated and measured.** The slots are one allocation of exactly 33,554,432 bytes, made on the first spill and never grown; the structure holds nothing else on the heap. The test asserts the bytes held. Capacity is 8,388,608 cooldowns; half full takes no refusal and 90 % refuses under 1 % (each refused cooldown stays in the table). `saturated()` treats the filter as full past 15/16 of its slots.
  - **Sizing, with WK-10e's scale:** live cooldowns ≈ sample rate × (answered share × interval × scale + unanswered share × 6 h). At Balanced (about 100 samples a second), with about a fifth answered at about 6 h:
    - at scale 1, about 2.2 million cooldowns are live (2.0 million beyond the table, a quarter of the slots);
    - at scale 8 (48 h cooldowns for answers), about 5.2 million (5.0 million beyond the table, about 60 % of the slots).
    - Aggressive (about 500 a second) can fill the filter. It then refuses, and the table behaves as with the switch off: no worse than today.
  - **The budget (`docs/architecture.md` §3, owner-decisions §1.6.3 and §1.6.5):**
    - **Main process:** steady-state RSS under 1 GB at 10M torrents with default caches. The sampler's table is about 35–45 MB at its cap (`docs/decisions/2026-11-02-R-mem-triage.md`); the filter adds 32 MiB, about 3 % of that budget.
    - **A worker running a DHT node** (§19, WK-10e) has RSS ≤ 256 MB at defaults. The switch is off by default there too. Turned on, table plus filter is about 75 MB at worst, so a worker that turns it on should check its soak RSS against that budget.
  - **Measured, and only when used:** with the switch off, nothing is allocated (`a_full_table_with_the_switch_off_waits_as_before` asserts 0 bytes). With it on, nothing is allocated until the first cooldown moves (`a_spilled_cooldown_never_ends_early_and_ends_within_a_minute` asserts 0 bytes for a filter that stored nothing). Then exactly 33,554,432 bytes are allocated, once (`the_filter_is_bounded_keeps_every_key_and_rarely_misfires`).
- **Ended slots are swept,** 8,192 slots a maintenance tick, so a full pass takes about 17 minutes.
  - An unswept ended slot would read as live again about 1,215 minutes (20 h) after it ends: minutes wrap at 4,096, and a live end may lie up to 2,881 minutes ahead. The sweep runs long before that.
  - If one ever did read as live, it would only skip a node and over-count a share.
  - `debug_assert!` checks that the filter's clock never runs before its base, which is the moment of the first move.

## Never shorter

- **A node in cooldown is never sampled before its cooldown ends.** The argument:
  1. The table is exact, as today.
  2. A cooldown moves to the filter with its end rounded up to the next whole minute.
  3. Every moved cooldown has its own slot until it ends, so a duplicate, a spoofed contact or a tag collision can only add a match (a skip), never remove one.
  4. The filter has no false negatives: a stored end is at most 48 h and a minute ahead (`HORIZON_MIN`), so it can't wrap while live, and an insert past that is refused and stays in the table.
  5. A refusal leaves the cooldown in the table. Nothing is dropped to make room.
  6. Admission (`offer`, `next`, `reserve`) asks the table and the filter.
- **WK-10e's scaled cooldowns** (up to 48 h) move like any other and keep their full length.
- **Tested:** `no_node_is_sampled_in_cooldown` (proptest, 512 cases) runs random offers, direct reservations of any address (as a spoofed or duplicate contact would make), answers with any interval and scale, aborts, prunes, restarts and up to 3 h steps, against a table of 1–5 places. No node is ever reserved before its true end, and the table and filter stay within their bounds. `a_full_split_table_keeps_admitting_and_refuses_cooled_nodes` and `a_full_filter_leaves_the_table_full` cover the edges.

## Shares (the runner's I1, 2026-11-20)

- **Moved cooldowns keep counting against their source's shares until they end,** as they do in the table with the switch off. The shares are `MAX_PER_IP` 8, `MAX_PER_V4_NETWORK` 64, `MAX_PER_V6_NETWORK` 16 and `MAX_PER_V6_ALLOCATION` 64.
  - **The bug this fixes:** freeing a share on the move would have let one IP, or one /24, take about 11× (Balanced) to 54× (Aggressive) its share of the sample budget.
  - **How the count works:** `Spill::shares` counts the live slots in the address's two blocks whose tag matches at each level: the IP, the /24 or /64, and the /48. `has_room` adds those counts to the table's.
  - **Never low:** every moved cooldown keeps its own slot in one of those two blocks until its minute passes.
  - **Sometimes high, which only skips:** another key in the same blocks with the same tag bits also matches. At the IP level that is 1 in 256 per neighbour (1 in 16 for an IPv6 /64 within its /48).
- **No extra memory:** it costs nothing beyond the 32 MiB slots. Each check reads two 256-byte blocks.
- **Tested:** `moved_cooldowns_still_hold_their_sources_shares` runs 2 h of a full table of 6 h timeouts, with a hostile source offering 5 new addresses a second. The hostile source is one IP with many ports, one /24, one IPv6 /64 and one /48 in turn.
  - With the switch off and with it on, each gets at most its share.
  - With the share counting removed, one IP got 29 samples against its share of 8.
  - The split still samples over ten times as many nodes in all.

## Occupancy (the runner's I3)

- **`dht_sample_spill_slots`,** a gauge of filter slots in use, of 8,388,608, so an experiment can see the filter near 15/16 (7,864,320), where the sampler falls back to its table alone. Its row is in `docs/metrics.md`, with a render test.

## Restart (CR-0d)

- **Which case applies (the runner's condition, 2026-11-20):** spilled cooldowns are **not** kept in the snapshot. A test shows instead that the restored state never samples a node earlier than today's restore would.
  - With the switch off, those cooldowns would have stayed in the table, and today's snapshot lists each one with its end, bounded by WK-10e.
  - With it on, the gate holds every sample until at least that same bounded end.
  - `no_node_is_sampled_in_cooldown` checks this against a reference that applies today's bound at each restart; `a_spilled_cooldown_gates_the_next_run_until_it_ends` checks it through `Proof::commit` and a real resume.
  - No on-disk format change.
- **The snapshot format doesn't change.** It lists the table, at most 200,000 entries, as today. The filter can't be listed, so on commit the proof takes the latest end the filter may hold (`Sampler::spill_end`, an upper bound on every live one), caps it at now + H as the snapshot's other entries are, and folds it into the snapshot's existing gate.
- **The next run restores the listed cooldowns and samples no one until that gate passes.** That honours every moved cooldown at least as long as today's snapshot would have kept it. WK-10e's argument still bounds a scaled one to `MAX_INTERVAL` from the commit.
- **The cost:** with the switch on, every clean restart after the table has filled waits up to about 6 h before sampling, as a run with a dirty marker does today.
  - In steady state the filter holds cooldowns ending across the next 6 h, so the gate is near its cap.
  - Today's sampler, restarted with a table that isn't full, has no gate, and a restored full table frees places as its cooldowns end.
  - **Plainly:** with the switch on, sampling is held after a clean restart once the table has filled, for up to about 6 h.
  - This time is gated, so `dht_sample_full_seconds_total` doesn't count it.
  - **CR-AB-1's handling (runner, 2026-11-20):** no restarts inside a block; where one happens, report the gated seconds (`dht_sample_suppression_seconds`, `dht_sample_suppression_reason`) beside sampler-full time.
  - **Follow-up (needs the runner, an on-disk format change):** persist the filter under the restart proof's commit, so a restarted run samples at once. Not built here.
- **With nothing spilled,** no gate is added (`a_spilled_cooldown_gates_the_next_run_until_it_ends`, both cases).
- **CR-0d's restart tests pass unchanged with the switch off.** The proptest's restart step does what `Proof::commit` and the start do: seal in-flight samples at now + H, list, gate, and restore into a fresh sampler.
- **A persisted filter** would let a restarted run sample at once. That's a new on-disk file under the restart proof's commit, which this slice doesn't own (`restart.rs`), so it's left as a follow-up if the live run shows restarts matter.

- **Monitoring (P5):** a restored record that moves to the filter counts as honoured when it moves, since the filter keeps it to its end. Offers the filter refuses for a restored address don't add to the refused count. Both are monitoring only.

## The metric

- **`dht_sample_full_seconds_total`,** a counter of seconds with no labels: time the sampler couldn't admit anyone while sampling was allowed (not paused, past suppression and any restart gate). It is checked once a maintenance tick (`Sampler::full_time`) and counts with the switch on or off. It's the plan's "sampler-full time" (cr-spec rule 6.4). Its row is in `docs/metrics.md`, with a render test.
- **`dht_sample_deferred_total` wasn't enough:** it counts deferred ticks, not time.

## The screen

- **CR-SIM can't model a run that fills the table in a reasonable test time.** #974's decisions say its 12-minute `live-lookup` run never reaches the 200,000-place cap. Filling it needs about 35 simulated minutes at the full sample rate, and the scripted network to hand out more than 200,000 distinct sampleable nodes. While the table isn't full, on and off behave identically, so the existing goldens can't move.
- **Instead, a deterministic dht-level test on tokio's paused clock** (`split_tests.rs`). It drives the real `Sampler` in the actor's order (prune, account, refill the frontier, send the tick's budget), and no sample is ever answered, so each one times out after 4 s and cools 6 h, as on 2026-11-16:
  - `a_table_of_timeouts_idles_the_sampler_off_but_not_on`, which runs in CI: a 2,000-place table, 10 samples a second, 3 h. With the switch off, the table fills in about 200 s and sampling stops for the rest of the run; with it on, sampling holds the full rate and the sampler is never full.
  - `full_scale`, ignored and run in release: the real 200,000-place table and Balanced's 100 samples a second for 8 h.

| Arm | Samples sent | Sampler-full time         | Samples per second |
| --- | ------------ | ------------------------- | ------------------ |
| Off | 400,000      | 24,801 s of 28,800 (86 %) | 13.9               |
| On  | 2,880,000    | 0 s                       | 100                |

- **This shows the mechanism only:** sampling continues and sampler-full time falls to zero, with the query rate unchanged. Whether more sampling stores more torrents a minute is for the live run: stage 3, CR-AB-1, reported on sampler-full time and its worst hours. A screen never replaces a live run.
- **If the runner frees `crates/crawler-sim/tests/sim/**`,** a filling scenario could be added there later. It needs a long-run knob and a large node population.

## Refinements of the plan (§2.19)

- **The filter's form:** a two-choice fingerprint table rather than per-bucket Bloom filters, for the false-positive reason above. Its budget is 32 MiB rather than "a few MB", so Balanced's steady state fits even at WK-10e's largest scale.
- **Restart:** the snapshot's gate covers spilled cooldowns instead of a new persisted file. With the switch on, that costs up to about 6 h of sampling after each clean restart once the table has filled. A persisted filter is the follow-up if the live runs show restarts matter.
