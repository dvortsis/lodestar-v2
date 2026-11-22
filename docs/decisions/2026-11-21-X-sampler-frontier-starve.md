# X-sampler-frontier-starve decisions (#1022)

A lead from the X-test-vm sampler screen, with `dht.sampler_cooldown_split` on:

- the frontier sat at 0 for about 3.3 h, with every known node in cooldown;
- samples a second fell from about 100 to 48–58, and the reply ratio from 0.82 to 0.47;
- then it recovered on its own.

It was a works-as-designed run, so it's a lead, not A/B evidence. This slice reproduces it in CR-SIM and names the cause. **The cause isn't the sampler's, and the only lever is a cooldown's length,** so per the brief there's no fix here. The runner places it.

## Reproduction (CR-SIM)

The `base` network, made larger: 20,000 nodes, Balanced, 90 simulated minutes, seed 1. The rest is as `base` has it:

- 15 % dead;
- 60 % of live nodes BEP 51, with a 300 s interval;
- 2 % of datagrams lost each way;
- 0.5 % error replies;
- a remote rate limit of 20 queries a second per remote node (`net.rs:340`), which one crawler's sampling never reaches.

Run it with `cargo test -p lodestar-crawler-sim --test sim sampler_starve -- --ignored --nocapture` (about 21 s of wall time per arm, about 65 s in all; the test's doc comment says what to expect). It's ignored, like the sim's other measurements. Its per-minute sampling counters are read only by this test, so every other sim test and golden is unchanged.

Three arms:

- **A** has the split on.
- **B** has it off.
- **C** has the split on, but no loss, no error replies and no remote rate limit. The rate limit never binds in A either, so C's difference is the loss and the errors.

| Minutes | A: samples/min | B: samples/min | C: samples/min | A: frontier (mean) | A: cooling (mean) |
| ------- | -------------- | -------------- | -------------- | ------------------ | ----------------- |
| 1–10    | 2,912          | 2,912          | 2,950          | 2,001              | 15,673            |
| 11–20   | 1,544          | 1,544          | 1,729          | 0.3                | 18,514            |
| 31–40   | 1,287          | 1,287          | 1,720          | 0.4                | 18,513            |
| 51–60   | 1,092          | 1,092          | 1,728          | 0.2                | 18,611            |
| 81–90   | 860            | 860            | 1,732          | 0.4                | 18,734            |

Whole-run totals:

| Arm | Samples | Failed samples | Timeouts |
| --- | ------- | -------------- | -------- |
| A   | 122,923 | 14,762         | 63,384   |
| C   | 168,575 | 9,711          | 47,754   |

What the table shows:

- **The frontier is about 0 in every arm once the first pass is done,** and nearly every known node is cooling. On a network the sampler can cover, an empty frontier isn't by itself starvation: every candidate a reply or the routing table brings is sampled within a tick.
- **Starvation is the falling rate.** With ordinary loss, samples a minute fall steadily, from 1,544 to 860 over 80 minutes, and keep falling. Without loss or errors they hold at about 1,730. The only difference is about 5,000 more failed samples.
- **The split changes nothing here,** byte for byte (A = B). At 20,000 nodes the 200,000-entry table never fills, so nothing moves to the spill filter. So the sim can't exercise the split. That the lead isn't caused by it is inferred: the starvation appears here without the split doing anything, and the split only let the live run sample long enough to reach this state.

## The cause

**One unanswered or failed sample cools a node for 6 hours, so ordinary loss walks the live BEP 51 nodes into 6-hour cooldowns.**

How it happens:

- **A timeout:** `maintain` completes a timed-out sample with `SAMPLE_TIMEOUT_BACKOFF`, which is `sampler::MAX_INTERVAL` = 6 h (`crates/dht/src/node/actor.rs:100`, applied at `actor.rs:2919`).
- **An inconsistent or error reply:** this cools the node for `sampler::UNSUPPORTED_BACKOFF`, also 6 h (`actor.rs:1881`).
- **A node that answers is cooled only for its own interval,** here 300 s (`actor.rs:1865–1875`, `sampler.rs` `complete_scaled`). So it can be sampled again after 5 minutes at the soonest: once its cooldown ends, `prune` forgets it until a reply or the routing table offers it again. Each sample is a fresh chance of a loss or an error reply: about 4–5 % here (the remote rate limit doesn't bind).
- **The expected time until a live BEP 51 node's first failure** is therefore at least about 300 s ÷ 0.045 ≈ 1.9 h (a lower bound, since 300 s is the soonest it's sampled again). After that it's out for 6 h.
- **The useful nodes leave first.** Over a few hours most of them drop out, and sampling falls back on whatever fresh contacts replies bring, which skews toward nodes that don't answer. The reply ratio falls with it.
- **It recovers on its own** once the first wave of 6-hour cooldowns ends. That matches the live run's roughly 3.3 h at 0 and its recovery, given CR-0's live rate of unanswered queries (about 38 %, so failures come much sooner there than here).

### What it isn't

- **Not a lack of refill paths.**
  - Every non-ping reply offers up to 16 contacts (`NODES_PER_REPLY`), from samples and lookups alike (`actor.rs:1819–1827`).
  - Below 64 candidates, `sample_tick` offers the whole routing table and starts an exploration lookup per family each tick (`actor.rs:3121–3135`).
  - In arm C these keep the rate flat, at or above what the network's live BEP 51 nodes can give (about 10,200 nodes at a 300 s interval, or about 2,000 a minute, less those that time out).
- **Not the forgetting of ended cooldowns.** `Sampler::prune` drops an ended cooldown without re-queuing the node (`sampler.rs:349–366`), so it must be offered again by a reply or the routing table. Arm C shows rediscovery keeps up.
- **Not the spill filter's shares or false positives.** At this scale it holds nothing (A = B). A lookup compares an address against every live slot in its two 64-slot blocks, each match a chance of 2⁻²⁰ (an 8-bit key, 8 bits of the IP and 4 of the address). So a refusal that isn't owed runs about 128 in 2²⁰ (0.012 %) for an address whose /24 has nothing moved, and about 64 in 2¹² (1.6 %) in a /24 holding its full 64 moved cooldowns: the figures `2026-11-20-X-sampler-cooldown-split.md` states. Its shares only ever overcount (`Spill::shares`): a slot from another network matches the /24's 8-bit key about 2⁻⁸ of the time, against the /24 cap of 64. Both are far below the 4–5 % loss per sample.

## Why there's no fix here

The lever is the 6 h after a single failed sample, and the brief rules it out on three counts:

- **It's outside the sampler.** It's the actor's timeout and failure handling (`actor.rs:100`, `:1881`, `:2919`). The brief's owns are `sampler.rs` and `sampler/**`.
- **Changing it shortens a cooldown.** Possible changes include a shorter first backoff, or a retry before the 6 h. Either breaks X-sampler-cooldown-split's rule that a cooldown is never shortened (its "Never shorter" section).
- **CR-0d's restart proof assumes it in places.** `actor.rs:98–99` notes it ("CR-0: six hours, which the restart proof relies on"). The proof itself bounds restrictions from above, H = `MAX_INTERVAL` + `query_timeout`, with S8 sealing at now + H, so a shorter first backoff stays inside it. What would need re-checking: the sim's restart lower-bound model, which assumes send + 6 h for an unanswered sample (the README's restart scenario, `restart.rs`); and a per-node failure streak, which isn't in the snapshot, so a restart would reset it.

So the decision belongs to the runner, and to the owner if it changes a number the owner set. The runner will place the fix as its own slice.

## Proposal for that slice (not built here)

Escalate the backoff per node: a short first cooldown after one timeout, and the 6 h after a second in a row. It keeps the 6 h for nodes that really don't answer, and lets a node that dropped one packet come back.

- **It shortens a first cooldown.** So it needs its own switch, default off; it can't ride on `dht.sampler_cooldown_split`.
- **It needs a re-check against CR-0d's restart proof:** the sim's restart lower-bound model (send + 6 h for an unanswered sample) and the streak's loss on restart, as above.
- **It needs the runner's call before it's built.**

## A metric that would show it

Today:

- `dht_sample_cooling` counts cooldowns, but not why each began.
- `dht_sample_sent_total` shows the falling rate, but not the cause.
- The actor's `sample_failures` counter exists (`Stats::sample_failures`), but it isn't exported.

Proposed: **`dht_sample_cooling{reason}`**, a gauge of live cooldowns by why they began, with reasons `interval` (an answer), `timeout` and `failed` (an error or unsupported reply). A run in this state would show `timeout` and `failed` approaching the whole of `dht_sample_cooling`. Per the brief, adding it to `docs/metrics.md` waits for X-metrics-prefix-doc. The runner queues it, with the backoff change, as its own slice.
