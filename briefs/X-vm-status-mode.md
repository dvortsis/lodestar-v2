# X-vm-status-mode (runner brief, 2026-11-22Z)

Kind: helper (fleet tooling). Dep X-block-on-only-mode (#1060) is merged. Recorded in `docs/decisions/2026-11-22-runner-records-r12.md` (PR #1068).

## The gap
start-block's answer shows `mode_on` only as one more `ok`, because `infra/fleet/run.py summary()` drops ok names. The runbook (`docs/operations/cloud-fleet-runbook.md` §3.3.2) tells the operator to check `MODE=on` in `/var/lib/lodestar-block/block.env` by hand.

## What to do
1. `infra/fleet/steps/vm/vm-status.sh` prints the block's mode as its own line, `mode=abba` or `mode=on`, read from `block.env`.
   - Allow-list the value: anything else prints `mode=unknown`.
   - A missing `MODE` means abba, matching `rules.parse`.
   - The line must survive `run.py summary()`. Check how vm-status lines reach the operator: if summary drops it too, emit it as a `count` line or whatever form summary keeps, without changing summary's format for other steps.
2. Add tests in `infra/fleet/tests/test_block.py` for abba, on, missing and a bad value.
3. The runbook's check uses the vm-status line.
4. Decisions file.

## Rules
- No change to rules, the arms, the leak test, copy-off or any stop condition.
- No cloud calls. Tests run locally (`infra/fleet/run_tests.sh`).
- Read nothing private, and record no endpoint or instance details.

## Owns
- `infra/fleet/steps/vm/vm-status.sh`
- `infra/fleet/tests/test_block.py`
- `docs/operations/cloud-fleet-runbook.md`, the §3.3.2 check only
- `docs/decisions/2026-11-22-X-vm-status-mode.md`
- If `run.py` must change, ask me first.

## Branch and PR
- Branch `slice/X-vm-status-mode`. PR title "X-vm-status-mode: vm-status shows a block's mode".
- Gates: `infra/fleet/run_tests.sh`, plus shellcheck on vm-status.sh with no new findings.
- No step 0 is needed unless something here is wrong. Push once, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
