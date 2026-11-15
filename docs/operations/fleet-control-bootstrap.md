# Fleet-control bootstrap

This page covers the AWS side of the cloud fleet: what the deployed stacks hold, where their outputs go, the GitHub settings they rely on, and how to change them safely. All three stacks live in `us-east-1` and are built by `infra/aws/fleet-control/build_templates.py`.

| Stack | Template | State |
|---|---|---|
| Fleet-control | `fleet-control.json` | **Deployed**, at the owner's request |
| PIA delivery | `pia-delivery.json` | **Deployed**, at the owner's request |
| Cleanup | `cleanup.json` | **Not deployed yet.** See [Deploy the cleanup stack](#deploy-the-cleanup-stack) at the end. |

Keep account IDs, bucket names and ARNs in your private notes and GitHub environment secrets, never in the repository. No agent or CI deploys or changes these stacks: you do, in the AWS console, after reviewing a change set.

## What the deployed stacks hold

**Fleet-control** (11 resources, or 10 if it reused an existing GitHub OIDC provider):
- three private, encrypted buckets, kept if the stack is deleted, each with a TLS-only bucket policy:
  - **state**: Terraform state, the fleet manifest, the spend ledger and the cleanup authorization. Versioned; old versions expire after 30 days.
  - **evidence**: run evidence, expiring after 14 days.
  - **control**: locks and flags, unversioned.
- `LodestarFleetOperator`, the provisioning role, usable only by `fleet.yml` on `main` in the `cloud-fleet` environment;
- `LodestarOperatorBridge`, the operator's role, usable only by the three operator workflows on `main` in `cloud-operator`;
- `lodestar/akamai-fleet-token`, the fleet's Linode token (the first of three tokens);
- an unattached deny-all policy, for emergencies;
- the GitHub OIDC provider, unless one already existed.

**PIA delivery** (4 resources, or 3 with provider reuse): the `LodestarPiaDelivery` role (`fleet.yml` on `main` in `pia-delivery`), `lodestar/akamai-firewall-token` (the second token), its own deny-all policy and, if needed, the OIDC provider. Its role reads the PIA secret and `fleet/fleet.json`, never Terraform state.

Each role names the exact object keys it may use. No role has wildcard access to a bucket, and the provisioning role can't read the cleanup token.

## Where the outputs go

| Output | GitHub environment secret | Environments |
|---|---|---|
| StateBucketName | `STATE_BUCKET` | `cloud-fleet`, `pia-delivery` (and `cloud-operator` for the injection test) |
| EvidenceBucketName | `EVIDENCE_BUCKET` | `cloud-fleet` |
| ControlBucketName | `CONTROL_BUCKET` | `cloud-fleet`, `cloud-operator` |
| FleetOperatorRoleArn | `AWS_FLEET_ROLE_ARN` | `cloud-fleet` |
| BridgeRoleArn | `AWS_BRIDGE_ROLE_ARN` | `cloud-operator` |
| PiaDeliveryRoleArn (PIA stack) | `AWS_PIA_ROLE_ARN` | `pia-delivery` |
| CleanupRoleArn, EmergencyFunctionArn (cleanup stack) | `AWS_CLEANUP_ROLE_ARN`, `AWS_CLEANUP_FUNCTION_ARN` | `fleet-cleanup` |

## GitHub settings the roles rely on

AWS trusts a GitHub job only if its OIDC token names this repository by its immutable IDs, one approved environment and an exact workflow file on `main`. The settings live in `infra/aws/fleet-control/github-deployment-policy.json`. The read-only audit checks them and must pass before live use:

```
python infra/fleet/deployment_policy.py audit
```

| Environment | Branches | Required reviewer | Admin bypass |
|---|---|---|---|
| `cloud-fleet` | `main` only (one branch rule) | You, alone; **Prevent self-review** unticked | Off |
| `pia-delivery` | `main` only | You, alone; **Prevent self-review** unticked | Off |
| `cloud-operator` | `main` only | None, or you alone (owner decision, 2026-11-14) | Off |
| `fleet-cleanup` | `main` only | **None**, and no wait timer: your dispatch is the approval | Off |

Why **Prevent self-review** stays unticked: you're the only reviewer, so ticking it would stop you approving runs you started, and provisioning would deadlock. Adding any other reviewer breaks the sole-owner rule. Keep `FLEET_LIVE` off until the audit passes and cleanup is deployed and tested.

## Changing a deployed stack

- **Always review a change set first.** In the console: **CloudFormation ▸ Stacks ▸ (stack) ▸ Stack actions ▸ Create change set for current stack**. Read every row before **Execute change set**. Stop on any Remove or Replacement you didn't expect.
- **Never delete a retained bucket, secret or log group** to make a name free or "start clean". They hold state and evidence, and deleting the stack leaves them in place on purpose.
- **Update templates from the repository only.** Regenerate them with `build_templates.py`, never edit the JSON by hand. CI checks they match (`build_templates.py --check`).

## Cost

Fleet-control and PIA delivery cost about **$0.50–$1 a month** at low use: two secrets ($0.40 each) plus small S3 and API charges. The cleanup stack adds about **$1–2 a month plus usage** ([details](fleet-control-cleanup.md#cost)). Retained buckets and secrets keep their small charges until you delete them yourself.

## Deploy the cleanup stack

The cleanup stack deletes the fleet's VMs at expiry or in an emergency. The full design, its checks and alerts are in [Fleet cleanup stack](fleet-control-cleanup.md). These are the console steps, the same as on that page, which also has the change-set review table.

1. **Gather five values** (keep them private):
   1. In the AWS console, set the region to **US East (N. Virginia) us-east-1**. Open **CloudFormation ▸ Stacks ▸** the fleet-control stack **▸ Outputs**. Copy **StateBucketName** and **ControlBucketName**.
   2. In **Simple Notification Service ▸ Topics**, copy the **Name** of a topic with a **Confirmed** email subscription. If there isn't one, create a **Standard** topic named `lodestar-alerts`, add an **Email** subscription, and confirm it from your inbox.
   3. Your Linode account `euuid`, from your private fleet configuration (`linode-cli account view --format euuid --text --no-headers`).
   4. An activation time: the current UTC time, rounded down to the minute, like `2026-11-15T18:00:00Z`. Cleanup refuses any VM created before it.
   5. `infra/aws/fleet-control/cleanup.json` from `main` (GitHub ▸ the file ▸ **Download raw file**). If the runner gave you a SHA-256, check it with `Get-FileHash .\cleanup.json`.
2. **Create a change set.** Open **CloudFormation ▸ Create stack ▸ With new resources (standard)**.
   1. Choose **Choose an existing template ▸ Upload a template file ▸ Choose file ▸** `cleanup.json`, then **Next**.
   2. Set **Stack name** to `lodestar-fleet-cleanup`. Fill in the parameters: the two bucket names, `EnableExpiration` = `false`, your `euuid`, your activation time and the topic name. Choose **Next**.
   3. Keep the defaults, including **Roll back all stack resources**. Tick **I acknowledge that AWS CloudFormation might create IAM resources with custom names**, then **Next**.
   4. Choose **Create change set** (not **Submit**). Name it `cleanup-owner-review`, then **Create change set**.
3. **Review it.** When it shows **CREATE_COMPLETE**, the **Changes** tab must list **16 rows, all Add**, matching the table in [Create a change set and review it](fleet-control-cleanup.md#create-a-change-set-and-review-it). On the **Template** tab, `ReservedConcurrentExecutions` must not appear. Anything else: **Delete change set**, stop, and tell the runner.
4. **Create the stack.** Choose **Execute change set ▸ Execute change set**. Wait for **CREATE_COMPLETE** on the **Events** tab. Copy **CleanupRoleArn** and **EmergencyFunctionArn** from **Outputs**.
5. **Install the third token.**
   1. In Akamai Cloud Manager, open **API Tokens ▸ Create a Personal Access Token**. Name it `lodestar-cleanup`. Give it **Account: Read Only**, **Firewalls: Read Only**, **Linodes: Read/Write** and **No Access** to everything else. Copy the token.
   2. In AWS, open **Secrets Manager ▸ lodestar/akamai-cleanup-token ▸ Set secret value** (or **Retrieve secret value ▸ Edit**). Choose **Plaintext**, paste only the token, then **Save**.
   3. Test it: open **Lambda ▸ lodestar-fleet-cleanup-emergency ▸ Test**, with the event `{"dry_run": true}`. With no fleet yet, it fails at `"stage": "authorization"` and the alarm emails you within about 5 minutes. That proves alerts work. Choose **Test** again within a minute: it must stop at `authorization` again, not `busy`. Repeat both tests on `lodestar-fleet-cleanup-expiration`. A failure at `lease`, `busy`, or a `cleanup_lease_release_failed` log line: tell the runner.
6. **Create the `fleet-cleanup` environment.**
   1. In GitHub, open **Settings ▸ Environments ▸ New environment**. Name it `fleet-cleanup`.
   2. Leave **Required reviewers** and **Wait timer** off, and untick **Allow administrators to bypass configured protection rules**.
   3. Under **Deployment branches and tags**, choose **Selected branches and tags** and add one rule: **Branch**, `main`.
   4. Add the environment secrets `AWS_CLEANUP_ROLE_ARN` (CleanupRoleArn) and `AWS_CLEANUP_FUNCTION_ARN` (EmergencyFunctionArn), and the environment variable `FLEET_CLEANUP_ENABLED` = `true`.
   5. Ask the runner to run the settings audit.

Expect the HeartbeatAlarm to email about hourly while scheduled expiration is off. Expiration stays off until a fleet is registered, dry runs pass, the [legacy-watchdog handoff](fleet-watchdog-coordination.md) is done and a disposable canary passes. Then [turn it on](fleet-control-cleanup.md#later-turn-on-scheduled-expiration) as a separate change set.
