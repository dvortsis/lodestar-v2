# X-escalate-full-table decisions

The follow-up to the escalate gap in runner records r17 (`2026-11-24-runner-records-r17.md`). It corrects one line of CR-SAMPLE-BACKOFF-ESCALATE (`2026-11-21-CR-SAMPLE-BACKOFF-ESCALATE.md`), which stays as it is.

## The correction
- **CR-SAMPLE-BACKOFF-ESCALATE said:** "In the long run a dead node costs one extra sample per 6 h, a full table included."
- **That held only with `dht.sampler_cooldown_split` on.** With the split off and the table full, a struck node never got its 300 s second sample: it waited for a free place like a newcomer, so the switch did nothing on a full table.
- **After this slice it holds with the split off too.** Once a struck node is offered again after its short cooldown, it gets its second sample at once, full table or not, and a dead node costs one extra sample per 6 h either way.
  - **The sampler doesn't queue a struck node by itself.** It comes back the way any candidate does: in a reply that lists it, or the actor's low-frontier refill. With the split off and nothing held, `sample_tick` skips that refill on a full table, so on a full table the node comes back through replies. In the sim screen that took at most about 100 s more.

## The gap had three parts, not one
Confirmed on `main` at 32396391 before any change, by the sim screen below (escalation on gave the same run as off: no struck node sampled again).
- **`Sampler::next()`** returned `None` on a full table before it looked at the frontier, when `make_room` refused (always, split off). r17 named this one.
- **`Sampler::saturated()`** said the sampler couldn't admit anyone, so the actor's `sample_tick` returned before it called `next()` at all.
- **A full frontier** (20,000 candidates, stuck behind a full table) refused the struck node's offer, so it couldn't even queue.

## The fix: held candidates (switch on only)
- **With `dht.sample_backoff_escalate` on, a node that holds a table record waits apart, in `held`:** a streak whose short cooldown ended, or a cooldown that ended but wasn't pruned yet. Its sample replaces its own record (`reserve`'s existing `was` path), so it needs no free place. A node with no record still needs room, as before.
- **`next()` hands out a held node first,** full table or not. One that no longer holds a record (its streak ended, or a split table moved it to the spill filter) is offered again as a newcomer, to the frontier and its rules. One that may not be sampled is dropped, as the frontier drops one.
  - **The slice's refinement, for the runner holder to adopt and flag (§2.19):** the brief asked for this only on a full table, and for a streak. The slice also does it with room in the table, so a struck node's retry comes about 300 s after its failure rather than behind the whole frontier. It also holds a cooldown that ended but wasn't pruned yet, since its sample replaces its record in the same way. Both are behind the switch, default off, and a struck node's retry is at most once per streak.
- **`saturated()` is false while any node is held.** If every held one lost its record meanwhile, `next()` offers them again as newcomers and, on a full table, admits no one that call.
- **`held` has its own bound, `MAX_FRONTIER` (20,000), apart from the frontier's,** so a full frontier can't keep a struck node out. Each held address holds a table record, so it already counts against its IP's and block's shares; holding it adds none. At about 40 bytes a `NodeInfo`, that's under 1 MB at the bound, in practice far less.
- **`frontier_len()`** (the `dht_sample_frontier` gauge and the actor's low-frontier refill) counts held candidates too, with the switch on. And since `saturated()` is false while anything is held, `dht_sample_full_seconds` and `sample_deferred` count less with the switch on (1,045 to 1,004 full-table seconds in the sim screen). `docs/metrics.md` isn't this slice's to change, so the runner places those notes.
- **Unchanged:** rate and outstanding caps, every cooldown's length (nothing is shortened), the table's bound, the shares, the split's spill rules, the restart proof's snapshot.
- **With the switch off, `held` stays empty,** so every path is today's. The sim screen's switch-off arm pins `main`'s numbers from before this slice (380 samples, 1,045 full-table seconds).

## The sim screen
`escalate::a_struck_node_gets_its_second_sample_on_a_full_table` (`crates/crawler-sim/tests/sim/escalate.rs`, about 3 s): the base network (2,000 nodes, 2 % loss each way), 20 simulated minutes, split off, the sampler's table cut to 200 records so it fills in the first minute and stays full (about 1,000 s of `sample_full_seconds`).

| | Samples | Full-table seconds | Struck nodes due in the run | Sampled again |
|---|---|---|---|---|
| Escalation off | 380 | 1,045 | 65 | 0 |
| Escalation on, before this slice | 380 | 1,045 | 65 | 0 |
| Escalation on, after | 461 | 1,004 | 66 | 66, all 301–404 s after the failure |

The test asserts every struck node's second sample comes no sooner than 300 s after its failure and within 120 s after that; with escalation off, none is sampled again within the run.

## A test-only seam outside the slice's paths
- **`lodestar_dht::node::testing::SamplerCapacity`** (feature `testing`, never in a production build): a guard that sets the table's bound for samplers made on its thread. The sim runs the node on a current-thread runtime on the test's thread, so a guard in one test can't reach another. It lives in `sampler.rs`; `node/testing.rs` (itself compiled only with the feature) gains one `pub use` line for it.
- **The slice's `owns` widens to include `crates/dht/src/node/testing.rs`: runner holder's call, 2026-11-24,** on the conditions that the seam stays behind the `testing` feature and that no slice editing `node/**` runs beside this one. The PR updates its own `runner/slices.yaml` entry.
- **Why a seam:** the table holds 200,000 records, and the sim's networks have at most 20,000 nodes ("the 200,000-entry table never fills at this size", `sampler_starve`).

## Tests
- `split_off_a_full_table_still_samples_a_struck_node`, `a_full_frontier_doesnt_keep_a_struck_node_out`, `a_held_node_whose_streak_ends_is_a_newcomer_again`, `with_the_switch_off_nothing_is_held` (`sampler/escalate_tests.rs`).
- `shares_hold_under_any_sequence` now runs with escalation on or off, the split on or off, a small table or the full one, and failures. It checks that every queued address is in the frontier or held, exactly once, and that the table never exceeds its bound. With the split on, that covers a held node whose record moves to the spill filter.
