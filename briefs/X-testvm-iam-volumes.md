# X-testvm-iam-volumes (runner brief, 2026-11-28)

**Slice:** X-testvm-iam-volumes (runner/slices.yaml). Opus, core. Under `/security-review` (IAM).
**Branch:** `slice/X-testvm-iam-volumes` from current `main`.
**PR title (exact):** `X-testvm-iam-volumes: The test-VM policy requires the Purpose tag on volumes and lets the test user delete its own`
**Owns:** `infra/dev-vm/aws-claude-testvm.json`, `infra/dev-vm/linux-aws.md`, `infra/dev-vm/windows.md`, `docs/decisions/*-X-testvm-iam-volumes.md`.

## The owner's decision (owner-decisions §2.88, items 31 and 33, yes)
- **31:** the statement that allows creating test volumes (`RunOnlyEncryptedBoundedVolumes` or its
  equivalent) also requires the tag `Purpose=X-test-vm` on the volumes.
- **33:** the test user may `ec2:DeleteVolume` on its own test volumes only, scoped like the
  existing terminate statement (`ManageOnlyOwnLodestarTestInstances`): `Project=Lodestar`,
  `Environment=Test`, `Purpose=X-test-vm`, and `LaunchedBy` equal to its own `aws:userid`.
- The runbooks' teardown step deletes a root volume that survives termination, by its id.
  (DeleteOnTermination=true is already the default since #1162. This covers the leftover case.)

## Rules
- **No cloud calls.** Don't use AWS MCP tools or the AWS CLI. The owner applies the reviewed policy.
- IAM must be least privilege.
  - Check each condition key with the IAM docs you know.
  - Tag-on-create needs `aws:RequestTag` plus `ec2:CreateTags` with `ec2:CreateAction`.
  - Delete needs `ec2:ResourceTag/...`.
  - The `LaunchedBy` must be set from the caller and not be forgeable by the caller. Explain how the
    existing terminate statement achieves that, and mirror it.
- Validate that the JSON parses. If the repo has a policy lint or test (search `infra/dev-vm`
  and `.github/workflows`), run it.
- The user docs (linux-aws.md, windows.md) are for someone new to AWS: plain steps.
- Run `/security-review` on your diff before you ask for review, and state the result in the PR.
- No account numbers (only 111122223333 or 999988887777 as examples), no instance or volume ids,
  no IPs.
- Decisions file: `docs/decisions/<date>-X-testvm-iam-volumes.md`.
- Follow `.claude/skills/steward/SKILL.md`. Report `ready <PR> <sha>` to the runner session.
  After the merge, the runner tells the owner to apply the policy.
