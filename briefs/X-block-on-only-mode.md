# X-block-on-only-mode (runner brief, 2026-11-22Z)

Kind: core (fleet harness). Dep X-fleet-stacked-arms (#1049) is merged. The lead is from X-test-vm.

## The gap
`infra/fleet/steps/vm/start-block.sh` always opens segment 1 as control (ABBA). A screen that needs the treatment on throughout, such as a fresh-database screen, can't run.

## What to do
1. **Add an explicit on-only mode**, for example a `MODE=abba|on` argument, with `abba` as the default so existing callers are unchanged.
   - In on-only mode, every segment runs the treatment.
   - Refuse on-only for an A/B stage that needs a control. Find where the stage is known, the rules file or the control side, and refuse there and again on the VM side (fail closed, the same pattern as `args_valid`).
   - Analysis must not read an on-only block as an A/B result: no control means no `analyze_cr` verdict. Find where that happens and make it refuse or mark the block, with a test.
2. **Trace every reader of the block's arguments and segment order:**
   - the control side that writes `args`;
   - `block-lib.sh`;
   - the segment switcher and `collect()`;
   - `analyze_cr`.
   Name each in step 0. If a file outside owns must change, list it and wait for my answer.
3. **Runbook** (`docs/operations/cloud-fleet-runbook.md`):
   - document the mode and when to use it;
   - add that the image has no `env` binary (read a running process's switches from `/proc/<pid>/environ`);
   - add that a fresh-database screen needs at least 12 h before its restart drill.

## Rules
- No cloud call, no terraform, no deployed-stack update, no workflow permission or secret change.
- Never loosen the leak test, the host guard, the boot guard or any stop condition. On-only changes only which arm each segment runs.
- Output stays fixed names only (no free text in step output).
- Don't record IPs, endpoint addresses, instance or volume ids, or account names anywhere.
- **Tests in `infra/fleet/tests/test_block.py`:**
  - abba is unchanged;
  - on-only runs the treatment in every segment;
  - on-only is refused where a control is required, on both sides;
  - a bad MODE value fails `args_valid`;
  - analysis refuses or marks an on-only block.

## Owns
- `infra/fleet/steps/vm/start-block.sh`
- `infra/fleet/steps/vm/block-lib.sh`
- `infra/fleet/tests/test_block.py`
- `docs/operations/cloud-fleet-runbook.md`
- `docs/decisions/2026-11-22-X-block-on-only-mode.md`
- this slice's `runner/slices.yaml` entry
- Refine at step 0: the control side and the analysis files, if they must change.

## Step 0 (before building)
1. The readers of args and segment order.
2. Where the stage's need for a control is known.
3. The exact files, owns changes and an estimate.

## Branch and PR
Branch `slice/X-block-on-only-mode`, PR title "X-block-on-only-mode: Fleet A/B blocks: a mode with the treatment on in every segment". Run the infra tests the way CI does (see `.github/workflows/ci.yml`'s infra job) and shellcheck. Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
