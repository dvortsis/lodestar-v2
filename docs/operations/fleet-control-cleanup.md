# Fleet-control approval and cleanup separation

This is a proposed source change, not a deployed service. No AWS/Linode resources, IAM, secrets, GitHub settings or Terraform state were changed while preparing it. The original PR #632 work remains in its original working tree; this proposal starts from `main`.

## Authorization model

| Operation | Identity and authorization | Available authority |
|---|---|---|
| Provisioning, PIA delivery, operator work | Existing exact main-workflow OIDC roles; selected main branch; sole owner required reviewer; administrator bypass disabled | Existing reviewed fleet provisioning/PIA scopes. Native approval must occur before job execution. |
| Scheduled expiration | Separately owner-deployed EventBridge rule invokes only the expiration Lambda | Exact registered Linode deletion after expiration or a confirmed security tripwire; expiration has no GitHub dependency or Terraform access. Disabled by default. |
| Emergency teardown | Owner-only, first-attempt `workflow_dispatch` of `fleet-emergency-down.yml` at main; `fleet-cleanup` environment has main-only branch restrictions and no reviewer/timer queue | `LodestarFleetCleanupInvoker` may invoke only the emergency Lambda. It cannot read credentials, state, or invoke the expiration/other functions. |
| Broker execution | Separate Lambda service role and dedicated cleanup credential | Read one authorization object, set one mismatch flag, read its own token and write its own logs. No Terraform state, IAM, provisioning or credential access in the GitHub invoker. |

Installing/enabling the cleanup stack is a separate owner authorization. The emergency dispatch is itself the owner's explicit teardown request; it does not wait for a second environment approval. The role's exact workflow/main/environment trust, workflow actor gate, runtime context check and signed token claim checks all constrain that request. The client rejects requests queued for over ten minutes, prepares a non-secret authorization version and executes only that version. The server also checks request freshness and that all registered resources predate the request. Changed registry versions and delayed/replayed requests fail; the invoke client waits for the broker and never automatically retries. Re-runs and schedule/PR/comment events cannot take this emergency path. `FLEET_CLEANUP_ENABLED` is an independent owner-installed switch for emergency invocation; it does not control expiration.

The existing `fleet.yml` no longer schedules cleanup or accepts manual teardown. Its owner-gated provisioning jobs remain protected. Live `/fleet down`, legacy `reap`/`dispatch-down`, and the old tag-wide `LiveBackend` delete methods refuse before cloud access/deletion. Dry-run decision models remain for pure tests; they are not an alternate live execution path. The retired live fallback tests have been replaced with broker scope/expiration/mutation/refusal tests.

AWS trust also checks the cleanup caller's immutable owner `actor_id`, repository ID, main `ref` and environment directly, in addition to the exact customized subject/audience. [AWS documents these GitHub condition keys](https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_policies_iam-condition-keys.html). Thus another actor cannot assume the invoker role merely by changing a workflow's job-level actor gate. Direct `workflow_ref` remains carried in the customized subject; the design does not substitute the reusable-only `job_workflow_ref` claim.

## Resource boundary

After an approved apply, the controller obtains exact VM IDs from Terraform output, checks their API identity/account/address/region/type/ownership/creation time and firewall bindings, then writes `fleet/cleanup-authorization.json` in the existing versioned state bucket. Registration failure marks the fleet mismatched and fails the provisioning job. A partial apply that never yields a valid registration is **not automatically adoptable**; it requires owner inventory/recovery. The independent broker never searches for resources to delete by tag alone.

The authorization contains account UUID, fixed region `us-east`, deployment label `fleet`, creation/expiration timestamps, exact VM IDs/types/labels/creation timestamps, and corresponding firewall IDs/labels/creation timestamps, approved admin `/32`s and member IDs. Expiry must be after creation and at most 44 hours later. API creation timestamps must match exactly, fall after the owner-selected activation floor, and fit the recorded creation window. At most two nodes/firewalls are accepted, with unique main/worker roles matching the current preset. Missing, malformed, future, unbounded or partially readable authorization fails closed.

The broker additionally checks:

