# Missing fleet-control backend: investigation and owner decision

Investigation date: 2026-11-10 America/Denver (GitHub reads extend into 2026-11-11 UTC).
Working tree: `feat/lodestar-secure-pia-bridge`, `bcf434743c1219192972bf810d86b55b6236afeb`.
This is a read-only investigation and a proposed remediation procedure. No cloud resources, IAM policies, repository settings or secrets were changed. No Terraform apply, backend initialization, state migration, live VPN connection or crawler was run.

## Finding

The shared backend is implemented in source but owner setup is incomplete. There is no verified fleet-control deployment or backend binding in the evidence reviewed. The owner's successful inventory reports no account-owned S3 buckets in the inspected AWS account. Therefore a working account-owned backend cannot be recovered merely by choosing another enabled region or refreshing an observer session.

The evidence strongly supports an unperformed bootstrap. It does **not** establish that a stack was never created and subsequently deleted, or that no backend exists in another account. Neither alternative has affirmative evidence. Do not turn this finding into permission to create a new fleet or discard old private state.

The original plan explicitly permits a dedicated fleet AWS account while PIA secrets stay in their existing account. The canonical fleet account must be established before any bootstrap proposal is approved.

## Evidence and its limits

| Evidence | Verified result | Meaning |
|---|---|---|
| Owner's read-only AWS inspection | Three stacks in `us-east-1`: `lodestar-foundation`, `lodestar-akamai-watchdog`, `lodestar-expiration-guard`; no fleet-control stack found across 17 enabled regions; successful `ListBuckets` returned zero buckets | Supplied by the owner; not independently replayed in this investigation. Confirm pagination, no bucket filters, caller account and inclusion of deleted stacks in the retained evidence. |
| Agent's cached observer identity check | `lodestar-observer-mcp` failed because its observer STS session expired | A failed read supplies no inventory result. No login, administrator fallback or permission change was attempted. |
| Backend implementation history | Commit `c1eb8cb1` introduced templates and S3 backend. [PR #641](https://github.com/dvortsis/lodestar-v2/pull/641) merged 2026-11-09 01:02:14 UTC; its description says “Nothing calls a cloud.” | A merged implementation is not a deployment receipt. |
| Live-mode implementation history | [PR #646](https://github.com/dvortsis/lodestar-v2/pull/646) merged 2026-11-09 03:09:18 UTC; live mode requires `FLEET_LIVE=true` | Implemented capability does not enable cloud operations. |
| CI source | `.github/workflows/ci.yml` rebuilds/tests templates and uses `terraform init -backend=false` for validation | Passing `infra` cannot prove the S3 backend exists or is accessible. No CloudFormation bootstrap deployment command was found in current workflows or the searched available workflow Git history. |
| GitHub configuration | Repository variables: zero; repository secret names: zero. `cloud-fleet` variable and secret counts: zero. `pia-delivery` and `cloud-operator` absent from the environment list | No configured role ARN, backend bucket, control bucket or live-mode flag identifies another account through these paths. Secret values were never requested. |
| Scheduled workflow history | All seven retained schedule runs had blank `FLEET_LIVE`; backend-dependent runs also had blank role/bucket fields. Two issue-comment runs were skipped | Successful schedule runs used the dry-run backend. Their “no fleet” output is not a cloud inventory finding. |
| Deployment records | Seven `cloud-fleet` records correspond to those schedule runs. Latest deployment `7698883902` links to the dry-run reaper job | GitHub environment deployment status is not evidence of AWS stack creation. |
| Source/main comparison | `build_templates.py` blob `108da31a63d670b2e7da6fa99d8821c95a86c378` matches current GitHub `main`; main's workflow still gates live operations and supplies existing bucket secrets | The bootstrap separation is also present on main, not just this PR branch. |
| Local backend metadata | Checkout has no populated backend metadata; `terraform.tfstate` is zero bytes. The former Downloads working directory identified in historical memory is absent at that exact path | Neither proves an idle Linode account. No raw state, private key or credential file was emitted. Other private backups remain unsearched. |
| Operational history | `lodestar-cloud-assets.md` describes deployed guard/watchdog services, not a deployed fleet-control backend, and says original private Terraform state was not moved | The legacy watchdog secret `lodestar/akamai-token` is separate from the proposed fleet token shell. Existing cleanup services do not imply fleet-control was deployed. |

Retained fleet run IDs, all schedule conclusions `success`: `42552145715`, `42609175937`, `42710656351`, `42757040822`, `42824671151`, `42898840522`, `42937463450`. The two issue-comment runs (`42567110215`, `42570535492`) concluded `skipped`, not passing live checks.

The [latest inspected run](https://github.com/dvortsis/lodestar-v2/actions/runs/42937463450) has one successful reaper job and seven skipped jobs. Its log shows blank `FLEET_LIVE`, `AWS_FLEET_ROLE_ARN`, `STATE_BUCKET`, `EVIDENCE_BUCKET`, and `CONTROL_BUCKET`, followed by `reaper: none (no fleet); Linodes over 44 h: 0`. `infra/fleet/fleet.py` selects `DryRunBackend` and an in-memory lock when live mode is false.

[AWS ListBuckets](https://docs.aws.amazon.com/AmazonS3/latest/API/API_ListBuckets.html) lists buckets owned by the authenticated sender. It is not discovery of buckets shared from another account. A successful, unfiltered, fully paginated zero result rules out current general-purpose bucket ownership in that account, including a normally deployed version of these templates. It does not rule out access to an externally owned bucket.

[CloudFormation ListStacks](https://docs.aws.amazon.com/cli/latest/reference/cloudformation/list-stacks.html) includes deleted stacks for 90 days when no status filter is supplied. The source retains all three buckets and their bucket policies on stack deletion/replacement. Even deletion of this stack alone would normally leave its buckets; zero buckets would additionally require no successful bucket creation, separate bucket deletion, or deployment in another account.

## What must exist, and where the name comes from

`infra/aws/fleet-control/README.md` requires an owner-reviewed deployment of `fleet-control.json` in the fleet account. It creates 11 logical resources, or 10 when reusing an existing GitHub OIDC provider:

- Three S3 buckets and three bucket policies: state, evidence and control.
- Two named roles: `LodestarFleetOperator` and `LodestarOperatorBridge`.
- One unattached `LodestarFleetDenyAll` managed policy.
- One retained empty Secrets Manager shell, `lodestar/akamai-fleet-token`.
- Optionally one GitHub OIDC provider.

Bucket names are generated by CloudFormation; there is no committed bucket name to type into the access initializer. Outputs supply `StateBucketName`, `EvidenceBucketName`, `ControlBucketName`, `FleetOperatorRoleArn`, and `BridgeRoleArn`. The stack's physical name is owner-selected; discovery should also inspect outputs/resources rather than searching only for a name containing `fleet-control`.

`backend.tf` only declares an S3 backend with encryption and native S3 locking. It neither creates nor discovers its bucket. Manual scripts require `LODESTAR_TF_STATE_BUCKET` and `LODESTAR_TF_STATE_REGION`; `StateBackend.ps1` fixes the key to `terraform/lodestar.tfstate`. Terraform >=1.10 uses `terraform/lodestar.tfstate.tflock`; no DynamoDB lock table is needed. The automated live runner supplies the same state key, existing `STATE_BUCKET`, and `us-east-1`.

The state bucket also holds `fleet/fleet.json` and `fleet/spend-ledger.json`. Control uses `locks/fleet.json` and `locks/fleet-mismatch.json`. A failed/missing read is not proof that a fleet is absent. Existing fleet ownership, leases and mismatch state must be reconciled before any new deployment.

`pia-delivery.json` is a separate, optional-for-backend stack in the PIA secret's account. It creates one role, one unattached policy, one firewall-token shell and optionally an OIDC provider. Its cross-account access is **only** `GetObject` on `fleet/fleet.json`. It does not establish a cross-account Terraform backend. The existing PIA secret must not be copied or retrieved for this investigation.

## Demonstrated prerequisites before secure live enablement

1. GitHub's OIDC customization endpoint currently returns `use_default=true`, `use_immutable_subject=true`, and an immutable repository prefix. The templates require a legacy-format customized subject containing `repo`, environment context and `job_workflow_ref` at `refs/heads/main`. These formats do not match. [GitHub documents](https://docs.github.com/en/actions/reference/security/oidc) that immutable owner/repository IDs remain in the repository segment even after claim customization. Simply following the old claim-key setup instruction would therefore still need trust-template alignment. Preserve immutable IDs and the exact environment/workflow/main restrictions in a reviewed source change; do not widen `StringEquals` to a wildcard or remove workflow binding. Review impact on every existing federated integration before an owner changes repository OIDC settings.
2. `cloud-fleet` currently has `deployment_branch_policy=null` and no protection rules; the required main-only branch restriction is absent. The PIA/operator environments are not installed. These are owner setup work, not evidence that an existing bucket is hidden.
3. Bucket/role/account-owner configuration and credential shells are not wired into the required GitHub environments. Keep `FLEET_LIVE` off while resolving this. Creating a bucket alone would not complete setup or authorize Linode creation.

No trust code or GitHub settings were changed during this investigation.

## Read-only closure procedure

Use an existing approved read-only identity, in the owner's terminal, with refreshed authentication. The observer documented in this repository is EC2 Describe-only; an authentication refresh does not grant CloudFormation, IAM, S3 or CloudTrail reads. If the needed read role does not exist, stop and request the separately reviewed permission decision below.

1. Record caller identity/account, timestamp, partition and enabled region list. Preserve successful unfiltered/paginated `ListBuckets` results and errors separately. Repeat in each owner-identified candidate fleet account using its approved read role; do not guess accounts or assume arbitrary roles.
2. For every enabled region, run `cloudformation list-stacks` **without a status filter**, including deleted and failed stacks. Inspect resources and selected outputs of the three known stacks and any candidate stack, regardless of its name. Look for all three bucket logical resources, role names and `StateBucketName`/`ControlBucketName`. Follow nested stacks if present. Include retained or failed-creation resources in reconciliation.
3. Read IAM metadata for the named fleet roles, policies and existing GitHub OIDC provider; read only Secrets Manager metadata for the proposed shell names. This detects partial/manual setup and name collisions without secret-value access.
4. Query existing CloudTrail event history from 2026-11-08 through the inspection time, across the relevant regions, for `CreateStack`, `CreateChangeSet`, `ExecuteChangeSet`, `DeleteStack`, `CreateBucket`, and `DeleteBucket`. Emit only event IDs/times/names, selected resource names and account/region metadata. Do not export raw request parameters or `CloudTrailEvent` bodies, which can contain sensitive inputs. Follow candidate stack IDs against their events. [Event history covers 90 days of regional management events](https://docs.aws.amazon.com/awscloudtrail/latest/userguide/view-cloudtrail-events.html); do not enable a new trail, run paid queries, or claim uninspected accounts are covered.
5. Owner locates prior operational state/backups and non-secret backend configuration from their known working directories. Preserve originals. Read backend type/bucket/key/region and resource counts/types through an allowlist; never print state outputs, attributes, private keys or raw JSON. The repository's zero-byte file is not a usable recovery source.
6. If an external bucket is identified, verify its actual owner, region, stack outputs and expected-owner checks using an authorized role in that account. Read the existing state/manifest/ledger/locks in memory through the selected-field procedure. Compare against paginated, read-only Linode inventory in the correct Akamai account before deciding to reuse it. No backend migration or lock write is part of inspection.

Useful read-only commands, with owner-supplied values (do not paste private output into the repository):

```powershell
aws sts get-caller-identity --profile <approved-read-only-profile>
aws ec2 describe-regions --all-regions --profile <approved-read-only-profile>
aws cloudformation list-stacks --region <enabled-region> --profile <approved-read-only-profile>
aws cloudformation list-stack-resources --stack-name <stack-id> --region <region> --profile <approved-read-only-profile>
aws cloudformation describe-stacks --stack-name <stack-id> --region <region> --profile <approved-read-only-profile> --query 'Stacks[].{StackId:StackId,Status:StackStatus,BackendOutputs:Outputs[?OutputKey==`StateBucketName` || OutputKey==`EvidenceBucketName` || OutputKey==`ControlBucketName`]}'
aws s3api list-buckets --profile <approved-read-only-profile> --query 'Buckets[].{Name:Name,CreationDate:CreationDate}'
aws cloudtrail lookup-events --region <region> --profile <approved-read-only-profile> --start-time 2026-11-08T00:00:00Z --lookup-attributes AttributeKey=EventName,AttributeValue=CreateStack --query 'Events[].{Id:EventId,Time:EventTime,Event:EventName,Resources:Resources}'
```

Keep AWS CLI automatic pagination enabled. Check each command's exit code; do not convert `AccessDenied`, expired credentials, incomplete pagination or unavailable regions into an empty result. The CloudTrail example is one event type/region; repeat for the listed event types and regions.

## Remediation decisions and human approval gates

| Gate | Concrete owner decision/action | Required outcome before proceeding |
|---|---|---|
| 1. Establish authority | Identify the canonical fleet AWS account/region and Akamai account, and authorize any additional read-only account inspection | Record account ownership and the evidence above. The design prefers a dedicated fleet account; this investigation does not authorize creating an account or using the inspected account by default. |
| 2. Reconcile existing resources | Recover prior state and compare it with full Linode inventory; resolve existing leases/controller ownership | If a backend or fleet already exists, stop the fresh-bootstrap path and reuse/reconcile it. Any migration, adoption/import, decommission, state write or lock intervention requires a separate reviewed owner decision. Preserve state backups. The older README's clean-start instructions are not approval to delete resources/state now. |
| 3. Review source/settings | Approve a focused OIDC subject/template alignment and required main-only environment restrictions, with matching policy tests and a federated-consumer impact review | Retain immutable IDs, exact audience, workflow and environment binding. No broad trust, administrator fallback or weaker controls. This is separate from cloud deployment approval. |
| 4. Approve backend bootstrap proposal only if absent | Choose the stack name, exact source SHA, account, `us-east-1`, resource list and cost allowance; reuse an existing OIDC provider if present | A future **human** prepares a CREATE change set with `CAPABILITY_NAMED_IAM` using an approved deployment identity. Creating that change set is a write and was not performed here. Existing named roles/secrets/providers must be reconciled before creation. Do not follow the README's force-delete-secret advice; preserve any existing secret and separately review adoption/import or a template adjustment. |
| 5. Approve execution separately | Review the actual change set, parameter values, IAM permissions/trust and retention behavior, then explicitly authorize human execution | For an absent fleet-control stack expect 11 Adds, or 10 with provider reuse, and no modification/removal of existing stacks. Unexpected replacements/removals or collisions stop execution. The optional PIA stack is a separate approval (4 Adds, or 3 with provider reuse). No Terraform apply is involved in AWS bootstrap. |
| 6. Approve wiring and initialization | After successful bootstrap, owner captures actual stack outputs into their secure configuration and approved GitHub environments, verifies bucket policies/versioning/encryption, and sets secret values through the normal secure console process | No credentials in chat, repository or ordinary logs. Terraform backend initialization, any first manifest/ledger write and any state transition need the reconciled ownership decision. Do not fabricate an idle tombstone to bypass unknown inventory. |
| 7. Authorize a live test separately | Review the disposable-Linode test plan, exact Terraform resource delta, time/cost limit and cleanup owner | Keep live mode disabled until all prerequisites and required cleanup tests pass. Linode creation, VPN credential use, tunnel interruption, restart and cleanup require their own explicit authorization. No crawler or merge is included. |

The existing access initializer's bucket prompt should be cancelled until gate 1 establishes a real output. Do not invent a name, create a standalone bucket, change IAM to hunt for a nonexistent bucket, or run Terraform against a fresh state as a workaround.

## Required permissions, separated by purpose

These are permission requirements for review, not permissions granted by this report. Use existing authorized roles where possible; no IAM change was made or is implicitly authorized. Organization SCPs, permission boundaries, bucket policies and explicit denies still apply.

| Purpose | Required access |
|---|---|
| Account/region discovery | Caller identity; `ec2:DescribeRegions`. Optional organization account discovery only if the owner already authorizes it; no Organizations account creation. |
| Stack inspection | `cloudformation:ListStacks`, `DescribeStacks`, `ListStackResources`, `DescribeStackEvents`; optional `GetTemplate` for a candidate template, handled privately. Listing usually needs wildcard resource scope; stack-specific reads should be scoped to candidate stack ARNs. |
| Backend discovery/security metadata | `s3:ListAllMyBuckets`; for an identified bucket, `GetBucketLocation`, `GetBucketVersioning`, `GetEncryptionConfiguration`, `GetBucketPublicAccessBlock`, `GetBucketOwnershipControls`, `GetLifecycleConfiguration`, `GetBucketPolicy`. These do not grant state writes. |
| State reconciliation | `s3:ListBucket` on the identified state/control buckets and `s3:GetObject` on the exact state, manifest, ledger and control keys. `GetObjectVersion`/`ListBucketVersions` only if historical-state recovery is approved. No Put/Delete, lock creation, migration or force-unlock. State is sensitive even when not called a credential. |
| Partial bootstrap/name collision inspection | `iam:ListOpenIDConnectProviders`, `GetOpenIDConnectProvider`, `GetRole`, `ListRolePolicies`, `GetRolePolicy`; `GetPolicy`/`GetPolicyVersion` for named policies. `secretsmanager:DescribeSecret` on the specific shell names; list metadata only if separately needed. **No `GetSecretValue` is needed for this investigation.** |
| Historical deployment proof | `cloudtrail:LookupEvents` in the inspected regions. No trail changes, Lake query creation or paid analysis is needed. |
| Future human-only CloudFormation deployment | Change-set Create/Describe/Execute and stack read/event access, plus an execution identity allowed to create/configure the reviewed S3 buckets/policies, named IAM roles/inline policies/managed policy, optional OIDC provider, and named Secrets Manager shell. Example service actions include `s3:CreateBucket`, `PutBucketPolicy`, `PutBucketVersioning`, `PutEncryptionConfiguration`, `PutBucketPublicAccessBlock`, `PutBucketOwnershipControls`, `PutLifecycleConfiguration`; `iam:CreateRole`, `PutRolePolicy`, `CreatePolicy`, optional `CreateOpenIDConnectProvider`; `secretsmanager:CreateSecret`, plus required read/tag and narrowly scoped rollback operations. Review the final execution policy against the change set; this is not a complete deployable IAM policy. If using a CloudFormation service role, `iam:PassRole` is needed only for that role, conditioned on CloudFormation. `CAPABILITY_NAMED_IAM` acknowledges named resources; it grants no permissions. No administrator grant to the observer. |
| Future runtime backend access | Terraform needs ListBucket and Get/Put on the fixed state key; native locking needs Get/Put/Delete on its `.tflock`. Fleet runtime additionally needs the separately reviewed manifest, ledger, control and evidence grants already represented in the template. Human secret population and runtime GetSecretValue are distinct from read-only discovery. SSE-S3 needs no customer KMS key or DynamoDB permissions. |

GitHub inspection used environment/variable/secret **names and counts**, OIDC settings, workflow logs and deployment metadata. Future setting/secret changes require an authorized repository administrator or appropriately scoped fine-grained access; current read access is not write approval.

## Estimated incremental recurring cost

Assumptions: `us-east-1`, S3 Standard, ordinary small-state/evidence usage, no replicas, no new compute, no free-tier/credit discount, before tax and transfer. These are prospective costs, not a claim about the account's actual bill.

| Component | Monthly estimate |
|---|---|
| Fleet-control token secret | $0.40 per active stored secret-month |
| Optional PIA-delivery firewall-token secret | Additional $0.40/month; existing PIA master/watchdog secrets are outside this incremental estimate |
| Aggregate S3 storage, including retained state versions | $0.023 per GiB-month for the first pricing tier |
| S3 PUT/COPY/POST/LIST requests | $0.005 per 1,000 |
| S3 GET and comparable read requests | $0.0004 per 1,000 |
| Secrets Manager API calls | $0.05 per 10,000 |
| IAM roles/policies/OIDC and native AWS CloudFormation resources | No additional service fee; no DynamoDB table, customer KMS key, EC2, NAT gateway or Lambda is in these bootstrap templates |

Example: 1 GiB aggregate storage, 1,000 write/list requests, 10,000 S3 reads and 10,000 Secrets Manager calls per month gives **$0.482/month** for fleet-control, or **$0.882/month** including the additional PIA-delivery shell. A **$1–$2/month working allowance** is reasonable for that small usage model; it is not a spending cap. More evidence, retained versions, requests or data transfer can raise it. Consider the plan's owner-approved $10/month AWS budget alert separately; an alert does not prevent spending.

State's noncurrent versions expire after 30 days; evidence current objects expire after 14 days; control is unversioned. Buckets/policies and secret shells are retained on stack removal, so deletion of the stack does not finish cleanup or necessarily stop recurring storage/secret charges. Do not delete shared state as disposable test cleanup.

Pricing checked against [AWS Secrets Manager pricing](https://aws.amazon.com/secrets-manager/pricing/), [AWS's published S3/request cost table](https://docs.aws.amazon.com/solutions/latest/automated-security-response-on-aws/cost.html), [S3 pricing](https://aws.amazon.com/s3/pricing/), [IAM FAQ](https://aws.amazon.com/iam/faqs/), and [CloudFormation pricing](https://aws.amazon.com/cloudformation/pricing/). Linode/PIA test expenses and existing guard/watchdog services are excluded from this backend-only estimate; their authorization remains separate.

## Validation and disposition

- `python infra/aws/fleet-control/build_templates.py --check`: passed.
- `python -m unittest discover -s infra/aws/fleet-control -p 'test_*.py'`: **22 passed**.
- Gitleaks 8.30.1 scan of this report: passed, no leaks found. Git whitespace checks and an explicit trailing-whitespace check of the new file found no errors.
- These are source/policy tests; no live deployment, remote state access or cross-account discovery was validated by them.

The verified defect is the assumption that a documented/merged backend must already exist. Source and workflow evidence explain why CI success did not produce one. “Never deployed in any account” remains unproven until the owner completes historical and candidate-account reads. The next decision is account/state ownership and read-only evidence closure, followed only then by separately authorized setup. No infrastructure or IAM change, Terraform apply, live test or additional speculative remediation is authorized by this report.
