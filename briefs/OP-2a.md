# OP-2a (Opus, security-sensitive): the fleet's AWS templates and the Terraform S3 backend

Assigned by the runner, session_01EYhgh4JX3AmHzKXPjYCoUg, on 2026-11-08 at about 22:55Z. It continues in OP-1's session, which merged as #636 (bc2a6e95). Follow `.claude/skills/steward/SKILL.md`. Branch `slice/OP-2a` from main bc2a6e95 or later. PR title `OP-2a: <title>`. The runner gives it a fresh Opus review and a /security-review.

## Read first
- `docs/plan/2026-11-08-cloud-operator.md`: §3 and §4 (credentials, revocation, the containment for the loosenings the owner confirmed on 2026-11-08), "Owner setup" step 4 (which deploys this template), and the Slices row **OP-2a**. Its contents are binding.
- `docs/decisions/2026-11-08-OP-1.md`: the lock and state keys. The lock is `locks/fleet.json` in the control bucket, inside the plan's `locks/fleet*` prefix. `fleet.json` is `fleet/fleet.json` in the state bucket. Grant exactly those keys.
- `infra/` as merged (#625, #634, #636), and `docs/operations/lodestar-cloud-assets.md`.
- The `OP-2a` entry in `runner/slices.yaml`.

## Owns
- `infra/aws/fleet-control/**`;
- `infra/akamai/terraform/versions.tf` and `infra/akamai/terraform/backend.tf`;
- `docs/decisions/2026-11-08-OP-2a.md`;
- **shared, one small edit if needed:** `ci_changes.py`'s `infra` filter and the `infra` job in `ci.yml`, only to run your template tests.

## What OP-2a is
The CloudFormation templates the owner deploys, and nothing that calls a cloud:
1. **The GitHub OIDC provider.**
2. **Three roles:**
   - `LodestarFleetOperator`, `LodestarOperatorBridge` and `LodestarPiaDelivery`;
   - each trusted on `aud` = `sts.amazonaws.com` **plus** a `sub` naming its own environment and workflow at `refs/heads/main`;
   - least privilege: the exact bucket keys and prefixes, never `*`;
   - `LodestarPiaDelivery` is its own stack, so it can sit in the secret's account.
3. **The deny-all policy** the plan describes for revocation.
4. **The buckets:**
   - state (SSE-S3), evidence and control;
   - public access blocked, TLS-only bucket policies, versioning where the plan asks;
   - the cross-account `fleet.json` grant.
5. **The secret shell.** An empty secret; the owner puts the value in.
6. **Terraform's S3 backend:**
   - `backend.tf` with native S3 locking (`use_lockfile`);
   - `required_version` raised to 1.10 in `versions.tf`;
   - the bucket, key and region come from `-backend-config`, never literals;
   - check that the `.ps1` scripts and `fleet.py` still `init` correctly (CI's Terraform validate uses `-backend=false`).

## Tests
- cfn-lint, or a stdlib-plus-PyYAML check if cfn-lint can't be pinned with hashes, installed with `--require-hashes` as OP-1 did.
- **Policy tests over the parsed templates:**
  - every trust policy pins both `aud` and a `sub` at `refs/heads/main` with no wildcard in the environment or workflow;
  - no `Action: "*"` or `Resource: "*"` outside the deny-all;
  - every bucket blocks public access and denies non-TLS;
  - no account ID or ARN with an account ID appears as a literal (use `AWS::AccountId` and parameters).
- Terraform `fmt` and `validate` still pass.

## Rules
- No cloud call, in CI or anywhere.
- No live account IDs, ARNs with account IDs, IPs or tokens in the repo.
- Never the owner's soak host.
- No model identifiers.
- No fixed sleeps in tests.
- Write a decisions file for anything the plan left open. Never loosen §4's containment; if the plan seems to need that, stop and message the runner.
