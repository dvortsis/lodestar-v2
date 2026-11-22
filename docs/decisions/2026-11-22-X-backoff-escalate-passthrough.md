# X-backoff-escalate-passthrough decisions (#PR)

Runner brief, 2026-11-22Z, to fix main. CR-SAMPLE-BACKOFF-ESCALATE (#1029) made `crates/lodestar/src/config.rs` read `LODESTAR_DHT_SAMPLE_BACKOFF_ESCALATE`. The fleet's switch test (`infra/fleet/tests/test_steps.py`, `test_every_crawler_switch_passes_through_by_bare_name`) then failed on main, inside `ci-ok`, because the soak collector didn't know the variable.

## The switch is passed through, off unless set
- **The collector records it.** `docs/soak/tools/linux/collect.py` `SWITCH_ENV` gains `sample_backoff_escalate` → `LODESTAR_DHT_SAMPLE_BACKOFF_ESCALATE`, named after the config field (`config.dht.sample_backoff_escalate`).
- **The fleet stack passes it through by bare name,** as `LODESTAR_DHT_SAMPLER_COOLDOWN_SPLIT` is: `infra/fleet/steps/vm/compose.yaml` and `block-lib.sh`'s `CRAWLER_SWITCHES`. An unset variable stays unset, so the binary's default holds.
- **In no treatment, and not in `STAGE1_SWITCHES` or `STACKED`** (the brief's default). The A/B plan for it is the runner's.
  - The analyzer's `EXPERIMENT_SWITCHES` comes from `SWITCH_ENV`, so it's a switch a segment records and checks: from stage 2 it must be off in both arms unless it's a winner.
  - A stacked segment must record every `SWITCH_ENV` switch, so collectors built before this change make such a segment invalid. That's the rule for every new switch, as with stage 3's.
- **The tests that iterate `SWITCH_ENV` read it, so they needed no edit.** That's `test_steps.py`, `test_block.py`, `test_analyze_cr*.py` and `test_collect_spec.py`.
- **Docs:** `docs/soak/tools/linux/README.md` lists it among the recorded switches. `docs/operations/cloud-fleet-runbook.md` names no switch, so it's unchanged.

## The path filter that let it through
- **The `infra` job never ran on #1029.** `.github/scripts/ci_changes.py`'s `INFRA` filter matched neither `crates/lodestar/src/config.rs` nor `docs/soak/tools/`, the two files the fleet tests read the switches from.
- **The filter now matches both:** `crates/lodestar/src/config.rs` exactly, and anything under `docs/soak/tools/`.
- **A test case covers them, and near misses** (`config.rs.bak`, `config/mod.rs`, `main.rs`, `docs/soak/README.md`, `docs/soak/toolsx/`): `FleetTestInputs` in `test_ci_changes.py`. It fails on main's filter.

## Not done here
- **No deployed stack is updated.** That stays the owner's, with no cloud calls.
