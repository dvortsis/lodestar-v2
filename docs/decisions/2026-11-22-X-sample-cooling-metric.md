# X-sample-cooling-metric decisions (#PRNUM)

X-sampler-frontier-starve (#1022) proposed a gauge of the sampler's cooldowns by why they began. CR-SAMPLE-BACKOFF-ESCALATE (#1029) added the first failure's short backoff. This slice exports both, and the actor's failed-sample count. Sampling behaviour doesn't change.

## Names (the runner's calls)
- **`dht_sample_cooling_reason{reason}`, a new gauge family.**
  - The old unlabelled `dht_sample_cooling` stays as it is, because the soak tools read it as one series (`docs/soak/tools/linux/collect.py`, `analyze_cr.py`, `analyze_cr0.py`).
  - A label under the same name would have broken them, and would have left an unlabelled series beside labelled ones.
  - The reasons sum to `dht_sample_cooling` exactly.
- **`dht_sample_failures_total`**, the counter, named as `dht_sample_sent_total` is (X-metrics-prefix-doc).

## The reasons: a fixed set, named from the code
| Reason | Where it begins | Switch off | Switch on |
|---|---|---|---|
| `interval` | `Sampler::complete_scaled`: an answer with the BEP 51 fields | yes | yes |
| `unsupported` | `Sampler::complete_unsupported`: a consistent reply without the BEP 51 fields, or a KRPC 204 | yes | yes |
| `timeout` | `Sampler::fail` (from `maintain`): no reply, the full backoff | yes | the second failure in a row only |
| `error` | `Sampler::fail_reply`: an error reply other than 204, or an inconsistent reply, the full backoff | yes | the second failure in a row only |
| `first_failure` | `Sampler::fail`/`fail_reply` with `dht.sample_backoff_escalate` on: a first failure's 300 s backoff | always 0 | yes |
| `restored` | `Sampler::restore`: the restart proof's restored restriction | yes | yes |
| `sealed` | `Sampler::seal_in_flight`: a sample in flight at a clean stop | yes | yes |

- **No label comes from network input.** `CoolingReason::as_str` is the only source. The render test pins all seven, and checks that no other reason series renders.
- **Excluded, because they aren't cooling records:**
  - a remembered streak whose short cooldown ended (`State::Struck`). It holds a table place (the snapshot's `sample_known`, which isn't exported), but it's in neither this gauge nor `dht_sample_inflight`.
  - cooldowns moved to the spill filter, which `dht_sample_spill_slots` counts.
  - `docs/metrics.md` says both.

## How the gauge stays exact
- **The reason is stored in the cooling record itself.** `State::Cooling` gains a `reason`, which fits in the record's padding, so the record stays 32 bytes (`a_streak_costs_no_more_than_the_record`).
- **The counts are kept on every transition.** `states` changes only through `Sampler::put` and `Sampler::take`, which add and remove the reason's count. Every insert, removal, overwrite, prune, conversion to `Struck`, spill move and restore goes through them, so the counts are exact by construction.
- **Tests:**
  - `cooling_by_reason` debug-asserts that its sum equals `cooling()`;
  - the escalation proptest (both switches, split on and off) and the split proptest (restarts, spills, both escalation settings, timeouts and error replies) assert after every step that the counts equal a recount of `states`;
  - `cooling_reasons_follow_every_transition` and `a_spilled_cooldown_leaves_its_reason` pin each transition;
  - the actor test `a_failed_sample_escalates_only_with_the_switch_on` pins each reply's reason, switch on and off.
- **Per-scrape cost: O(1).** On its maintenance tick the actor copies the seven counts into `Stats` atomics, as it does for `sample_cooling`. Nothing walks the 200,000-place table.
- **Mutation-checked:**
  - dropping the decrement;
  - counting an error reply as a timeout;
  - counting no support as an interval;
  - counting a seal as a restore;
  - counting a first failure as its cause.
  - Each fails a test.

## The counter
- **`dht_sample_failures_total` is the actor's existing `sample_failures`,** now exported. It counts timeouts, error replies (a 204 included) and inconsistent replies.
- **A sample in flight at a clean stop is sealed, not failed.** The crawler sim's restart check already balances this counter against the network's record.

## Behaviour
- **Unchanged.** `complete_unsupported` is the old `complete_scaled(UNSUPPORTED_BACKOFF, scale 1)`, and `fail_reply` is the old `fail`. Only the counted reason differs.
- **The sim goldens are byte-identical,** because the new snapshot field isn't in the report.