- Actual AWS execution account against its CloudFormation-pinned account, `us-east-1`, same-account S3 policies and `ExpectedBucketOwner` on every S3 call.
- Actual Linode account UUID at planning and before each VM deletion; exact API region, approved type, labels and all `lodestar`, `test`, `managed-terraform` tags.
- Every authorized firewall's identity and its complete bounded device inventory. Any foreign device, changed resource, incomplete pagination or unreadable API prevents the affected cleanup. The entire initial target set is validated before the first write.
- Current authorization ETag with `IfMatch` before mutation. Replaced/revoked authorization stops further work.
- Remaining invocation time before starting deletion, and confirmation that each requested VM returns 404. Unconfirmed deletion fails and never counts as success.

The expiration function cannot select early teardown from its input. It retains the original pure firewall auditor: disabled/default-ACCEPT firewalls, unapproved ingress, duplicate job rules or confirmed stale job ingress independently authorize a scoped security teardown before expiry. Unlike the old reaper it does not rewrite firewall rules; confirmed stale ingress terminates the affected registered fleet rather than attempting an unfenced rule-set repair. Unknown/rate-limited audit data marks the fleet mismatched and alerts without authorizing early deletion. Public GitHub metadata is cached per invocation; expiration and owner-emergency teardown skip this dependency. The emergency function may delete before expiry only within the same account/region/ownership/creation boundaries; its mode is fixed in its separately authorized deployment. Caller payload cannot supply targets, token, URLs, scope or mode. `dry_run=true` only reduces authority and never writes the flag or deletes resources. STS authenticates GitHub JWT signatures; local decoded-claim checks are consistency checks, not signature verification.

Before deletion the broker sets `locks/fleet-mismatch.json` to true. It never clears that flag, updates Terraform state, writes an idle tombstone or migrates state. Successful compute teardown does not authorize recreation. Owner reconciliation must confirm inventory, retained resources and state/ledger/manifest ownership before another provision.

## Deliberately retained resources

The broker **never deletes firewalls, volumes, reserved IPs, buckets, secrets or Terraform state**. Cloud Firewalls can protect multiple services and are not a VM-region boundary. A read of their attachments is not an atomic deletion condition. The cleanup token therefore needs only `firewall:read_only`, alongside `account:read_only` and the Linode permission needed to delete the registered VMs. Its API adapter only permits GET and exact Linode DELETE paths; POST, PUT, arbitrary URLs/redirects and firewall DELETE are rejected. The token is never delivered to GitHub. Linode's token scopes are broader than HTTP DELETE; the private broker supplies that narrower method/target boundary.

