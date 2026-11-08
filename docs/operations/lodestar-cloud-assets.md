# Lodestar cloud infrastructure: operational source of truth

Decision record: `docs/decisions/2026-11-08-X-cloud-infra.md`. Agents only read; every deploy, reboot, remote session or delete is run by a human (`.agents/skills/lodestar-fleet/SKILL.md`).

## Deployed services

| Monitored provider | AWS CloudFormation stack | Lambda | Schedule |
|---|---|---|---|
| AWS Windows EC2 test | `lodestar-expiration-guard` | `lodestar-aws-expiration-guard` | 15 minutes |
| Akamai Linode | `lodestar-akamai-watchdog` | `lodestar-akamai-expiration-guard` | 30 minutes |

Both services were reported active when the repository copy was reconciled on 2026-11-08; this repository has not re-checked them. The Akamai token is stored **only** in AWS Secrets Manager (`lodestar/akamai-token`). The SNS email subscription is confirmed; the AWS expiration guard's `NotificationEmail` uses `NoEcho` in the current live template. Routine agents use `LodestarObserver` and cannot assume `LodestarProvisioner`.

## Canonical templates

- `infra/aws/expiration-guard/deployed/template.json`: derived from live `lodestar-expiration-guard`, with the real VPC and Windows security group IDs replaced by **required** CloudFormation parameters. The private email address was already converted to `NotificationEmail` (`NoEcho`) in the deployed stack. Do not add literal parameter defaults.
- `infra/akamai/watchdog/deployed/lodestar-akamai-watchdog.json`: matched the live `lodestar-akamai-watchdog` template before its account-specific SNS ARN was converted to `Fn::Sub` using AWS pseudo-parameters. `index.py` matched the deployed inline Lambda code at reconciliation time; since then one dead allowlist entry (`/networking/firewalls?page=`) has been removed from the source, so the deployed copy differs by that line until the next stack update.
- `infra/akamai/terraform`: Akamai instance Terraform. Keep `.terraform.lock.hcl`; never commit `.tfstate`, `terraform.tfvars`, credentials or `.terraform/`.
- `.agents/skills/lodestar-fleet`: repository copy of the agent skill; synchronize deliberately to the global execution copy outside Git.
- The skill and the PowerShell scripts hold **no** account IDs or resource IDs (AMI, subnet, security group, instance role or profile). The scripts read them from the operator's local `LODESTAR_AWS_*` environment variables or parameters and have no defaults.

These public source templates are **semantically parameterized representations**, not byte-identical snapshots of the live stack. The finalizer did not deploy them, update the AWS stacks, or change the Linode fleet. Before any future CloudFormation update, review a change set and supply existing parameter values securely. In particular, updating the AWS expiration-guard source requires `NotificationEmail`, `LodestarVpcId` and `WindowsSecurityGroupId`. Do not infer an email address from the repository.

## Excluded from public source control

Alternative watchdog drafts, a former local AWS guard candidate and outdated cloud scripts were backed up on the operator's machine and left out of the repository. No original Terraform operational state or credentials were moved.

## Constraints and remaining tests

- The two automatic reapers have passed non-destructive inventory/dry-run checks; **actual expired-instance deletion remains unverified**. Do not present untested deletion as proven.
- The Akamai watchdog does not automatically remove detached firewalls, volumes or reserved addresses.
- The migrated Terraform directory must **not** execute `terraform apply` until private state and working-directory ownership are reconciled. A new empty state may recreate machines, and out-of-band expirations can likewise trigger recreation.
- Never grant an unrestricted local coding agent access to administrator cached credentials in the same Windows session. IAM role separation alone is not a local process sandbox.
- Secret scans are best-effort. Before pushing a public branch, review staged changes and Gitleaks results, and confirm all Terraform and Git validations.
