# X-testvm-delete-on-termination (Sonnet; runner brief 2026-11-28)

**Branch:** `slice/X-testvm-delete-on-termination`. **PR title:** `X-testvm-delete-on-termination: Test-VM launches set DeleteOnTermination on the root volume`.

## Why
#1017 (X-testvm-teardown-by-id) found that both test-VM launch steps, and `Deploy-AwsWindows.ps1`'s `/dev/sda1` mapping, leave the root volume's `DeleteOnTermination` to the image's default. If an image says false, a terminated test VM leaves a paid volume behind. The teardown step deletes it by id, but the launch should not depend on that.

## Task
Set `DeleteOnTermination=true` explicitly on the root (`/dev/sda1`) block-device mapping in:
1. `infra/dev-vm/linux-aws.md` step 2 (around line 28): add it to the `/dev/sda1` mapping's list.
2. `infra/dev-vm/windows.md` step 2 (around line 56): the same.
3. `infra/akamai/terraform/scripts/Deploy-AwsWindows.ps1` line 214: `Ebs={VolumeSize=$RootVolumeGiB,Encrypted=true,DeleteOnTermination=true}`.

Keep each runbook's teardown step (step 7: delete the root volume by id if it still exists) exactly as it is. It is the backstop, and the runbooks should say so in one short clause next to the new setting.

If the repo has a test or lint for the PowerShell script (look under `infra/akamai/terraform/scripts/` and `.github/workflows/` for PSScriptAnalyzer or Pester), run it or make sure CI does.

## Owns
- `infra/dev-vm/linux-aws.md`
- `infra/dev-vm/windows.md`
- `infra/akamai/terraform/scripts/Deploy-AwsWindows.ps1`
- `docs/decisions/2026-11-28-X-testvm-delete-on-termination.md` (short: what changed and why; no owner decision changes)

Ask the runner before touching anything else.

## Rules
- **No cloud calls of any kind.** Don't run the script or any AWS CLI or tool, even read-only, and don't use any AWS MCP tool that is announced. No IAM changes. Owner queue items 31 and 33 are the owner's.
- **Nothing identifying in what you write:** no instance or volume ids, account numbers, IPs or host paths.
- No model identifiers in commits, PRs or code. UTC times with `Z` only. Never merge.

## Done
- CI green.
- Self-review per `.claude/skills/steward/SKILL.md`.
- Send the runner `ready <full sha>` with the claude-code-remote send_message tool, to session_01EYhgh4JX3AmHzKXPjYCoUg.
