# X-sample-cooling-metric (runner brief, 2026-11-22Z)

Kind: core, model opus. Deps: X-sampler-frontier-starve and X-metrics-prefix-doc (both merged). CR-SAMPLE-BACKOFF-ESCALATE (#1029) is merged, so actor.rs is free.

## What
Export how many nodes the sampler is cooling and why, and how many samples have failed, so an operator, the soak tools and the A/B blocks can see the backoff at work.
- **A gauge, `dht_sample_cooling{reason}`:** nodes currently cooling. The reasons come from a fixed, bounded set:
  - `interval`: the normal re-sample wait;
  - `timeout`: a failed sample;
  - `unsupported`: a reply saying sampling isn't supported;
  - with the escalate switch on, the first (short) backoff versus the escalated one.

  Name the reasons from the code. Never take a label from network input.
- **A counter, `sample_failures`:** the actor already keeps it but doesn't export it. Follow the metrics naming that X-metrics-prefix-doc set.
- **`docs/metrics.md`:** both metrics, their labels and their units, in the contract's format.

## Step 0 (send to the runner before building)
1. Where the cooling state lives, and how the gauge is kept exact: updated on every transition, or counted at scrape time. Give the cost per scrape at the sampler's 200,000-place table. A scrape-time walk is fine only if it's bounded and cheap. Otherwise keep counts on each transition, with a test that the counts equal a recount.
2. The final reason list, with the escalate switch both off and on.
3. Changed files and an estimate.

## Rules
- **Owns:**
  - `crates/dht/src/node/actor.rs`
  - `crates/dht/src/node/sampler.rs` (if the state lives there; say so at step 0)
  - `crates/lodestar/src/metrics.rs`
  - `docs/metrics.md`
  - `docs/decisions/*-X-sample-cooling-metric.md`
  - this slice's own `slices.yaml` entry (refine its owns)
- **No behaviour change to sampling.** The rate and outstanding caps stay the same, and so does the switch's default.
- **Tests:**
  - unit tests for the counts, including across the escalate path and pruning;
  - a metrics-render test for the names and labels;
  - no fixed sleeps, and any wait at least 10 s.
- **Branch and PR:** branch `slice/X-sample-cooling-metric`, PR title "X-sample-cooling-metric: <title>". Push once per round, then send "ready <pr> <sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
