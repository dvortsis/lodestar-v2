# X-test-vm-fix brief (runner, 2026-11-20)

**Slice:** X-test-vm-fix. Opus, security-sensitive (cloud IAM template and test-VM tooling). Branch `slice/X-test-vm-fix`, created **from #864's head 51f8d9f2a17902fb73e276561be6d78044fe9927**: `git fetch origin pull/864/head && git checkout -b slice/X-test-vm-fix 51f8d9f2a17902fb73e276561be6d78044fe9927`, then merge `origin/main` (never rebase). PR titled `X-test-vm-fix: <title>`. It supersedes #864, which the runner closes when this merges. Never push to #864's branch. Follow `.claude/skills/steward/SKILL.md`. Send the runner `ready <sha>` once CI is green.

**Why:** #864 (X-test-vm, the cloud tester's test-VM tooling) had a fresh Opus review plus `/security-review` at 51f8d9f2 on 2026-11-20: CHANGES, with 2 blocking and 7 important findings. Their surfaces are the test-VM IAM template, the SSH helper and the docs. The coordinator has done its last round, so this slice carries the fixes.

**The findings and the chosen fixes arrive in a message from the runner session (session_01EYhgh4JX3AmHzKXPjYCoUg), not in this file.** Never write the finding detail, or the reason behind a fix, into a commit message, the PR body, the decisions file or any branch file beyond the fix itself, until the runner says so. The PR says only "fixes the #864 review findings" and lists finding ids.

## Rules (hard)
- **No cloud calls of any kind:** no AWS CLI, SDK or MCP tools, no terraform init, plan or apply, no IAM deploy. Template and code edits only. Deploying an updated stack is the owner's step.
- Never read or print `AWS_*` or API-key environment variables.
- Never write account numbers (12-digit numbers other than the documentation placeholders 111122223333 and 999988887777), IP addresses, instance ids, endpoint addresses or account names into any file.
- Tighten only. Never widen a permission, loosen a condition or remove a guard. If a fix seems to need a wider permission, stop and ask the runner.
- Leave the owner's Unraid fence, the $100 cap, IMDS settings and every "Present" item the message lists exactly as they are.

## Owns
#864's files:
- `infra/dev-vm/**`
- `.agents/skills/lodestar-fleet/SKILL.md`
- `infra/fleet/run_tests.sh`
- `docs/decisions/2026-11-12-X-test-vm.md`, extended with a dated "2026-11-20 review fixes" section

## Done when
- Every blocking and important finding in the runner's message is fixed, with a test that would fail without the fix, where the finding is testable (`infra/dev-vm/tests/`).
- The optional items are fixed or answered in the PR body (one line each).
- `python3 -m pytest infra/dev-vm/tests` (or the repo's runner for them) and `infra/fleet/run_tests.sh` pass, and CI is green.
- A fresh Opus self-review plus `/security-review` runs before the PR opens.
- **Size:** most of #864's ~2,300 lines are already reviewed. State the delta size in the PR body.
