# X-backoff-escalate-passthrough (runner brief, 2026-11-22Z): FIX MAIN

Kind: helper, model opus (fix-main, so it goes ahead of other work). Deps: CR-SAMPLE-BACKOFF-ESCALATE (#1029, merged).

## Why
`main` is red in the `infra` job, which sits inside `ci-ok`.
- **The failing test:** `infra/fleet/tests/test_steps.py`, `Compose.test_every_crawler_switch_passes_through_by_bare_name`. It requires every `env("LODESTAR_DHT_*")` that `crates/lodestar/src/config.rs` reads to be a switch the collector knows, or a known setting.
- **The cause:** since #1029, `config.rs` reads `LODESTAR_DHT_SAMPLE_BACKOFF_ESCALATE`, and the collector doesn't know it.
- **Why #1029 merged green anyway:** `.github/scripts/ci_changes.py`'s `INFRA` filter matches neither `crates/lodestar/src/config.rs` nor `docs/soak/tools/**`, so the job never ran. Every PR that touches `infra/` now fails `ci-ok`.

## Do
1. **Pass the switch through** exactly as `LODESTAR_DHT_SAMPLER_COOLDOWN_SPLIT` is passed:
   - `docs/soak/tools/linux/collect.py` `SWITCH_ENV`, with a sensible key such as `sample_backoff_escalate`;
   - `infra/fleet/steps/vm/compose.yaml` (bare name, not pinned);
   - `infra/fleet/steps/vm/block-lib.sh`'s switch list.

   It stays off unless set.
2. **Update every test and README that iterates `SWITCH_ENV` or `EXPERIMENT_SWITCHES`** so it stays green:
   - `docs/soak/tools/analyze_cr.py` and its `test_analyze_cr*.py`;
   - `linux/test_collect_spec.py`;
   - `infra/fleet/tests/test_block.py`;
   - `docs/soak/tools/linux/README.md`;
   - `docs/operations/cloud-fleet-runbook.md`, if it lists the switches.

   Say whether the new switch belongs in `STAGE1_SWITCHES` or `STACKED`. Default: neither. Leave the A/B plan to the runner.
3. **Close the path-filter gap:** the `INFRA` filter in `.github/scripts/ci_changes.py` also matches `crates/lodestar/src/config.rs` and `docs/soak/tools/`. Add a case to `.github/scripts/test_ci_changes.py`.
4. **Write a short decisions file:** `docs/decisions/2026-11-22-X-backoff-escalate-passthrough.md`.
5. **Add this slice's own `runner/slices.yaml` entry and `ws` placement** next to `X-fleet-switch-passthrough`. The runner's #1040 doesn't add one.

## Rules
- **Repo change only.** Updating a deployed stack stays the owner's. No cloud calls, and no terraform init.
- **Checks:**
  - `python3 -m pytest infra/fleet/tests docs/soak/tools .github/scripts/test_ci_changes.py` (pip install hypothesis locally if it's missing);
  - `cd tools/status-connector && npm test`.

  Reproduce the failure on `main` first, then show it passing.
- **Branch and PR:** branch `slice/X-backoff-escalate-passthrough`, PR title `X-backoff-escalate-passthrough: <title>`. Push once, then send "ready <pr> <sha>". Follow `.claude/skills/steward/SKILL.md`, and never merge.
