# Lodestar cloud infrastructure: operational source of truth

Decision record: `docs/decisions/2026-11-08-X-cloud-infra.md`. Agents only read; every deploy, reboot, remote session or delete is run by a human (`.agents/skills/lodestar-fleet/SKILL.md`).

## Deployed services

| Monitored provider | AWS CloudFormation stack | Lambda | Schedule |
|---|---|---|---|
| AWS Windows EC2 test | `lodestar-expiration-guard` | `lodestar-aws-expiration-guard` | 15 minutes |
| Akamai Linode | `lodestar-akamai-watchdog` | `lodestar-akamai-expiration-guard` | 30 minutes |

Both services were reported active when the repository copy was reconciled on 2026-11-08; this repository has not re-checked them. The Akamai token is stored **only** in AWS Secrets Manager (`lodestar/akamai-token`). The SNS email subscription is confirmed; the AWS expiration guard's `NotificationEmail` uses `NoEcho` in the current live template. Routine agents use `LodestarObserver` and cannot assume `LodestarProvisioner`.

## Canonical templates

- `infra/aws/expiration-guard/deployed/template.json`: built from `index.py` by `build_template.py` (tests: `test_guard.py`); derived from live `lodestar-expiration-guard`, with the real VPC and Windows security group IDs replaced by **required** CloudFormation parameters. The private email address was already converted to `NotificationEmail` (`NoEcho`) in the deployed stack. Do not add literal parameter defaults. **The source differs from the live stack** until the owner deploys it:
  - the terminate grant also requires `ec2:InstanceType = c6a.xlarge`, and the function `DependsOn` its log group (X-cloud-infra);
  - the 48-hour clock starts at the earliest of `LaunchTime` and the root volume's `AttachTime`, so a stop/start can't reset it (X-cloud-infra-2);
  - it scans every `Project=Lodestar` instance and logs one outside its scope as `out_of_scope_instance`; the `lodestar-expiration-guard-out-of-scope` alarm fires on it (X-cloud-infra-2);
  - a `lodestar-expiration-guard-heartbeat` alarm fires when the function hasn't run for an hour (X-cloud-infra-2).
- `infra/akamai/watchdog/deployed/lodestar-akamai-watchdog.json`: matched the live `lodestar-akamai-watchdog` template before its account-specific SNS ARN was converted to `Fn::Sub` using AWS pseudo-parameters. `index.py` matched the deployed inline Lambda code at reconciliation time. **The source has changed in six ways since** (listed in `infra/akamai/watchdog/deployed/README.md`): three from X-cloud-infra (the dead allowlist entry, `TOKEN_REQUIRED`, `DependsOn`) and three from X-cloud-infra-2 (the out-of-scope alarm, the heartbeat alarm, the 90-second deletion confirmation with a 300 s timeout).
- `infra/akamai/terraform`: Akamai instance Terraform. Keep `.terraform.lock.hcl`; never commit `.tfstate`, `terraform.tfvars`, credentials or `.terraform/`.
- `.agents/skills/lodestar-fleet`: repository copy of the agent skill; synchronize deliberately to the global execution copy outside Git.
- The skill and the PowerShell scripts hold **no** account IDs or resource IDs (AMI, subnet, security group, instance role or profile). The scripts read them from the operator's local `LODESTAR_AWS_*` environment variables or parameters and have no defaults.

These public source templates are **semantically parameterized representations**, not byte-identical snapshots of the live stack. The finalizer did not deploy them, update the AWS stacks, or change the Linode fleet. Before any future CloudFormation update, review a change set and supply existing parameter values securely. In particular, updating the AWS expiration-guard source requires `NotificationEmail`, `LodestarVpcId` and `WindowsSecurityGroupId`. Do not infer an email address from the repository.

## Deploying template changes (owner only)

Agents never deploy. The owner deploys each template from an interactive terminal with the `lodestar-admin` MFA session, through a change set:

1. **Check the source.** In a clean checkout of `main`, run `python3 build_template.py && git diff --exit-code` in the template's folder, and run its tests (`python3 -m unittest -v`).
2. **Create a change set; don't update directly.**
   - AWS guard: `aws cloudformation create-change-set --stack-name lodestar-expiration-guard --change-set-name x-cloud-infra-2 --template-body file://template.json --capabilities CAPABILITY_NAMED_IAM --parameters ParameterKey=NotificationEmail,UsePreviousValue=true ParameterKey=LodestarVpcId,UsePreviousValue=true ParameterKey=WindowsSecurityGroupId,UsePreviousValue=true --profile lodestar-admin --region us-east-1`
   - Akamai watchdog: `aws cloudformation create-change-set --stack-name lodestar-akamai-watchdog --change-set-name x-cloud-infra-2 --template-body file://lodestar-akamai-watchdog.json --capabilities CAPABILITY_NAMED_IAM --profile lodestar-admin --region us-east-1`
3. **Review it:** `aws cloudformation describe-change-set --stack-name <stack> --change-set-name x-cloud-infra-2 ...`. Expect *Modify* on `Function` (both stacks) and `Role` (guard only), and *Add* on `OutOfScopeMetric`, `OutOfScopeAlarm` and `HeartbeatAlarm`. **Stop if anything shows *Remove* or *Replacement*,** above all on the Secrets Manager secret, the SNS topic or its email subscription.
4. **Execute it:** `aws cloudformation execute-change-set --stack-name <stack> --change-set-name x-cloud-infra-2 ...`, then wait for `UPDATE_COMPLETE`.
5. **Check the result.**
   - Invoke each Lambda with `{"dry_run": true}`; the result now includes `out_of_scope`.
   - The two heartbeat alarms read `OK` within an hour. They start in `INSUFFICIENT_DATA` and, because missing data counts as breaching, may alert once until the first scheduled run.
6. **Roll back if needed:** the previous template stays in the stack's history; update back to it the same way, through a change set.

## Excluded from public source control

Alternative watchdog drafts, a former local AWS guard candidate and outdated cloud scripts were backed up on the operator's machine and left out of the repository. No original Terraform operational state or credentials were moved.

## Constraints and remaining tests

- The two automatic reapers have passed non-destructive inventory/dry-run checks; **actual expired-instance deletion remains unverified**. Do not present untested deletion as proven.
- **The live IAM roles are not in this repository.** `LodestarObserver` (with its explicit denies), `LodestarProvisioner` (trusted only by `lodestar-admin` with MFA), the `LodestarRestrictedEC2` policy and the instance role exist only in the live account, as the owner reported. Nothing here defines or checks them; the skill describes them from that report.
- **More than 10 eligible Linodes stops all deletion (deliberate, fail closed).** A sudden crowd of expired machines more likely means a scoping bug than real expiry, so the watchdog raises an error and deletes nothing that run; the error alarm brings a person in.
- CI checks `terraform fmt`/`init`/`validate`, the Lambda tests, the template rebuilds and a PowerShell parse check on every change to `infra/`. **The PowerShell scripts' runtime behaviour is still unverified.**
- The Akamai watchdog does not automatically remove detached firewalls, volumes or reserved addresses.
- The migrated Terraform directory must **not** execute `terraform apply` until private state and working-directory ownership are reconciled. A new empty state may recreate machines, and out-of-band expirations can likewise trigger recreation.
- Never grant an unrestricted local coding agent access to administrator cached credentials in the same Windows session. IAM role separation alone is not a local process sandbox.
- Secret scans are best-effort. Before pushing a public branch, review staged changes and Gitleaks results, and confirm all Terraform and Git validations.
