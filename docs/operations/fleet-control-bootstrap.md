# Fleet-control bootstrap: owner review procedure

This procedure prepares a backend; it is not deployment authorization. Cleanup approval coupling is resolved in the separately reviewed [independent cleanup design](fleet-control-cleanup.md). The proposed canonical fleet account is the account selected by the owner for this remediation, with region `us-east-1`. Keep its account ID and bucket names in owner-local configuration. No new AWS account, replacement fleet, IAM change or GitHub setting is authorized by these instructions.

## Verified source correction and outstanding live checks

The prior read-only GitHub inspection found repository ID `1427502271`, owner ID `83323977`, `use_default=true`, `use_immutable_subject=true` and prefix `repo:dvortsis@83323977/lodestar-v2@1427502271`, without an active customized subject. Recheck live configuration before commissioning. Source CloudFormation OIDC trusts require immutable identity, environment and direct `workflow_ref` at `main`, with exact audience `sts.amazonaws.com`. The provisioning/PIA/operator roles are listed below; the separate cleanup template adds an invocation-only role for owner emergency dispatch. See [GitHub's OIDC claim reference](https://docs.github.com/en/actions/reference/security/oidc): `workflow_ref` names the workflow the run started from, and `job_workflow_ref` names the workflow file the job runs: a reusable workflow if one is called, otherwise the same value as `workflow_ref`. The runner refuses a token where the two differ. The issuer discovery metadata also advertises `workflow_ref`.

| Role | Environment | Direct workflow refs allowed |
|---|---|---|
| `LodestarFleetOperator` | `cloud-fleet` | `fleet.yml@refs/heads/main` |
| `LodestarPiaDelivery` | `pia-delivery` | `fleet.yml@refs/heads/main` |
| `LodestarOperatorBridge` | `cloud-operator` | `operator-wake.yml`, `operator-apply.yml`, `operator-injection.yml`, each at `refs/heads/main` |

`fleet.yml`, its PIA delivery jobs and the three operator workflows are implemented and covered by workflow/policy regressions. Local tests do not establish live operator readiness; keep it disabled until the native settings, issued claims and approved runtime are verified.

The live fleet and PIA entry points share `apply.oidc_session`. It checks run identity and live GitHub settings **before requesting a JWT**; then checks issued issuer/audience/subject, repository IDs, environment, workflow and main ref **before STS**. Parsing claims locally is a consistency check, not authentication; STS verifies the token signature. No live JWT was requested during code-only remediation. Actual claim issuance/STS success remains a separately authorized integration check.

The inspected `cloud-fleet` environment has no selected-branch restriction or required reviewer, and the PIA/operator/cleanup environments are absent. Code changes cannot install native environment protections. The GET-only audit therefore must currently fail:

```powershell
python infra/fleet/deployment_policy.py audit
```

It reads through existing `gh` authentication and never creates environments. Native settings required for **provisioning, PIA and operator** environments:

- Deployment branch policy: selected/custom branches, exactly one entry `main` of type `branch`, no tags/wildcards. Merely allowing all protected branches is insufficient.
- Required reviewers: exactly the repository owner user ID `83323977`. GitHub accepts any one listed reviewer, so adding another person/team would no longer require this owner's approval.
  - **Exception: `cloud-operator`.** Since 2026-11-14, by the owner's decision, `cloud-operator` needs no required reviewer, so operator wakes run without approval. The audit accepts either no required-reviewers rule or the owner-only rule below, so the owner can turn approval back on at any time. Any other reviewer, a team, a second reviewer, a wait timer or any other protection rule fails the audit. The environment is listed under `unattended_environments` in `infra/aws/fleet-control/github-deployment-policy.json`. `cloud-fleet` and `pia-delivery` still require the owner.
- Administrator approval bypass: disabled.
- The required-reviewers rule must explicitly return `prevent_self_review=false` (leave **Prevent self-review** unchecked). With the owner as sole reviewer, `true` prevents the owner from approving a run they initiated and deadlocks provisioning. Missing or non-boolean values also fail the audit. This is an explicit single-owner approval gate, not a two-person control; adding another reviewer would violate the sole-owner requirement. [GitHub documents the initiator restriction](https://docs.github.com/en/actions/reference/workflows-and-actions/deployments-and-environments#required-reviewers). Enabling self-review does not approve a run automatically: the owner must still approve each protected job, and administrator bypass remains disabled.

The separate `fleet-cleanup` environment instead requires main-only admission and no approval/timer queue; no required-reviewers rule or `prevent_self_review` field is required there. It holds invocation-only authority for the owner-dispatch workflow and no provisioning credential. Its actor, freshness, version and role boundaries are specified in the [cleanup runbook](fleet-control-cleanup.md). Independent expiration requires no GitHub environment approval. Inspect and coordinate both existing Lambda schedules using the [legacy-watchdog handoff procedure](fleet-watchdog-coordination.md) before activating the broker; this runbook does not authorize schedule changes.

Required reviewers gate provisioning and PIA jobs. Scheduled expiration is instead an independently authorized EventBridge/Lambda broker, and emergency teardown is an owner-only dispatch with invocation-only authority in `fleet-cleanup`, without a reviewer/timer queue. It receives no provisioning credential or Terraform access. The cleanup stack, credentials, scope, alerts and canary need separate owner authorization before enabling expiration or live provisioning. The broker retains regionless firewalls for owner cleanup and never repairs state automatically.

The desired OIDC settings are the `oidc` object of `infra/aws/fleet-control/github-deployment-policy.json`. Changing repository-wide subjects may affect other federated consumers. Review actual IAM trusts through an approved read identity first; the expired observer session did not allow live AWS trust verification. Install and verify native provisioning environment protections before activating matching OIDC subjects/trust. Apply cloud trust and GitHub subject configuration only through separately approved changes, keep live mode off throughout, and verify actual issuance before enabling it. Do not leave a usable provisioning role behind an unprotected environment or weaken trust to match today's default subject.

## Blockers before proposing a CREATE change set

1. Owner confirms the canonical account and that no existing backend/retained resources already belong to this fleet. The previous account inventory is owner-reported, not a substitute for historical/other-account and name-collision checks. Inspect all candidate stack outputs/resources, including deleted/failed stack history and CloudTrail events as described in the [backend investigation](2026-11-10-fleet-backend-investigation.md).
2. Recover prior private state, authoritative ownership and any fleet lock/manifest. Compare it with complete paginated Linode instances, firewalls, volumes and reserved-IP inventory in the correct Akamai account. **Live Linode inventory remains unresolved:** no read-only token is available to this agent. No credential value was read or requested from Secrets Manager; no empty-fleet conclusion is justified.
3. Reuse any existing GitHub OIDC provider. Preserve existing named roles, policies and secrets, including secrets pending deletion; resolve ownership/import/reference design before creation. Never delete a secret or state to make a name available.
4. Review source changes, tests, exact template hashes, execution permissions and native environment settings. Keep `FLEET_LIVE` false/unset. The new `actions: read` permission on credential-consuming jobs only permits inspection; no GitHub setting-write permission was added.

## Human-only CloudFormation commands: preparation, review, execution

The following commands are **not executed by the agent**. Creating a change set is itself an AWS write and needs explicit approval first. Execution requires a second approval after inspecting that actual change set. Use the owner's existing authorized deployment profile; do not give the observer administrator access. The owner-local command sheet prepared with this change fixes the proposed account ID and template hash. Do not run a CREATE change set if a stack/backend is found; prepare a separately reviewed UPDATE/import plan instead.

From the reviewed repository root, first populate these non-secret values in the owner's terminal. Use the agreed canonical account; do not infer a role/profile's account from its name:

```powershell
$ExpectedAccountId = Read-Host 'Approved canonical AWS account ID'
$OwnerProfile = Read-Host 'Existing authorized owner deployment profile'
$Region = 'us-east-1'
$Stack = 'lodestar-fleet-control'
$ChangeSet = 'fleet-control-owner-review'
$ExistingOidcProviderArn = Read-Host 'Existing GitHub OIDC provider ARN, or empty only if verified absent'
$Template = (Resolve-Path 'infra/aws/fleet-control/fleet-control.json').Path
$ExpectedTemplateSha256 = Read-Host 'SHA256 from the approved template review'
$ActualHash = (Get-FileHash -LiteralPath $Template -Algorithm SHA256).Hash.ToLowerInvariant()
if ($ActualHash -cne $ExpectedTemplateSha256.ToLowerInvariant()) { throw 'Template differs from owner review' }
$IdentityJson = aws sts get-caller-identity --profile $OwnerProfile --output json
if ($LASTEXITCODE -ne 0) { throw 'Owner identity unavailable' }
$Identity = $IdentityJson | ConvertFrom-Json
if ($Identity.Account -cne $ExpectedAccountId) { throw 'Wrong AWS account; stop' }
```

After inventory/state reconciliation and explicit approval to **create only the change set**:

```powershell
$Parameters = @(
    "ParameterKey=ExistingOidcProviderArn,ParameterValue=$ExistingOidcProviderArn",
    'ParameterKey=PiaDeliveryRoleArn,ParameterValue='
)
aws cloudformation create-change-set --stack-name $Stack --change-set-name $ChangeSet --change-set-type CREATE --template-body "file://$Template" --parameters $Parameters --capabilities CAPABILITY_NAMED_IAM --region $Region --profile $OwnerProfile
if ($LASTEXITCODE -ne 0) { throw 'Change-set creation failed; stop and inspect, do not retry deployment' }
aws cloudformation wait change-set-create-complete --stack-name $Stack --change-set-name $ChangeSet --region $Region --profile $OwnerProfile
if ($LASTEXITCODE -ne 0) { throw 'Change set is not ready' }
aws cloudformation describe-change-set --stack-name $Stack --change-set-name $ChangeSet --include-property-values --region $Region --profile $OwnerProfile
if ($LASTEXITCODE -ne 0) { throw 'Change set could not be reviewed' }
```

Expected: **11 Adds**, or **10 Adds** when reusing the existing OIDC provider; no Modify/Remove/Replacement of existing resources. Resource types are three buckets, three bucket policies, two named roles, one unattached deny-all policy, one empty retained token shell and the optional OIDC provider. Stop on collisions, unexpected resource changes, drift or any ownership uncertainty. Inspect exact IAM trust/access policies, TLS/public-access blocks, encryption, lifecycle and retention. `CAPABILITY_NAMED_IAM` is acknowledgment, not a permission grant.

Only after a separate approval of that change set, recheck identity and the reviewed change-set ID. Pin execution to its ARN rather than a reusable name:

```powershell
$ReviewedChangeSetArn = Read-Host 'Exact approved change-set ARN'
$ExpectedChangeSetPrefix = "arn:aws:cloudformation:${Region}:${ExpectedAccountId}:changeSet/${ChangeSet}/"
if (-not $ReviewedChangeSetArn.StartsWith($ExpectedChangeSetPrefix, [StringComparison]::Ordinal)) { throw 'Unexpected change-set ARN' }
$IdentityJson = aws sts get-caller-identity --profile $OwnerProfile --output json
if ($LASTEXITCODE -ne 0 -or ($IdentityJson | ConvertFrom-Json).Account -cne $ExpectedAccountId) { throw 'Wrong/unavailable owner identity' }
aws cloudformation execute-change-set --change-set-name $ReviewedChangeSetArn --region $Region --profile $OwnerProfile
if ($LASTEXITCODE -ne 0) { throw 'Execution failed; inspect events' }
aws cloudformation wait stack-create-complete --stack-name $Stack --region $Region --profile $OwnerProfile
if ($LASTEXITCODE -ne 0) { throw 'Stack is not CREATE_COMPLETE; inspect events' }
aws cloudformation describe-stacks --stack-name $Stack --region $Region --profile $OwnerProfile --query 'Stacks[].{Status:StackStatus,Outputs:Outputs}'
```

Keep outputs in private operator configuration. Do not populate tokens or GitHub environment secrets automatically. After AWS bootstrap, secret population, repository OIDC/settings changes, manifest initialization and any Terraform backend/state operation each need the separate approvals below. No `terraform apply` is part of these commands.

The PIA-delivery stack is a separate approval in the PIA secret's actual account: 4 Adds, or 3 with provider reuse. Do not deploy it merely to create the shared backend. Its cross-account grant covers `fleet/fleet.json` only, never Terraform state. Any later fleet stack UPDATE granting that role also requires its own reviewed change set.

## Cost and permissions for owner review

Prospective incremental `us-east-1` baseline: one fleet secret $0.40/month; the optional PIA firewall secret adds $0.40/month. Aggregate S3 Standard storage $0.023/GiB-month; write/list requests $0.005/1,000; reads $0.0004/1,000; Secrets Manager calls $0.05/10,000. At 1 GiB, 1,000 write/lists, 10,000 reads and 10,000 secret calls, expect **$0.482/month**, or **$0.882/month** with the additional PIA shell, before tax/transfer. $1–$2/month is a small-usage allowance, not a cap. [AWS rates](https://docs.aws.amazon.com/solutions/latest/automated-security-response-on-aws/cost.html), [Secrets Manager pricing](https://aws.amazon.com/secrets-manager/pricing/).

IAM and CloudFormation native resource management have no added service fee. No compute, NAT gateway, Lambda, customer KMS key or DynamoDB table is introduced. State noncurrent versions expire after 30 days; evidence after 14 days; control is unversioned. Buckets, bucket policies and secret shells are retained: removing the stack does not remove shared data or necessarily stop charges. Existing AWS services, Linodes and VPN subscription/test costs are excluded.

The existing approved read role needs CloudFormation List/Describe resources/events, S3 bucket metadata/List/Get on identified backend keys, CloudTrail LookupEvents and narrow IAM/secret metadata reads. No secret values or writes are needed for inspection. A future owner execution identity needs scoped CloudFormation change-set permissions plus reviewed creation/configuration of the listed S3, named IAM and secret-shell resources; `iam:PassRole` only if using a particular approved CloudFormation service role. Review read/tag/rollback actions against the exact change set, SCPs and boundaries. The [investigation permission table](2026-11-10-fleet-backend-investigation.md) separates these from runtime state-write permissions.

## Explicit approval gates

1. Confirm canonical account, inventory, recovered state and ownership. Resolve unavailable Linode inventory before a CREATE proposal.
2. Approve the source/security changes and the repository-wide federated-consumer impact review. No PR merge is implied.
3. Approve human preparation of an AWS change set with the exact template hash and listed resource/cost scope.
4. Review its actual ARN, resource delta and IAM policies; separately authorize human execution.
5. Separately authorize native GitHub environment protections/OIDC customization and secure secret population/wiring. Run the read-only audit again. Do not enable live mode until actual issued claims/STS access and the cleanup approval arrangement are verified.
6. Separately review/authorize backend initialization, any state/manifest migration or adoption, then the disposable-Linode integration plan with exact billable delta and cleanup. No crawler, PR merge or speculative remediation is included.