[Akamai documents Cloud Firewalls as available at no charge and attachable to multiple services](https://techdocs.akamai.com/cloud-computing/docs/cloud-firewall). Retained firewalls do not leave compute running, but still require separately approved owner cleanup. Full cloud cleanup is not claimed until that cleanup and state reconciliation are independently verified. Concurrent external writers remain an operational risk: use the dedicated fleet account and prohibit uncoordinated ownership/attachment changes; API reads/deletes are not a cross-resource transaction.

## Failure reporting and rollback

Two Lambda Errors alarms and a missing-success heartbeat alarm notify an existing owner-approved SNS topic. Selected target confirmations and fixed failure summaries go to retained 14-day log groups. Provider response bodies, exception text, tokens and raw authorization/state never enter public logs. A timeout, unreadable state, failed delete or missing credential is not an empty fleet. A partial failure leaves the mismatch flag set; confirmed deletions are logged and a later invocation rechecks each exact ID. No attached/shared firewall is deleted on a partial failure.

The schedule starts disabled (`EnableExpiration=false`). Owner setup must verify SNS delivery/topic policy, heartbeat behavior, function credentials, the exact authorization and a disposable canary before a separate change set enables it. Existing deployed guard/watchdog services were not modified or re-verified. Before migration the owner must inspect their actual account/region/tag/activation scopes and coordinate retirement or coexistence; this PR cannot certify the live legacy watchdog or its token's account.

## Owner deployment procedure — commands prepared only

First review/merge this draft through the normal owner process; the agent does not merge it. Establish the proposed canonical account through the private owner configuration (the user selected account ending `8045`), confirm live Linode/state inventory, and resolve any existing stacks, retained resources and secret-name collisions. Do not create another fleet/state backend if one already exists.

Deploy fleet-control's backend only through the separately approved procedure in [the bootstrap runbook](fleet-control-bootstrap.md). Cleanup is a separate stack with **13 Adds**: two Lambda functions, two IAM roles, two retained log groups, one retained empty cleanup secret, one schedule, one scoped invocation permission, one heartbeat metric filter and three alarms. It creates no GitHub provider, bucket, VM or Terraform state. Its provider ARN is constructed from the stack's own account and must already exist. Its execution role cannot access the provisioning token or Terraform state. Notification ARNs are also built from the stack account and region. The heartbeat alarm may alert during the intentionally disabled commissioning period; that is not proof of failed deletion.

The owner supplies these private, non-secret parameters from verified outputs/inventory. Do not infer bucket names, account UUID, activation floor or notification topic from this repository. Preserve existing named secrets/roles; stop on collisions and review adoption/reference options.

```powershell
$OwnerProfile = Read-Host 'Existing authorized owner deployment profile'
$ExpectedAccountId = Read-Host 'Approved canonical AWS account ID'
$Region = 'us-east-1'
$Stack = 'lodestar-fleet-cleanup'
$ChangeSet = 'cleanup-owner-review'
$StateBucket = Read-Host 'Verified fleet StateBucketName output'
$ControlBucket = Read-Host 'Verified fleet ControlBucketName output'
$LinodeAccount = Read-Host 'Verified intended Linode account euuid'
$Activation = Read-Host 'Approved activation floor in UTC YYYY-MM-DDTHH:MM:SSZ'
$Topic = Read-Host 'Existing same-account notification topic NAME with confirmed delivery'
$Template = (Resolve-Path 'infra/aws/fleet-control/cleanup.json').Path
$ExpectedHash = Read-Host 'SHA256 of the exact reviewed cleanup template'
if ((Get-FileHash -LiteralPath $Template -Algorithm SHA256).Hash.ToLowerInvariant() -cne $ExpectedHash.ToLowerInvariant()) { throw 'Template differs from owner review' }
$IdentityJson = aws sts get-caller-identity --profile $OwnerProfile --output json
if ($LASTEXITCODE -ne 0 -or ($IdentityJson | ConvertFrom-Json).Account -cne $ExpectedAccountId) { throw 'Wrong/unavailable owner identity' }
$Parameters = @(
  "ParameterKey=StateBucketName,ParameterValue=$StateBucket",
  "ParameterKey=ControlBucketName,ParameterValue=$ControlBucket",
  "ParameterKey=LinodeAccountEuuid,ParameterValue=$LinodeAccount",
  "ParameterKey=MinCreatedUtc,ParameterValue=$Activation",
  "ParameterKey=NotificationTopicName,ParameterValue=$Topic",
  'ParameterKey=EnableExpiration,ParameterValue=false'
)
```

After explicit approval to **create only the change set**, a human may run:

```powershell
aws cloudformation create-change-set --stack-name $Stack --change-set-name $ChangeSet --change-set-type CREATE --template-body "file://$Template" --parameters $Parameters --capabilities CAPABILITY_NAMED_IAM --region $Region --profile $OwnerProfile
if ($LASTEXITCODE -ne 0) { throw 'Creation failed; inspect, do not deploy' }
aws cloudformation wait change-set-create-complete --stack-name $Stack --change-set-name $ChangeSet --region $Region --profile $OwnerProfile
if ($LASTEXITCODE -ne 0) { throw 'Change set is not ready' }
aws cloudformation describe-change-set --stack-name $Stack --change-set-name $ChangeSet --include-property-values --region $Region --profile $OwnerProfile
```

Review the actual ARN, 13 Adds, all IAM policies, same-account provider/S3/notification configuration and fixed modes. Stop on any unexpected changes/removals/replacements or unknown ownership. Execution needs a second explicit approval; pin it to the approved ARN:

```powershell
$ReviewedChangeSetArn = Read-Host 'Exact separately approved change-set ARN'
if (-not $ReviewedChangeSetArn.StartsWith("arn:aws:cloudformation:${Region}:${ExpectedAccountId}:changeSet/${ChangeSet}/", [StringComparison]::Ordinal)) { throw 'Unexpected ARN' }
$IdentityJson = aws sts get-caller-identity --profile $OwnerProfile --output json
if ($LASTEXITCODE -ne 0 -or ($IdentityJson | ConvertFrom-Json).Account -cne $ExpectedAccountId) { throw 'Wrong/unavailable owner identity' }
aws cloudformation execute-change-set --change-set-name $ReviewedChangeSetArn --region $Region --profile $OwnerProfile
if ($LASTEXITCODE -ne 0) { throw 'Execution failed; inspect events' }
aws cloudformation wait stack-create-complete --stack-name $Stack --region $Region --profile $OwnerProfile
```

No command above was executed. Subsequent owner actions, each requiring separate authorization:

1. Populate the dedicated cleanup credential through the secure console process, after verifying account and least-privilege scopes; no values in chat/Git/logs.
2. Install the reviewed OIDC customization and environment policies. Keep owner approvals for provisioning/PIA/operator environments. Configure `fleet-cleanup` for selected `main` only with no tags, reviewer/timer/custom-approval queue, and no administrator bypass. Set its role/function outputs and the independent emergency switch. No provisioning secrets belong there.
3. Independently inspect live account inventory, state and legacy cleanup services. Verify exact created-ID registration, read-only broker validation, signed JWT/STS claims, negative owner/branch/re-run cases, alert delivery and a disposable deletion canary. **No live access/canary was available in this remediation.**
4. Only after those checks, approve a separate UPDATE change set setting `EnableExpiration=true`; verify recurring heartbeat. Do not enable live provisioning without this independently verified cleanup arrangement.
5. For an emergency, the owner intentionally dispatches the pinned main workflow, which has no target inputs. Its two internal calls prepare and then execute one version of the already registered authorization; neither can choose a different fleet or supply targets. Inspect its conclusion and broker logs/alarms. Confirm every registered VM is gone, separately remove only verified retained firewalls, then explicitly reconcile state/ledger/manifest/authorization and clear the mismatch flag. No automatic state repair or Terraform apply/destroy is authorized here.

## Cost and remaining live limits

The separate cleanup stack adds one secret (about $0.40/month), one custom heartbeat metric (about $0.30/month), three standard alarms (about $0.30/month), and Lambda/log/S3/SNS/EventBridge usage. At low usage, budget roughly **$1/month plus usage** in addition to the backend; do not treat this as a cap or an actual bill. [Secrets pricing](https://aws.amazon.com/secrets-manager/pricing/), [CloudWatch pricing](https://aws.amazon.com/cloudwatch/pricing/), [Lambda pricing](https://aws.amazon.com/lambda/pricing/). Retained secrets/logs may continue charging after stack removal. Existing services, Linodes and PIA are excluded.

Local unit/mutation/schema/security checks prove the proposed code's behavior under tested inputs. They do not prove that a backend, cleanup stack, intended cloud account, restricted token, notification path or native GitHub protections exist. Live inventory/ownership, actual JWT issuance/STS trust, Linode deletion semantics/permissions, notification delivery, concurrent-writer controls and full retained-resource/state cleanup remain deployment gates.

## Local validation record

The dedicated branch was checked without cloud credentials or deployment operations:

- Fleet control plane: 182 tests; PIA bridge: 38 tests.
- Existing Akamai watchdog: 26 tests; AWS expiration guard: 15 tests, with fake clients only.
- CloudFormation/broker suite: 43 tests, including 40 independent policy-widening mutations across the IAM audits and packaged-Lambda execution checks.
- Reproducible template generation on Windows and Linux; cfn-lint 1.60.0 and explicit CloudFormation Guard 3.2.1 rules across all three fleet-control templates.
- Terraform formatting and backend-disabled initialization/validation; the pure PowerShell StateBackend tests and PowerShell syntax parsing.
- Gitleaks 8.30.1 on every changed/new file and Git whitespace checks.

Read-only GitHub inspection still reports default OIDC subjects and an unprotected `cloud-fleet` environment. The deployment-policy audit is expected to refuse this live configuration. These are unresolved commissioning blockers, not successful live security checks. No signed JWT/STS exchange, service-side CloudFormation deployment validation or live Linode canary was performed.
