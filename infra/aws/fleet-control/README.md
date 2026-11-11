# Fleet control: AWS stacks

The AWS side of the cloud fleet control plane (OP-2a). Plan: `docs/plan/2026-11-08-cloud-operator.md` §3–§4 and "Owner setup" step 4. Decisions: `docs/decisions/2026-11-08-OP-2a.md`.

**The owner deploys these, never CI or an agent.** Review a change set before each create or update.

Implementation in Git does not mean either stack exists. Missing or unreadable backend/state is a deployment blocker, never proof of an idle fleet. Use the existing authoritative state and inventory; do not initialize a replacement fleet from an empty checkout.

The current bootstrap procedure, required approvals, command preparation and cost model are in [the owner bootstrap runbook](../../../docs/operations/fleet-control-bootstrap.md). `github-deployment-policy.json` is the reviewed desired configuration; it does not change GitHub settings. The read-only audit is:

```powershell
python infra/fleet/deployment_policy.py audit
```

The provisioning/PIA/operator environments require selected branch `main` only (no tags), the repository owner as sole required reviewer, explicit `prevent_self_review=false`, and administrator approval bypass disabled. Blocking self-review would prevent the sole owner from approving their own dispatch. An owner must separately approve and install these settings. The live runner refuses AWS role assumption if its environment or OIDC configuration differs or cannot be read. Expiration and emergency teardown are separated from provisioning approval: see [the independent cleanup design](../../../docs/operations/fleet-control-cleanup.md). The separate `fleet-cleanup` environment admits only owner-dispatched emergency invocation, never provisioning, and has no approval/timer queue.

OIDC must retain immutable owner/repository IDs and use `include_claim_keys` in the exact order `repo`, `context`, `workflow_ref`, with `use_default=false` and `use_immutable_subject=true`. The subject binds the direct workflow through `workflow_ref`. Tokens also carry `job_workflow_ref`: it names the reusable workflow when one runs the job, and otherwise equals `workflow_ref`. It is not a substitute, and the runner refuses a token whose `job_workflow_ref` differs from the expected `workflow_ref`. For this repository, a fleet subject is:

```text
repo:dvortsis@83323977/lodestar-v2@1427502271:environment:cloud-fleet:workflow_ref:dvortsis/lodestar-v2/.github/workflows/fleet.yml@refs/heads/main
```

Every template trust uses exact `StringEquals` for this subject shape and `aud=sts.amazonaws.com`. Review all federated consumers before a separately authorized GitHub customization change. Never relax trust to accept a default subject or wildcard. The runtime also checks actual issued token claims before STS; STS remains responsible for signature validation. No live JWT or AWS authentication has been verified merely by testing the templates.

| Template | Account | Creates |
|---|---|---|
| `fleet-control.json` | the fleet's AWS account | the GitHub OIDC provider (unless one exists), `LodestarFleetOperator`, `LodestarOperatorBridge`, the state, evidence and control buckets, the empty `lodestar/akamai-fleet-token` secret, the unattached `LodestarFleetDenyAll` policy |
| `pia-delivery.json` | the account that holds `lodestar/pia-vpn` | `LodestarPiaDelivery`, the empty `lodestar/akamai-firewall-token` secret, the unattached `LodestarPiaDenyAll` policy |

Both use named IAM resources, so a deploy needs `CAPABILITY_NAMED_IAM`. Neither holds an account ID: they use AWS pseudo-parameters and the parameters below.

The separate `cleanup.json` stack creates the independently scoped brokers, invocation-only role, dedicated empty credential shell, logs and alerts. It reuses the same-account OIDC provider and existing buckets; its expiration schedule defaults to disabled. It requires its own owner review/authorization. See the cleanup runbook for 13 expected Adds, costs, exact commands and retained-resource limitations.

## Deploy, in order (us-east-1)
1. **`fleet-control.json`.** Parameters:
   - `ExistingOidcProviderArn`: leave empty, unless the account already has a `token.actions.githubusercontent.com` provider (an account holds one per URL); then give its ARN.
   - `PiaDeliveryRoleArn`: leave empty for now.
2. **`pia-delivery.json`.** Parameters:
   - `ExistingOidcProviderArn`: as above, for this account. If it's the same account as step 1, give the provider step 1 created.
   - `PiaSecretArn`: the exact ARN of `lodestar/pia-vpn`, with its six-character suffix.
   - `StateBucketName`: step 1's `StateBucketName` output.
   - `FleetAccountId`: the 12-digit ID of step 1's account. The `fleet.json` grant holds only for a bucket in that account. Type it into the console; never commit it.
3. **Only if the two stacks are in different accounts:** update the `fleet-control` stack with `PiaDeliveryRoleArn` set to step 2's `PiaDeliveryRoleArn` output. That grants the role `s3:GetObject` on `fleet/fleet.json` alone.
4. Check Secrets Manager metadata for name collisions before preparing the change set. If either named secret already exists or is pending deletion, **stop and preserve it**. Establish its owning stack and consumers. The owner must separately review a supported resource-import/adoption procedure or a template change to reference the existing secret. Do not replace a secret to make bootstrap succeed, change its deletion schedule, or discard recoverable values. Only after an approved deployment creates a new empty shell may the owner populate it through the secure console process (owner setup steps 5 and 6.3), never through a template parameter, file, log or chat. Do not retrieve existing secret values for this inspection.
5. Copy the outputs into the GitHub environment secrets each output's description names.

If `LodestarPiaDelivery` is ever deleted and recreated, in either account, update the `fleet-control` stack again with its ARN (step 3): a bucket policy names a role by its internal ID, so it otherwise keeps pointing at the old role.

## Build and test
The JSON is generated: edit `build_templates.py`, then run it. CI fails when the committed JSON differs from what the builder makes.

```
python3 infra/aws/fleet-control/build_templates.py
python3 -m unittest discover -s infra/aws/fleet-control -p 'test_*.py'
cfn-lint --region us-east-1 --template infra/aws/fleet-control/fleet-control.json infra/aws/fleet-control/pia-delivery.json infra/aws/fleet-control/cleanup.json
cfn-guard validate --rules infra/aws/fleet-control/deployment.guard --data infra/aws/fleet-control/fleet-control.json --data infra/aws/fleet-control/pia-delivery.json --data infra/aws/fleet-control/cleanup.json --output-format json
```

The independent Python policy audit and its negative mutations run in CI. The local schema/security checks above were validated with cfn-lint 1.60.0 and CloudFormation Guard 3.2.1; Guard has no implicit ruleset. The live deployment-policy audit is a separate check of unchanged GitHub settings, not an AWS deployment or a CI prerequisite requiring credentials.
