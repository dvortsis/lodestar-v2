# Fleet cleanup stack

The cleanup stack deletes the fleet's Linode VMs when their time is up, or when you ask in an emergency. It runs on its own in AWS, so cleanup never depends on GitHub approvals, Terraform or the provisioning credential.

**Where things stand:**

| Stack | State |
|---|---|
| Fleet-control (`fleet-control.json`) | Deployed, at the owner's request |
| PIA delivery (`pia-delivery.json`) | Deployed, at the owner's request |
| Cleanup (`cleanup.json`) | **Not deployed yet.** Follow [Deploy the cleanup stack](#deploy-the-cleanup-stack) below. |

The cleanup stack couldn't deploy before because it reserved Lambda concurrency, and this account's Lambda concurrency limit (10) leaves nothing to reserve. It now uses a lease instead, which gives the same guarantee: at most one run of each cleanup function at a time ([decision](../decisions/2026-11-15-X-cleanup-fits-quota.md)). No agent or CI deploys anything; you do, with the steps at the end.

## Who can do what

| Operation | Who starts it | What it can do |
|---|---|---|
| Scheduled expiration | An EventBridge schedule, every 15 minutes, **off by default** | Delete the registered VMs once their authorization has expired, or earlier if the firewall audit finds a definite security problem. No GitHub dependency. |
| Emergency teardown | You, by running `fleet-emergency-down.yml` on `main` | `LodestarFleetCleanupInvoker` can invoke the emergency function only. It can't read credentials or state, or call anything else. |
| The functions themselves | Their own Lambda role, `Executor` | Read one authorization object, set the mismatch flag, take and release their own lease, read their own Linode token and write their own logs. Nothing else. |

`fleet.yml` no longer tears fleets down. `/fleet down` and the old tag-wide delete paths refuse before touching the cloud.

## What the cleanup functions check

- **What to delete comes only from the registration.** After an approved apply, the fleet controller writes `fleet/cleanup-authorization.json` with the exact VM and firewall IDs, labels, types, creation times and an expiry at most 44 hours after creation. The functions never search by tag. A missing, malformed, future or over-long authorization fails.
- **Before any write, every target is checked:** the AWS account and region, the Linode account, each VM's region, type, label, tags and creation time, and each firewall's complete device list. Any foreign device, change or unreadable answer stops the run.
- **The authorization is pinned.** Before each deletion the function re-reads the authorization with `If-Match` on its version, so replacing or revoking it stops further work. It also confirms each VM is gone (404) before counting it.
- **One run per function at a time.** Before anything else the function takes a lease, `locks/cleanup-expiration.json` or `locks/cleanup-emergency.json` in the control bucket.
  - It creates the lease only if none exists (`If-None-Match: *`).
  - It takes over an expired lease only by its exact version (`If-Match`). The lease lasts 600 s, longer than the 300 s function timeout.
  - It releases the lease at the end by overwriting its own version.
  - A run that finds a live lease does nothing, logs one `cleanup_busy` line, returns `status: busy` and triggers the busy alarm. A lease error means no cleanup that run, and the Errors alarm fires.
- **Before the first deletion it sets `locks/fleet-mismatch.json`.** It never clears that flag, and never touches Terraform state. After any cleanup you reconcile by hand before the next fleet.
- **Emergency requests are fresh and pinned.** The workflow first asks the function to prepare (read-only), then executes that exact authorization version. Requests over ten minutes old, re-runs and changed authorizations fail.

## What it never deletes

Firewalls, volumes, reserved IPs, buckets, secrets and Terraform state. A Cloud Firewall can protect other services, and checking its attachments isn't atomic with deleting it, so firewalls are left for you to remove after checking. That's why the cleanup token needs only read access to firewalls.

## Alerts

All four alarms email the SNS topic you name when you deploy:

| Alarm | Fires when | What to do |
|---|---|---|
| `…ExpirationErrors`, `…EmergencyErrors` | A run failed: a refused check, a lease error, an unreadable API, a timeout | Read the function's log (CloudWatch ▸ Log groups ▸ `/aws/lambda/<stack>-expiration` or `-emergency`). A failure is never proof the fleet is gone. If the reason is `cleanup lease malformed`, see [Clear a damaged lease](#clear-a-damaged-lease). |
| `…BusyAlarm` | A run found the other run of the same function still holding the lease | Usually one run overlapping a slow or crashed one. If it repeats, look for a stuck run in the logs. |
| `…HeartbeatAlarm` | No successful expiration run in an hour | Expected while scheduled expiration is off (the default). After you turn it on, it means expiration isn't running. |

### Clear a damaged lease

A lease object that can't be read as a valid lease stops that function on every run, on purpose: unknown lease state never authorizes cleanup. To clear it:
1. Check no run is in progress: in the function's log group, the newest log stream should have no lines from the last 10 minutes.
2. Open **S3 ▸** the control bucket **▸ locks/**, tick `cleanup-expiration.json` or `cleanup-emergency.json` (only the one the error names), and choose **Delete**. Type `permanently delete` to confirm.
3. The next run creates a fresh lease. Deleting it removes no fleet data; it only lets the next run start.

Logs keep 14 days and hold fixed summaries only: no tokens, response bodies or authorization contents.

## Cost

About **$1–2 a month plus usage**: one secret ($0.40), two custom metrics (about $0.30 each), four alarms (about $0.10 each), plus small Lambda, log, S3 and EventBridge charges. The log groups and the secret are kept if you delete the stack, and keep their small charges until you delete them yourself. See [Secrets Manager pricing](https://aws.amazon.com/secrets-manager/pricing/) and [CloudWatch pricing](https://aws.amazon.com/cloudwatch/pricing/).

## Deploy the cleanup stack

You do this in the AWS console. It takes about 20 minutes. You'll review exactly what will be created before anything is created.

### Before you start: gather five values

Keep these in your private notes, never in GitHub or chat.

1. **The two bucket names.** In the AWS console, set the region (top right) to **US East (N. Virginia) us-east-1**. Open **CloudFormation ▸ Stacks**, choose the fleet-control stack, then the **Outputs** tab. Copy **StateBucketName** and **ControlBucketName**.
2. **A notification topic name.** Open **Simple Notification Service ▸ Topics**.
   - If a topic you already use for Lodestar alerts has a **Confirmed** email subscription, copy its **Name** (not the ARN).
   - If not, choose **Create topic ▸ Standard**, name it `lodestar-alerts`, and create it. Then choose **Create subscription ▸ Protocol: Email**, enter your address, and click the link in the email AWS sends. Wait until the subscription shows **Confirmed**.
3. **Your Linode account UUID (`euuid`).** This pins cleanup to one Akamai account. Use the value your private fleet configuration records. To read it again, run `linode-cli account view --format euuid --text --no-headers` with a read-only token.
4. **An activation time** in UTC, written like `2026-11-15T18:00:00Z`. Use the current UTC time, rounded down to the minute. Cleanup refuses any VM created before it, so nothing older can ever be deleted by mistake.
5. **The template file.** On GitHub, open `infra/aws/fleet-control/cleanup.json` on `main`, after this change has merged, and choose **Download raw file**. If the runner gave you a SHA-256 for it, check it in PowerShell with `Get-FileHash .\cleanup.json` and stop if it differs.

### Create a change set and review it

1. Open **CloudFormation ▸ Stacks ▸ Create stack ▸ With new resources (standard)**.
2. Under **Prerequisite - Prepare template**, choose **Choose an existing template**. Under **Specify template**, choose **Upload a template file ▸ Choose file**, pick `cleanup.json`, then **Next**.
   - The console saves the file in an S3 bucket it creates, named `cf-templates-…`. That's normal; the template holds no secrets.
3. **Stack name:** `lodestar-fleet-cleanup`. Fill in the parameters:

   | Parameter | Value |
   |---|---|
   | ControlBucketName | the ControlBucketName output |
   | EnableExpiration | `false` (leave it; you turn it on later) |
   | LinodeAccountEuuid | your Linode `euuid` |
   | MinCreatedUtc | your activation time |
   | NotificationTopicName | the topic **name** |
   | StateBucketName | the StateBucketName output |

   Choose **Next**.
4. On **Configure stack options**, leave the defaults. Under **Stack failure options**, keep **Roll back all stack resources**. At the bottom, tick **I acknowledge that AWS CloudFormation might create IAM resources with custom names**. Choose **Next**.
5. On **Review and create**, choose **Create change set** (not **Submit**). Name it `cleanup-owner-review`, then **Create change set**.
6. Wait until the change set's status is **CREATE_COMPLETE**, then open its **Changes** tab. You should see **exactly 16 rows, every one with Action Add**:

   | Logical ID | Type |
   |---|---|
   | CleanupToken | AWS::SecretsManager::Secret |
   | Executor, LodestarFleetCleanupInvoker | AWS::IAM::Role |
   | ExpirationLog, EmergencyLog | AWS::Logs::LogGroup |
   | Expiration, Emergency | AWS::Lambda::Function |
   | ExpirationErrors, EmergencyErrors, HeartbeatAlarm, BusyAlarm | AWS::CloudWatch::Alarm |
   | HeartbeatMetric, ExpirationBusy, EmergencyBusy | AWS::Logs::MetricFilter |
   | ExpirationSchedule | AWS::Events::Rule |
   | SchedulePermission | AWS::Lambda::Permission |

7. Open the **Template** tab and search for `ScopedCleanup`. The Executor role should have exactly five permissions:
   - read `fleet/cleanup-authorization.json` in the state bucket;
   - write `locks/fleet-mismatch.json` in the control bucket;
   - read and write `locks/cleanup-expiration.json` and `locks/cleanup-emergency.json` in the control bucket, and nothing else there;
   - read the `CleanupToken` secret;
   - write its own two log groups.

   Search for `InvokeEmergencyOnly`: the invoker role may only invoke the `Emergency` function. Search for `ReservedConcurrentExecutions`: there should be **no match**.
8. **If anything differs** (another row, a Modify or Remove, a wider permission, a name collision), choose **Delete change set**, stop, and tell the runner. Don't execute it.

### Create the stack

1. On the change set page, choose **Execute change set**. Keep **Roll back all stack resources**, then **Execute change set**.
2. Open the stack's **Events** tab and wait for **CREATE_COMPLETE** (a few minutes). If it shows **ROLLBACK**, copy the first failed event's reason and send it to the runner; don't retry.
3. The **HeartbeatAlarm** emails you within about an hour, and keeps doing so while scheduled expiration is off. That's expected; it stops once you turn expiration on.
4. Open the **Outputs** tab. Copy **CleanupRoleArn** and **EmergencyFunctionArn** into your private notes. You need them for GitHub below.
5. Check the concurrency: open **Lambda ▸ Functions ▸ lodestar-fleet-cleanup-emergency ▸ Configuration ▸ Concurrency**. It should say **Use unreserved account concurrency**. Check `lodestar-fleet-cleanup-expiration` the same way.

### Install the cleanup token (the third token)

The fleet uses three Linode tokens: the fleet token, the PIA firewall token and this cleanup token. The cleanup token stays in AWS and never reaches GitHub.

1. In **Akamai Cloud Manager**, open your profile menu ▸ **API Tokens ▸ Create a Personal Access Token**.
   - **Label:** `lodestar-cleanup`.
   - **Expiry:** the shortest you're willing to renew, for example 6 months. Put a reminder in your calendar.
   - **Access:** **Account: Read Only**, **Firewalls: Read Only**, **Linodes: Read/Write**. Set everything else to **No Access**.
   - Choose **Create Token** and copy the token. Cloud Manager shows it only once.
2. In AWS, open **Secrets Manager ▸ Secrets ▸ lodestar/akamai-cleanup-token**. In **Secret value**, choose **Set secret value**. If the console shows **Retrieve secret value** instead, choose it, then **Edit**.
3. Choose **Plaintext**, delete any placeholder, paste only the token (no quotes or spaces), and choose **Save**.
4. **Test it, and test your alerts.** Open **Lambda ▸ lodestar-fleet-cleanup-emergency ▸ Test**. Create an event named `dryrun` with the body `{"dry_run": true}` and choose **Test**.
   - With no fleet registered yet, the expected result is a failure. The log line should show `"stage": "authorization"`: the lease worked, and there's no authorization to read. Within about 5 minutes the **EmergencyErrors** alarm should email you. That email shows alerts work.
   - **Choose Test again** within a minute. The second run must also stop at `"stage": "authorization"`. That proves the first run released its lease. If it says `busy`, or either log has a `cleanup_lease_release_failed` line, stop and tell the runner.
   - Run the same two tests on **lodestar-fleet-cleanup-expiration**, so its lease is checked too.
   - If the stage is `lease`, the lease permissions are wrong: tell the runner.
   - When a fleet exists, the same test returns `"status": "dry_run"` and deletes nothing.

### Create the `fleet-cleanup` environment in GitHub

This environment lets only you, on `main`, run the emergency teardown. It deliberately has no reviewer or timer: your dispatch is the approval.

1. On GitHub, open the repository ▸ **Settings ▸ Environments ▸ New environment**. Name it `fleet-cleanup`, then **Configure environment**.
2. **Deployment protection rules:** leave **Required reviewers** and **Wait timer** unticked. Untick **Allow administrators to bypass configured protection rules**.
3. **Deployment branches and tags:** choose **Selected branches and tags ▸ Add deployment branch or tag rule**. Set **Ref type: Branch** and **Name pattern:** `main`, then **Add rule**. Add no other rules.
4. **Environment secrets ▸ Add environment secret**, twice:
   - `AWS_CLEANUP_ROLE_ARN`: the **CleanupRoleArn** output;
   - `AWS_CLEANUP_FUNCTION_ARN`: the **EmergencyFunctionArn** output.
5. **Environment variables ▸ Add environment variable:** `FLEET_CLEANUP_ENABLED` with the value `true`. Until it's `true`, the emergency workflow does nothing.
6. Ask the runner to run the settings audit (`python infra/fleet/deployment_policy.py audit`). It reads your settings and should report no `fleet-cleanup` problems.

### Later: turn on scheduled expiration

Scheduled expiration deletes VMs without you. Turn it on only when **all** of these are done:
- a fleet has been registered, and a dry run of each function returned `dry_run`;
- the alert email arrived;
- the old watchdog is handed over, following [the legacy-watchdog handoff](fleet-watchdog-coordination.md). The existing watchdog can act on the same VMs without reading this stack's authorization, so the two must not both be live;
- a disposable canary passed: a throwaway fleet, registered and then deleted by the emergency workflow, with every VM confirmed gone.

1. Open **CloudFormation ▸ Stacks ▸ lodestar-fleet-cleanup ▸ Stack actions ▸ Create change set for current stack**. Choose **Use existing template**, then **Next**.
2. Set **EnableExpiration** to `true` and leave every other parameter as it is. Choose **Next**, tick the IAM acknowledgement, then **Next** and **Create change set**.
3. The change set should show **one row: Modify ExpirationSchedule**. If it shows anything else, delete it and tell the runner. Otherwise choose **Execute change set**.
4. Within an hour the **HeartbeatAlarm** should return to **OK**. That also proves its log filter counts successful runs. If it stays in **ALARM** after two hours, tell the runner.
