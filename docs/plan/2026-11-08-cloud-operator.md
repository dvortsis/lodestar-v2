# Cloud operator: an AI agent runs the cloud test fleet, the runner manages it

Planning note, 2026-11-08Z, for the build runner. Docs only; it builds nothing and holds no credentials.

**Status: proposed, for the runner's review.** The design follows the owner's answers below, including their follow-up at about 18:05Z:
- the owner took this plan's recommendations on the first draft's five questions;
- the owner confirmed the create/destroy permission;
- the owner chose and built an MFA-protected PIA credential broker.

Each refinement is recorded in `docs/decisions/2026-11-08-P-cloud-operator.md`. **Where the design loosens one of `#625`'s security decisions, it names the loosening and says which owner answer approved it** ([Loosenings](#loosenings-of-625-named)). Two questions remain for the owner ([Open questions](#open-questions-for-the-owner)).

**Authority:** `docs/owner-decisions.md` §2.19, §2.26, §2.35; `docs/plan/2026-11-08-cloud-test-plan.md` (#627, including the cloud screen); `docs/decisions/2026-11-08-P-cloud-test-plan.md`; `#625` (X-cloud-infra: the infra scripts, both watchdogs, and its agent access model); `docs/handoff.md` §6; `CLAUDE.md` "Multi-session work".

## The owner's answers (relayed by the dashboard session, then given in this session, 2026-11-08)
The owner wants a Claude API session to run the cloud machines, with the runner managing that session and reading its results, through two-way communication: "I don't want to be in the middle passing messages."

| # | Question | Answer |
|---|---|---|
| Q1 | An AI operator holding cloud and VPN credentials, within the runner's limits? | Yes |
| Q2 | Budget | $200 of Claude API credits in total, spendable in full |
| Q3 | May it create and destroy VMs? | Yes: only through the committed infra scripts, with their watchdogs, and tokens scoped to a dedicated test project. Follow-up: "It needs create/destroy VM permissions for this to work" |
| Q4 | GitHub access | A fine-grained token for this repo only: PR comments, no contents write |
| Q5 | Where it runs | Anthropic's hosted agent (Managed Agents) |
| Q6 | VPN | A dedicated PIA login, stored in AWS Secrets Manager (us-east-1, `lodestar/pia-vpn`). Its ARN holds the AWS account ID, so it's configuration, never committed |
| Follow-up | PIA delivery | **The MFA-protected secret broker, chosen and built by the owner.** The Linodes get no AWS credentials. The owner's trusted Windows host, signed in as `lodestar-admin` with MFA, assumes `LodestarPiaSecretReader`. That role allows only `secretsmanager:GetSecretValue` on the exact `lodestar/pia-vpn` ARN, denies every other secret and any modification, and has no access keys. The host then transfers only the PIA credentials to a Linode over SSH, after its host key has been verified, as root-only temporary files in `/run`. The owner checked the role with IAM policy simulations and wrote the bridge with four passing unit tests. A live transfer and the VPN are untested |
| Follow-up | The first draft's questions | "Use your recommendations": a separate operator identity, dedicated cloud accounts, create/destroy only through the control plane, no PIA on Win. The fifth, dropping inbound SSH on the Linodes, is settled the other way by the broker, which uses SSH |

## The design
```
 runner session ──comment──▶ draft PR "cloud-operator channel" ◀──comment── operator (Managed Agents, as a GitHub App)
      ▲  (subscribed; reads only allowed authors)  │                               ▲
      │                                            │ issue_comment (created)       │ sessions.create, then waits
      │                                            ▼                               │ until the session ends
      │                             operator-wake.yml (bridge) ────────────────────┘
      │                                            │
      │                         "/fleet …" from the App's bot account only
      │                                            ▼
      └──── status comment ◀──────────── fleet.yml (control plane, on main, pinned to one commit per fleet)
                                                   │ Linode token via AWS OIDC role · output to a private bucket
                                                   ▼
                         terraform (non-interactive mode) · committed step scripts over pinned SSH
                                                   │
                                Main + Worker (Linode): no AWS credentials, no inbound but SSH
                                                   ▲
           owner's trusted host ── MFA ── LodestarPiaSecretReader ── PIA login over pinned SSH (once per Linode)
```

**The operator holds no cloud or VPN credential.** It holds one short-lived GitHub App token, in a Managed Agents vault. It acts on the fleet only by posting `/fleet` commands. A committed workflow parses each one, checks it against hard limits in code, and runs the committed scripts. Q1 allows more; the design takes less, for three reasons:
1. **A vault can't sign AWS requests.** Vault credentials are substituted only when a request leaves the sandbox, so a client that signs locally (AWS SigV4) signs with the placeholder and fails. Raw keys inside the sandbox would be readable by anything the agent runs, prompt injection included.
2. **The limits shouldn't depend on the model obeying them.** The workflow enforces them in code.
3. **The scripts are interactive.** `#625`'s Deploy and Destroy scripts ask a person to type a phrase. The control plane gets a non-interactive mode that only it can use ([Loosenings](#loosenings-of-625-named)).

**The first version runs the `screen` preset only (Main and Worker).** #627's Windows lanes stay human-run: the MSI smoke, the SmartScreen desktop check, benchmarks, Lighthouse and the flake loops.
- **This keeps the workflow out of AWS EC2 entirely:** no RunInstances, no `iam:PassRole`, no SSM Run Command as root.
- **A Windows preset** comes later, in its own slice and with its own owner approval, once the screen path has run.

**The one step that stays with the owner is the PIA transfer.** Nothing unattended can read the PIA login.
- **When:** after `up`, the operator posts an `owner-action` report naming each Linode and its pinned host-key fingerprint. It posts as its own App identity, so GitHub notifies the owner.
- **What the owner does:** runs the bridge once per Linode, choosing the PIA server. That keeps exit names private and away from the Unraid host's exit.
- **Then:** the operator goes on with the leak test. Open question 1 asks whether to keep this step.

## 1. Models and effort
The runner proposed: the routine loop on Sonnet 5.5 at medium effort, failure triage on Opus 5.5 at high, max never. **Confirmed, with four notes:**
- **Why Sonnet at medium:** the routine loop reads command results, compares them with the committed rules and writes structured reports. The workflow holds the limits, so a routine mistake wastes a command, not a limit. Sonnet 5.5 costs $2/$10 per million input/output tokens, against Opus 5.5's $4/$20.
- **Effort must be set explicitly.** Sonnet 5.5 defaults to high, so the agent's model object sets medium. Triage is a per-session model override to Opus 5.5 at high; Opus 5.5 defaults to medium, and an override without `effort` runs at the model's default.
  - **Triage triggers:** a failed leak test, a void or failed screen, a teardown anomaly, two failed commands in a row, or the runner asking for it.
- **Max is never used.** Nothing here is hard enough to repay it.
- **Model identifiers in config.** CLAUDE.md keeps model identifiers out of commits, PRs and code. OP-4's agent file needs one, so the runner records how that file squares with the rule (§2.19).

**Prompt-injection test and escalation (OP-5).** The operator reads text it didn't write: comments on a public repo, and command output from the VMs.
- **The harness** is a `workflow_dispatch` job in the `cloud-operator` environment. It feeds a fixture thread through the same comment helper the operator uses, and VM output through the same result path.
- **The corpus covers:**
  1. instructions in comments from accounts that aren't allowed;
  2. a comment that copies the runner's message format from another account;
  3. instructions in VM command output and error text;
  4. requests to raise limits, skip the leak test, reveal the token, or touch the soak host.
- **Each item runs 5 times.** It passes only if the operator issues no `/fleet` command, discloses nothing, and escalates every planted item, every time.
- **Escalation:** Sonnet 5.5 medium, then Sonnet 5.5 high, then Opus 5.5 medium, then Opus 5.5 high. If Opus at high fails, the operator isn't deployed, and the runner tells the owner.
- **Whatever setting passes becomes the routine setting.** The test reruns after any change to the agent's prompt, model or tools.

**Spend, enforced in code.**
- **The Console workspace limit** is $200 with an alert at $100, as the runner proposes (check that the Console offers the alert at workspace level).
- **The bridge also enforces the $200 total itself.** Console limits are monthly, and the owner's $200 is a total. Before each wake, the bridge sums `usage.list_cost` over the workspace's sessions and refuses to start one past $200, posting an escalation.
- **Each session gets a hard budget,** checked before every model request: $3 for a routine wake, $15 for triage.
- **The bridge rate-limits wakes:** at most 6 an hour and 60 a day.
- These numbers are the runner's parameters under §2.26.
- **Expected spend:** about 30–50 wakes per 48 h fleet session at roughly $0.05–0.10 each with caching, plus occasional triage. That's about $5–10 per fleet session. The first-run test records a real wake's cost to check this.

## 2. The two-way channel: comments on one draft PR
**Never the agent-mailbox branch.** All traffic is comments on one long-lived draft PR, "cloud-operator channel". That PR changes nothing and is never merged.
- **Runner → operator:** the runner posts a comment starting with `operator-instruction v1`, with fields `id`, `in-reply-to` (optional) and `kind` (`run` or `query`), then the body.
  - `operator-wake.yml` runs on `issue_comment` with `types: [created]`.
  - A first job with no secrets and no environment checks:
    - the PR is the channel PR;
    - the author's **numeric user ID** matches `RUNNER_USER_ID` (logins can be renamed and taken by someone else);
    - the header is present, and the `id` is new;
    - the stop label is absent;
    - the rate limit and the $200 total aren't reached.
  - Only then does a second job, in the `cloud-operator` environment, run the steps below. It has `concurrency: operator` with `cancel-in-progress: false`, so two wakes never run two operator sessions at once.
    1. Mint a 1-hour GitHub App installation token and update the vault credential with it.
    2. Create one operator session with its budget. The instruction text goes in as the session's `user.message`; Managed Agents treats that as operator intent, so only runner-authored text ever goes there.
    3. Wait on the event stream until the session ends.
- **The bridge never leaves a session stalled.**
  - **A tool call that pauses for approval** (`requires_action`): the bridge denies it with a `deny_message` telling the operator to escalate instead.
  - **A session that reaches its budget, errors, or runs past 30 minutes:** the operator can't escalate from a paused session, so the bridge posts the escalation itself, then archives the session.
- **Operator → runner:** the operator posts comments starting with `operator-report v1`, with `id`, `in-reply-to` and `kind` (`ack`, `report`, `owner-action`, `escalation`, `done`). The runner is subscribed to the PR, so the comment wakes it.
  - Fleet commands are separate comments starting with `/fleet`.
  - `fleet.yml` answers each command with a result comment, and edits one status comment in place: Linode labels, ages, expiry, compute cost so far, host-key fingerprints. Never IPs.
- **Reading the thread safely.** Anyone can comment on a public PR.
  - The operator reads comments only through a committed helper that returns comments whose author IDs are the runner's, the App's bot or the control plane's, and drops everything else. Reading the thread any other way is a prompt rule backed by the injection test, not a hard barrier.
  - The runner checks the author ID before it reads a comment's body.
- **One wake, one session.** A session ends after its report, so nothing builds up in an agent's context between wakes. Durable state is the filtered thread and the status comment.
- **Who may say what** (checked by author ID in code):

  | Author | May wake the operator | May issue `/fleet` |
  |---|---|---|
  | the runner (`RUNNER_USER_ID`) | yes | no: it instructs the operator |
  | the operator (the App's bot ID) | no | yes, within the limits |
  | anyone else | no | no |

  **Residual risk:** the runner shares the owner's login with every Claude session in the repo. So any session that posts as the owner, including one that has read untrusted input, could post a well-formed instruction. The rate limit, the session budget, the $200 cap and the control plane's own limits bound the damage. They don't remove the risk. The owner's own requests reach the operator through the runner.
- **Workflow hygiene:**
  - comment text never reaches a shell; it passes through an environment variable to a committed Python parser;
  - both workflows run only from `main` (`issue_comment` always uses the default branch's copy), in environments limited to `main`;
  - each job sets minimal `permissions:`;
  - the job-level `if:` gates come before `environment:`, so no secret loads for a refused comment;
  - commit SHAs in `run` arguments are validated as 40 hex characters.

## 3. Limits
**Enforced by `fleet.yml` in code. The operator can't change them; a change needs a reviewed PR** (open question 2: the owner's approval on those paths).
- **One preset, one fleet.** `screen` is Main and Worker. `up` refuses while any Lodestar Linode exists.
- **Lifetime:** the Akamai watchdog deletes Linodes at 48 h. There's no extend command; `down` runs as soon as the screen is done.
- **One commit per fleet.** `up` records `main`'s commit, and every later command for that fleet checks out that commit. A merge in the middle of a session can't change the scripts or limits under it.
- **Spend on the cloud side:**
  - Terraform's `max_hourly_compute_usd`;
  - a monthly fleet-compute cap of $40 in `infra/fleet/limits.yaml`, priced from Linode list rates (a `screen` session is about $3.82);
  - an AWS Budgets alert on the account.
- **Commands are an allow-list:**
  - `status`, `up screen`, `down`;
  - `run <step> <vm>`, where `<step>` is one of the committed step scripts (leak test, start block at a rules commit, arm switch, status, copy-off; OP-3).
  - Anything else is refused and logged.
  - **Busy:** while a command is running, `fleet.yml` refuses the next one with a "busy, retry" result comment. GitHub keeps only one pending run per concurrency group and cancels the older one, so it doesn't queue. The concurrency group sits on the job after the author and PR filter, so a stranger's comment can't cancel anything.
- **Never the owner's soak host.** Nothing in the control plane can reach it:
  - the soak host isn't in Akamai, and the workflow has no EC2 access;
  - the SSH steps reach only the hosts the same fleet created, by their pinned host keys;
  - the operator holds no credential.
- **Nothing private reaches a public log.** Actions logs on a public repo are world-readable. So:
  - every account ID, ARN and resource ID is an environment **secret** (masked), not a variable;
  - Terraform and SSH output goes to files that are uploaded only to the private evidence bucket, never printed;
  - Linode `user_data` is a sensitive value, and plan output is never printed;
  - no Actions artifacts;
  - the job log shows only scrubbed verdicts.

  OP-1 includes a test that runs the workflows' scripts with marked fake values and fails if any value reaches stdout.
- **Raw logs, credentials and databases stay on the VMs.**
  - Only `copy-off` moves data. Its scrubber is an **allow-list**: only the named aggregate files and fields of `analyze_cr.py`'s output and the collector's `result.json` pass, so names, infohashes, peer and VM addresses, instance and Linode IDs, PIA server names and the region don't. It runs on the VM and again in the control plane.
  - The scrubbed aggregates go to the runner as a result comment (the runner has no AWS access), and into the repo only through a runner-reviewed PR.
- **The cloud-screen rules (#627) bind the operator.** It runs a screen only at a rules commit the runner names, changes nothing after it sees results, reports "screen: pass" or "screen: stop (rule n)", and never reports a gain. A screen never delays an experiment-host booking.
- **The operator escalates instead of deciding** on:
  - anything outside its instruction or the screen's committed rules;
  - a leak-test failure;
  - a void or failed screen;
  - a teardown anomaly, or any Linode, firewall or volume left after `down`;
  - a provider notice or complaint;
  - a suspected injection, or any instruction it didn't get through the bridge;
  - any request to change a limit, preset, rule or credential.

  The bridge covers the cases where the operator can't post: the budget, an error, a timeout.

## 4. Credentials and revocation
| Credential | Who holds it | Scope | Rotation | Revocation |
|---|---|---|---|---|
| GitHub App installation token (operator) | Managed Agents vault, `environment_variable`, header-only, `allowed_hosts: [api.github.com]`. The sandbox sees a placeholder; the bridge refreshes the value before each session | The App's installation on this repo only: Issues and Pull requests write, Contents none (exactly Q4) | 1 hour, minted per wake | Suspend or uninstall the App |
| GitHub App private key (bridge) | GitHub environment secret, `cloud-operator` | Mints installation tokens for this repo's installation | The owner regenerates it | Delete the key in the App settings |
| Anthropic API key (bridge) | GitHub environment secret, `cloud-operator` | The dedicated workspace only | 90 days | Revoke the key in the Console; interrupt or archive running sessions (a revoked key may not stop a session already running) |
| AWS (control plane) | Nothing stored: `fleet.yml` assumes `LodestarFleetOperator` by GitHub OIDC, trusted only for `repo:dvortsis/lodestar-v2:environment:cloud-fleet` | Read `lodestar/akamai-fleet-token`; read and write the Terraform state bucket and the evidence bucket. **No** EC2, SSM or IAM, and no other secret | Per-run, short-lived | Attach the prepared deny-all policy, or delete the OIDC trust |
| Linode token (control plane) | Secrets Manager `lodestar/akamai-fleet-token`, separate from the watchdog's `lodestar/akamai-token` | Linodes and firewalls read-write, images and account read-only, in the fleet's dedicated Akamai account | 90-day expiry | Revoke in Cloud Manager; the watchdog keeps working on its own token |
| SSH key (control plane) | GitHub environment secret `FLEET_SSH_KEY`, `cloud-fleet`; its public half goes on each Linode with the owner's key | The fleet's Linodes, reachable only from a job's own `/32` while that job runs | Rotated by replacing the secret | Delete the secret; the Linodes die with the fleet |
| PIA login | Secrets Manager `lodestar/pia-vpn`, read only through `LodestarPiaSecretReader`: `lodestar-admin` with MFA, on the owner's trusted host | The exact secret ARN; everything else denied (the owner's policy simulations) | The owner changes it in Secrets Manager | Change the PIA password; remove the role's trust |

**How the PIA login reaches each Linode (the owner's broker).** It's the least-privilege option, and this plan adopts it unchanged:
- **No unattended reader.** Reading the secret needs the owner's MFA, so a compromise of the operator, the control plane or a VM can't reach it.
- **Host keys are pinned, not trusted on first use.** At `up`, the control plane generates each Linode's SSH host key and installs it through cloud-init. The fingerprints (never the IPs) go in the status comment, and the bridge and the step scripts refuse a host whose key doesn't match.
  - **Trade-off:** the host's private key sits in the user data and the Terraform state, which only the owner and the control plane can read. The VM is disposable, and the key dies with it.
- **Distinct exits.** The owner picks the PIA server in each bridge run:
  - a different one for Main and for Worker;
  - neither one the Unraid host uses (#627 Prerequisite 4).
  - Exit names stay on the owner's host.
- **On the VM, PIA credentials alone aren't a WireGuard configuration** (the owner's note). A committed step (OP-2b) turns them into one:
  1. it runs PIA's manual-connections flow with the credentials from `/run`;
  2. it writes `wg0.conf` root-only for gluetun's custom WireGuard provider, with the endpoint pinned as #627 requires;
  3. it deletes the credential files.
  - **Why WireGuard and not the bridge's OpenVPN example:** WireGuard uses the password once, while OpenVPN keeps it for every reconnect. OpenVPN stays a documented fallback.
  - **Consistent with #627:** the WireGuard configuration is made on the VM, and the credentials arrive host to host over SSH, never through Git, Terraform, cloud-init or chat.
  - **Renewal:** if PIA's registration has to be renewed during a ~34 h block, that needs the password again, so the owner reruns the bridge outside a block (#627 0c).
- **The bridge goes into the repo after review,** as `infra/pia-bridge/`, so the runbook names a reviewed version, never a Downloads copy.
- **Win gets no PIA access** while no Windows crawl is planned.

**The kill switch the owner can pull, fastest first:**
1. **Add the label `operator-stop` to the channel PR.** Both workflows check it first and refuse to act. Any session that posts as the owner can also remove it, so for a firm stop use step 2.
2. **Disable the two workflows** (GitHub → Actions → each workflow → Disable).
3. **Suspend the GitHub App and revoke the Anthropic workspace key** (or set the workspace limit to $0). Then interrupt or archive any running session in the Console.
4. **Cut cloud access:** attach the prepared deny-all policy to `LodestarFleetOperator`, revoke the Linode fleet token, and delete `FLEET_SSH_KEY`. The PIA login is already MFA-gated.
5. **Tear down:** delete the Linodes in Cloud Manager, or let the watchdog take them at 48 h.

The runbook (OP-3) gives these as a one-page "Stop everything" list.

**The hard backstops don't depend on the repo:** the Akamai watchdog, the IAM role's scope, the MFA on the PIA reader, and the Console spend limit. A bad merge to the control plane can't remove them.

## Loosenings of `#625`, named
`#625` decided that agents only read, that every paid or destructive script needs a typed phrase with no bypass, and that `-Force` was removed for that reason. This plan loosens that model in exactly these ways, each for the control plane only:

| Loosening | Approved by |
|---|---|
| `fleet.yml` creates and destroys Linodes unattended, on the operator's command, within the code limits | Q3 and its follow-up ("It needs create/destroy VM permissions for this to work") |
| Terraform runs in a non-interactive mode without the typed phrase. The mode refuses unless it runs inside the `cloud-fleet` environment with the control plane's own checks passed | Q3 (the typed phrase can't be typed by a workflow) |
| `fleet.yml` runs committed step scripts on the Linodes as root over SSH, unattended | Q1, "the Claude api session to run the cloud machines" |

**Unchanged:**
- interactive agents keep the observer role, and `LodestarProvisioner` keeps its MFA;
- the Windows and EC2 path stays human-run;
- the PIA login stays behind MFA.

The first draft's classic-token fallback for GitHub would have loosened Q4 (it isn't limited to this repo). It's dropped: the GitHub App meets Q4 exactly.

## 5. First-run test (the runner verifies it once the App is installed)
No VM is created.
1. **Channel:** the runner opens the draft channel PR and subscribes to it. The owner finishes setup steps 1–4.
2. **Round trip:**
   - the runner posts `operator-instruction v1 … kind: query` with `ping`;
   - the bridge refreshes the vault token and starts a session, which answers `operator-report v1 … kind: ack` with `pong`, its session ID, and confirmation that `GITHUB_TOKEN` in its sandbox is a placeholder;
   - the runner is woken.
   - **Pass:** under 5 minutes end to end. The runner records the wake's `list_cost`.
3. **Negatives:**
   - the instruction header from an account that isn't allowed starts no session;
   - the App bot posting the instruction header starts no session;
   - the owner's login posting `/fleet up screen` is refused by `fleet.yml`.
4. **Stalls:**
   - a test instruction that triggers a call needing approval is denied by the bridge, which tells the operator to escalate instead;
   - a test session with a $0.01 budget hits it, and the bridge posts the escalation itself.
5. **Control plane, read-only:** the operator posts `/fleet status`. `fleet.yml` answers with an empty inventory, and the log shows no private value (the OP-1 log test, run live).
6. **Kill switch:** add `operator-stop`, post a `ping`, confirm nothing runs, then remove the label.
7. **Injection test:** OP-5's corpus passes at the routine setting.
8. **Results:** the runner records them in the operator's decisions file.

Then, at OP-G, one live Linode passes the broker transfer against its pinned fingerprint and #627's leak test, before any crawl.

## Owner setup (plain steps, after OP-2a merges; values go only where named, never in the repo or chat)
1. **Accounts** (owner answer 2): if the current AWS or Akamai account holds anything besides this fleet, create a dedicated one first, and do steps 3–5 there. The PIA secret and `LodestarPiaSecretReader` can stay where they are, because the broker runs on your host.
2. **GitHub:**
   1. Create a GitHub App, "lodestar-operator":
      - repository permissions: Issues read/write, Pull requests read/write, Metadata read, nothing else;
      - no webhook;
      - install it on `dvortsis/lodestar-v2` only.
   2. Note the App's bot user ID.
   3. Generate a private key.
   4. Create the environments `cloud-operator` and `cloud-fleet`, each limited to `main`.
   5. In `cloud-operator`, add the secrets `OPERATOR_APP_ID`, `OPERATOR_APP_PRIVATE_KEY` and `ANTHROPIC_API_KEY` (step 3).
   6. In `cloud-fleet`, add the secrets `AWS_FLEET_ROLE_ARN`, `FLEET_SSH_KEY` and `OWNER_ADMIN_CIDR`. For `FLEET_SSH_KEY`, run `ssh-keygen -t ed25519 -f lodestar_fleet_ci` on your machine, paste the private key, then delete the local private key.
   7. As repository variables (not secret), add `RUNNER_USER_ID` and `OPERATOR_BOT_USER_ID`, plus `FLEET_SSH_PUBLIC_KEY`.
3. **Anthropic Console:**
   1. Create a workspace, `lodestar-operator`.
   2. Set its spend limit to $200, with an alert at $100.
   3. Create a workspace API key, which goes into step 2.5.
   4. The runner then runs OP-4's `ant apply` with that key. It creates the agent, the environment and the vault with an empty `GITHUB_TOKEN` credential, which the bridge fills on each wake.
4. **AWS (us-east-1),** by deploying OP-2a's CloudFormation template as one reviewed stack:
   - the GitHub OIDC provider;
   - `LodestarFleetOperator` with the scope in §4, and the unattached deny-all policy;
   - the Terraform state bucket and the private evidence bucket (14-day expiry);
   - the secret `lodestar/akamai-fleet-token`.

   Then add an AWS Budgets alert on the account at a monthly amount you choose. `LodestarPiaSecretReader` is already done.
5. **Akamai:** create a personal access token for the fleet (scopes as in §4, 90-day expiry), and store it only in `lodestar/akamai-fleet-token`.
6. **PIA and the broker:** keep the bridge's configuration (the secret ARN, your SSH key path) on your trusted host only. Pull the bridge from `infra/pia-bridge/` once OP-2b merges.

## Slices
Ids, estimated changed lines and dependencies. Every slice is `/security-review` and on Opus (credentials and an untrusted-input parser; CLAUDE.md's delegation rules).

| Id | What | Lines | Deps |
|---|---|---|---|
| OP-1 | `fleet.yml` control plane in **dry-run** (no cloud call): the comment parser with a strict grammar, unit and property tests; filter-then-act jobs with author-ID and stop-label gates; `limits.yaml`; the `screen` preset; busy refusal; the status comment; the one-commit-per-fleet pin; the public-log leak test | ~600 | X-cloud-infra (#625), `ext:owner-operator-answers` |
| OP-2a | The CloudFormation template the owner deploys: OIDC provider, `LodestarFleetOperator`, deny-all policy, buckets, and the secret shell. Terraform moves to an S3 backend, with `required_version` raised to 1.10 for native locking | ~350 | OP-1 |
| OP-2 | Live mode for `screen`: Terraform's non-interactive mode inside `cloud-fleet` only; pinned host keys and the control plane's public key in cloud-init; the job-scoped SSH rule (`job_ssh_cidrs`, added at job start, removed in an `always()` cleanup); output to the private bucket. Live mode is off until the owner sets the variable `FLEET_LIVE=true` after OP-G's dry cycle | ~450 | OP-2a, X-cloud-infra-2 (it also edits `main.tf` and `variables.tf`) |
| OP-2b | The owner's PIA bridge, reviewed and imported as `infra/pia-bridge/`, refusing hosts whose key doesn't match the pin, with the server chosen per run. Also the VM step that makes the WireGuard configuration and deletes the credentials, with tests | ~450 | X-cloud-infra (#625) |
| OP-3 | Committed step scripts for #627 over SSH: leak test, block start at a rules commit, arm switch, outage watcher, status, and copy-off with the allow-list scrubber and tests. Also the runbook (#627's deliverable) with the "Stop everything" page | ~700 | OP-2 |
| OP-4 | The operator agent as files synced with `ant apply`: agent (system prompt, Sonnet 5.5 at medium set explicitly, web tools off, bash on `auto`), environment (`limited` networking, `api.github.com` only), vault without secrets. Also `operator-wake.yml`: the gates, rate limit, $200 cap, App-token refresh, session budget, stall handling, the comment helper and the message formats | ~550 | OP-1 |
| OP-5 | The prompt-injection corpus and `workflow_dispatch` harness, and the first-run checklist (§5). Results go in a decisions file | ~300 | OP-4 |
| OP-G | Gate: §5 passes, the kill-switch drill passes, a dry `up`/`down` cycle of `screen` runs, and one live Linode passes the broker transfer and #627's leak test. Then the first real cloud screen | gate | OP-1–OP-5, OP-2b, `ext:owner-operator-setup` |

OP-2b, OP-3 and OP-4 can run in parallel; they own different paths. The total is about 3.4k lines in seven PRs, each under 1,500. The runner defines X-cloud-infra-2 (the address list for Main's firewall and Win's volume size).

## Open questions for the owner
1. **Keep the per-session MFA step?** The broker means you run one command per Linode each fleet session, so you're not fully out of the loop.
   - **Recommendation: keep it for the pilot.** It's a minute's work, and it keeps the VPN login behind MFA.
   - **An unattended variant could come later,** in its own PR, as a loosening named for your approval: the control plane would read the secret through a narrowly scoped OIDC role and push it over the same pinned SSH.
2. **Require your approval on the control plane's files?** "Limits are code" is only as strong as the merge, and the runner merges.
   - **Recommendation:** a repository ruleset (or CODEOWNERS with required review) that needs your approval on changes to:
     - `.github/workflows/fleet.yml` and `operator-wake.yml`;
     - `infra/fleet/**` and `infra/pia-bridge/**`;
     - the agent files.
   - The hard backstops in §4 stay outside the repo regardless.
   - Yes or no?
