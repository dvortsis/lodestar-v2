# X-wk-busy-metrics decisions

The coordinator counts the busy denials it sends, by cause. This is the metrics half of #637's busy retry. Brief: runner, 2026-11-08T23:15Z. The change of premise below is the runner's call, 2026-11-08T23:14Z.

## The premise changed: the coordinator counts, not the worker
- **The brief** asked for the worker's own counters (`busy_retried`, `busy_gave_up`, `busy_dropped`) as Prometheus metrics in `metrics.rs`.
- **They can't get there.** Those counters live in the remote `lodestar worker` process (`worker::runtime::Counts`), and that process serves no HTTP and no `/metrics`.
  - The coordinator's `/metrics` can't read them.
  - The only way a worker's counts reach the coordinator is the heartbeat's `proto::Stats`. It's postcard-encoded with no defaults, so a new field would be a wire change older peers can't decode.
- **The runner chose the coordinator side only:** no wire change, and no new listening surface on remote machines.
  - The coordinator counts the busy denials it sends. That's where a coordinator stuck on busy shows, and where the operator looks.
  - The worker's side (retried, gave up, dropped) stays in the worker's log: `worker.claims_gave_up`, once per streak.
  - Carrying the worker's counters in the heartbeat is logged as a possible follow-up, not queued.

## The metric
- **`cluster_claims_busy_total{cause}`:** a counter, per key. It sits beside the other `cluster_*` series in `crates/lodestar/src/cluster/serve.rs`, where every cluster series renders. That's the brief's "`metrics.rs`" in practice.
- **Exactly three fixed causes**, matching the three places the server answers busy:
  - `held_back`: grants held back. Either the admission hook allowed fewer new leases (the writer's queue at its high-water mark, §19.5), or too many commits happened during the store check (`claim_batch`'s forgotten window). It's the same count as the existing `cluster_busy_total`, which stays as it is so dashboards don't break.
  - `check_failed`: the batch's store check died or came back short, so every key in the batch was denied (#607). Counted per key. `cluster_store_check_errors_total` still counts the failed checks themselves.
  - `key_unknown`: the store couldn't read that one key (`KeyCheck::Unknown`, #628).
- **The names are this PR's, not the runner's first ones** (`queue_full`, `store_check`, `unknown_read`). The runner accepted the rename in #642's review. `held_back` is the accurate name, because `BatchAnswer::Busy` covers both the admission hook and `claim_batch`'s forgotten window (`leases.rs`), so `queue_full` would mislabel the second case. `check_failed` and `key_unknown` match `cluster_store_check_errors_total` and `KeyCheck::Unknown`.
- **No per-key, per-worker or free-form label.**
- **Counters are process-lifetime and summed over every opening of the port**, like the other cluster counters: turning workers off and on never sends them backwards.

## Also
- `crates/cluster/src/server.rs`: the stale `KeyCheck` doc line now says the worker claims a busy key again with back-off (#637).

## Tests
- `keys_held_back_are_counted_as_busy` (new, `crates/cluster/tests/transport.rs`, with an admission hook that admits one key): two keys held back, counted as `held_back`.
- `a_claim_whose_check_fails_is_denied`: one failed check, two keys counted as `check_failed`.
- `a_claim_denies_an_unknown_key_as_busy`: one key counted as `key_unknown`.
- `claims_busy_renders_each_cause` (`serve.rs`): the three series with their values, summed over two openings, and nothing else in the family.
- `claims_busy_reads_each_cause_from_the_snapshot` (#642's review): distinct snapshot values through `Totals::of`, so swapped fields fail.
- `every_family_is_documented` now counts 34 families, and finds the new row in `docs/metrics.md`.
- Mutation-checked: without the `check_failed` and `key_unknown` increments, their tests fail.
