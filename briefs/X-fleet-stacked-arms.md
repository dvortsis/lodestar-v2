# X-fleet-stacked-arms (runner brief, 2026-11-22Z)

Kind: core, model opus. Dep X-fleet-switch-passthrough is merged. This must land before any CR-AB-1 stage runs on the fleet.

## What
CR-AB-1 stages each test one switch on top of the winners already kept (the owner asked for stacking on winners). Today an A/B block's arms differ only in their treatment.
- **The WINNERS line:** a `WINNERS=` line in block.env turns the named earlier winners on in **both** arms, so the control is "winners on" and the treatment is "winners on, plus one switch".
- **The `lc` treatment key** maps to two variables (lookup_end and CR-1c's live change). Take the exact names from CRAWLER_SWITCHES and the X-fleet-switch-passthrough decisions file.
- **start-block.sh validates every key** in WINNERS and in the treatment against the one CRAWLER_SWITCHES list, and refuses an unknown key with a clear error, before anything starts.
- **A key in both WINNERS and the treatment is refused,** because a stage can't test a winner against itself.

## Rules
- **No cloud calls, no terraform, nothing run against the fleet.** This is scripts, tests and docs only.
- **No new switch names.** The single source is the existing CRAWLER_SWITCHES list. The tests are ast- or parse-based, as in X-fleet-switch-passthrough.
- **Both arms must get identical WINNERS variables.** Add a test that renders both arms' environments and diffs them: the only difference is the treatment.
- **Runbook (docs/operations/cloud-fleet-runbook.md):** add a short section on how to write a stacked stage, with an example block.env. Developer doc, so no user-docs skill.

## Step 0 (send to the runner before building)
1. Where block.env is parsed today and where each arm's environment is built.
2. The exact mapping for `lc`, and any other multi-variable key.
3. Files and an estimate.

## Owns
- `infra/fleet/steps/vm/block-lib.sh`
- `infra/fleet/steps/vm/start-block.sh`
- `infra/fleet/tests/test_block.py`
- `docs/operations/cloud-fleet-runbook.md`
- `docs/decisions/*-X-fleet-stacked-arms.md`
- this slice's `slices.yaml` entry

## Tests and process
- **Tests:** WINNERS empty, absent and multi-key; an unknown key refused; overlap refused; lc mapped to both variables; the arms' environments differ only in the treatment.
- **Before pushing:** run the infra python tests (`.github` and `infra/fleet/tests`) and shellcheck if it's installed.
- **Branch and PR:** branch `slice/X-fleet-stacked-arms`, PR title "X-fleet-stacked-arms: <title>". Push once per round, then send "ready <pr> <sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
