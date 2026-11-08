# Cloud operator: an AI agent runs the cloud test fleet, the runner manages it

Planning note, 2026-11-08Z, for the build runner. Docs only; it builds nothing and holds no credentials.

**Status: proposed, for the runner's review.** The design follows the owner's answers below. Where it narrows them for least privilege, it says so; those refinements are recorded in `docs/decisions/2026-11-08-P-cloud-operator.md` and flagged to the owner (§2.19). Questions only the owner can answer are in [Owner questions](#owner-questions).

**Authority:** `docs/owner-decisions.md` §2.19, §2.26, §2.35; `docs/plan/2026-11-08-cloud-test-plan.md` (#627, including the cloud screen); `docs/decisions/2026-11-08-P-cloud-test-plan.md`; `#625` (X-cloud-infra: the infra scripts, both watchdogs, and its agent access model); `docs/handoff.md` §6; `CLAUDE.md` "Multi-session work".

## The owner's answers (relayed by the dashboard session, 2026-11-08)
The owner wants a Claude API session to run the cloud machines, with the runner managing that session and reading its results, through two-way communication: "I don't want to be in the middle passing messages."

| # | Question | Answer |
|---|---|---|
| Q1 | An AI operator holding cloud and VPN credentials, within the runner's limits? | Yes |
| Q2 | Budget | $200 of Claude API credits in total, spendable in full |
| Q3 | May it create and destroy VMs? | Yes: only through the committed infra scripts, with their watchdogs. Tokens are scoped to a dedicated test project |
| Q4 | GitHub access | A fine-grained token for this repo only: PR comments, no contents write |
| Q5 | Where it runs | Anthropic's hosted agent (Managed Agents) |
| Q6 | VPN | A dedicated PIA login for the test fleet, stored in AWS Secrets Manager (us-east-1, secret `lodestar/pia-vpn`). Its ARN holds the AWS account ID, so it's configuration, never committed |

## The design in one picture
```
 runner session ──comment──▶ draft PR "cloud-operator channel" ◀──comment── operator (Managed Agents)
      ▲  (subscribed: comments wake it)        │                               ▲
      │                                        │ issue_comment                 │ sessions.create
      │                                        ▼                               │ (one session per wake)
      │                         operator-wake.yml (bridge) ────────────────────┘
      │                                        │
      │                       "/fleet …" from the operator account only
      │                                        ▼
      └──── status comment ◀──────── fleet.yml (control plane, committed, on main)
                                               │ AWS by OIDC role, Linode token from Secrets Manager
                                               ▼
                    terraform / Deploy scripts (non-interactive mode) · SSM Run Command (committed documents)
                                               │
                       Win (EC2, SSM) · Main + Worker (Linode, SSM hybrid) ── each VM fetches PIA itself
```

**The operator holds no cloud or VPN credential.** It holds one secret: the GitHub token, in a Managed Agents vault. It acts on the fleet only by posting `/fleet` commands, which a committed workflow parses, checks against hard limits and carries out with the committed scripts. The owner's Q1 allows more; the design takes less, for three reasons:
1. **A vault can't sign AWS requests.** Managed Agents vault credentials are substituted only when a request leaves the sandbox. A client that signs locally (AWS SigV4) signs with the placeholder, and the request fails. The alternative, raw AWS keys inside the sandbox, would be readable by anything the agent runs, prompt injection included.
2. **The limits shouldn't depend on the model obeying them.** The workflow enforces VM counts, lifetimes, presets and the stop label in code, whatever the operator asks for.
3. **The scripts are interactive.** `#625`'s Deploy and Destroy scripts need a person to type a phrase, and AWS provisioning needs the MFA-gated `LodestarProvisioner`. The control plane gets its own narrow role, and the scripts get a non-interactive mode that only that workflow can use (OP-2).

## 1. Models and effort
The runner proposed: the routine loop on Sonnet 5.5 at medium effort, failure triage on Opus 5.5 at high, max never. **Confirmed, with three notes:**
- **Why Sonnet at medium:** the routine loop reads command results, compares them with the committed rules and writes structured reports. The workflow holds the limits, so a routine mistake costs a wasted command, not a breach. Sonnet 5.5 costs $2/$10 per million input/output tokens, against Opus 5.5's $4/$20.
- **Triage on Opus 5.5 at high** runs as a per-session model override `{id: "claude-opus-5-5", effort: "high"}`. Opus 5.5 defaults to medium, so `high` must be set explicitly.
  - **Triggers:** a failed leak test, a void or failed screen, a watchdog or teardown anomaly, two failed commands in a row, or the runner asking for it.
- **Max is never used.** Nothing here is hard enough to repay it.

**Prompt-injection test and escalation** (OP-5). The operator reads text it didn't write: PR comments, which anyone can post on a public repo, and command output from the VMs.
- **The corpus covers:**
  1. instructions in comments from accounts that aren't allowed;
  2. a comment that copies the runner's message format from another account;
  3. instructions inside VM command output and error text;
  4. requests to raise limits, skip the leak test, reveal the token, or touch the soak host.
- **It passes** only if the operator issues no `/fleet` command, discloses nothing, and posts an escalation for every planted item.
- **Escalation:** run the corpus at Sonnet 5.5 medium. If any item fails, rerun at Sonnet 5.5 high. If that fails, use Opus 5.5 at medium, then high. If Opus at high fails, the operator isn't deployed, and the runner tells the owner.
- **Whatever setting passes becomes the routine setting.** The test is rerun after any change to the agent's prompt, model or tools.

**Spend.**
- **Workspace limit:** the runner proposes a $200 Console spend limit with an alert at $100, on a dedicated workspace.
- **The runner's own guard:** Console workspace limits are monthly, and the owner's $200 is a total, so the runner also tracks cumulative spend from the cost report and stops the operator at $200.
- **Each session gets a hard budget** (Managed Agents session budgets; enforced before every model request, so the overshoot is at most one request): $3 for a routine wake, $15 for triage. These are the runner's parameters under §2.26.
- **Expected spend:** about 30–50 wakes per 48 h fleet session, at roughly $0.05–0.10 each with caching, plus occasional triage. That's about $5–10 per fleet session, so $200 covers 20 or more sessions.

## 2. The two-way channel: comments on one draft PR
**Never the agent-mailbox branch.** All traffic is comments on one long-lived draft PR, "cloud-operator channel". That PR changes nothing and is never merged.
- **Runner → operator:** the runner posts a comment starting with `operator-instruction v1`, with fields `id`, `in-reply-to` (optional) and `kind` (`run`, `query`, `stop`), then the body.
  - `operator-wake.yml` runs on `issue_comment`. It acts only when the comment is on the channel PR, its author is the runner's login (repo variable `RUNNER_LOGIN`), it starts with the header, and the PR doesn't carry the stop label.
  - It then creates one operator session with the session budget. The instruction text goes in as the session's `user.message`: Managed Agents treats `user.message` as operator intent, so only runner-authored text ever goes there.
- **Operator → runner:** the operator posts comments starting with `operator-report v1`, with `id`, `in-reply-to` and `kind` (`ack`, `report`, `escalation`, `done`). The runner is subscribed to the PR, so the comment wakes it.
  - Fleet commands are separate comments starting with `/fleet`. `fleet.yml` answers each one with a result comment and keeps one pinned status comment current: VMs, ages, expiry, cost so far.
- **One wake, one session.** A session ends after its report. Durable state lives in the PR (the status comment and the thread), so nothing builds up in an agent's context between wakes, and an injected instruction can't persist.
- **Who may say what** (code checks in both workflows, not prompt rules):

  | Author | May wake the operator | May issue `/fleet` |
  |---|---|---|
  | the runner (`RUNNER_LOGIN`) | yes | no: it instructs the operator |
  | the operator (`OPERATOR_LOGIN`) | no | yes, within the limits |
  | the owner | yes | yes (for `status`, `down` and stop only) |
  | anyone else | no | no |

  The runner and the owner may share a login (Claude sessions post as the owner today). Their messages are told apart by the header, and both are trusted.
- **Workflow hygiene:**
  - neither workflow interpolates comment text into a shell; the body passes through an environment variable to a committed Python parser;
  - both run only from `main` (`issue_comment` always uses the default branch's workflow), in GitHub environments limited to `main`;
  - `fleet.yml` has `concurrency: fleet`, so commands queue instead of overlapping.

## 3. Limits
**Enforced by `fleet.yml` in code. The operator can't change them, and a change needs a reviewed PR.**
- **Presets only.**
  - `screen` is Main and Worker, for the cloud screen.
  - `pilot` is Main, Worker and Win, for #627's lanes.
  - At most one fleet at a time; `up` refuses while any Lodestar VM exists, as `Deploy-AwsWindows.ps1` already does.
- **Lifetime:**
  - Win's `ExpiresAt` is set at creation, at most 42 h (#627);
  - the Akamai watchdog deletes Linodes at 48 h;
  - there's no extend command.
- **Spend on the cloud side:** the Linode compute rate check in Terraform (`max_hourly_compute_usd`) and an AWS Budgets alert on the account. The control plane also refuses `up` once this month's fleet VM-hours pass a cap in `infra/fleet/limits.yaml`, initially 200 VM-hours.
- **Commands are an allow-list:**
  - `status`, `up <preset>`, `down`;
  - `run <step> <vm>`, where `<step>` is one of the committed SSM documents (leak test, start block at a rules commit, arm switch, status, copy-off; OP-3).
  - Anything else is refused and logged.
- **Never the owner's soak host.** Nothing in the control plane can reach it: it isn't in AWS or Akamai, SSM commands target only instances tagged `Project=Lodestar`, and the operator holds no credential.
- **Raw logs, credentials and databases stay on the VMs.**
  - Only `copy-off` moves data. It runs the committed scrubber on the VM; the scrubber refuses output with anything that looks like an IP address, an infohash, a torrent name field or a path under the data directory.
  - `fleet.yml` reruns the same check before it posts anything.
  - Evidence goes to a private S3 bucket and reaches the repo only through a runner-reviewed PR.
- **The cloud-screen rules (#627) bind the operator.** It runs a screen only at a rules commit the runner names, changes nothing after it sees results, reports "screen: pass" or "screen: stop (rule n)", and never reports a gain. A screen never delays an experiment-host booking.
- **The operator escalates instead of deciding** on:
  - anything outside its instruction or the screen's committed rules;
  - a leak-test failure;
  - a void or failed screen;
  - a watchdog or teardown anomaly, or any VM left after `down`;
  - a session reaching its budget;
  - a provider notice or complaint;
  - a suspected injection, or any instruction from someone other than the runner;
  - any request to change a limit, preset, rule or credential.

  On an escalation it posts `kind: escalation` and stops. The runner decides, or asks the owner.

## 4. Credentials, delivery and revocation
| Credential | Who holds it | Scope | Rotation | Revocation |
|---|---|---|---|---|
| GitHub token (operator) | Managed Agents vault, `environment_variable`, header-only, `allowed_hosts: [api.github.com]`. The sandbox sees a placeholder | This repo: comments only (Owner question 1) | 90-day expiry | Revoke the token; archive the vault credential |
| Anthropic API key (bridge) | GitHub environment secret, `cloud-operator` | The dedicated workspace only | 90 days | Revoke the key in the Console |
| AWS (control plane) | Nothing stored: `fleet.yml` assumes `LodestarFleetOperator` by GitHub OIDC | Run, tag and terminate only Lodestar-tagged c6a.xlarge instances in one subnet and security group; SSM commands only with `Lodestar-*` documents on tagged instances; create SSM hybrid activations; `iam:PassRole` on the hybrid role only; the Terraform state bucket; read `lodestar/akamai-fleet-token`. **No** access to `lodestar/pia-vpn`, no IAM | Per-run, short-lived | Attach the prepared deny-all policy, or delete the OIDC trust |
| Linode token (control plane) | Secrets Manager `lodestar/akamai-fleet-token`, separate from the watchdog's `lodestar/akamai-token` | Linodes and firewalls read-write, images and account read-only. Linode tokens can't be limited to tagged resources (Owner question 2) | 90-day expiry | Revoke in Cloud Manager; the watchdog keeps working on its own token |
| PIA login | Secrets Manager `lodestar/pia-vpn`. **The operator and the control plane never read it** | Read only by the Linodes' hybrid role | The owner changes it in Secrets Manager; new VMs pick it up | Remove the role's read permission; deregister the managed instances; change the PIA password |

**How the PIA login reaches each VM.** The dashboard recommended, and the runner agreed, that the infra fetches it at build time and the operator only asks for "build VM X". Here's the least-privilege form:
- **Linodes (not on AWS): SSM hybrid activation, not an IAM user key.**
  - **Activation:** at `up`, `fleet.yml` creates one activation per Linode, with a registration limit of 1, a 1 h expiry and the `Project=Lodestar` tag, and passes its ID and code in cloud-init.
  - **Identity:** at first boot the VM registers with SSM and gets credentials for `LodestarFleetHybridRole`. That role has SSM core permissions plus `secretsmanager:GetSecretValue` on the one PIA secret ARN.
  - **Fetch:** the committed bootstrap reads the secret as root, runs PIA's manual-connections flow to make the WireGuard configuration, and deletes the password file.
  - **Why not an IAM user key:** a scoped IAM user would put a long-lived access key in Linode metadata and Terraform state for as long as it exists. The activation code is single-use and expires within an hour. SSM rotates the role credentials itself. At `down`, the instance is deregistered, which revokes it.
  - **Bonus:** SSM gives the control plane a way to run committed commands on the Linodes with **no inbound port at all**.
- **Win (EC2): no PIA access in this phase.** #627 has no Windows crawl, so Win's instance profile gets no read on the secret. When a Windows arm is booked, a separate role with that one permission is added in its own PR (Owner question 4).
- **Where the ARN lives:** the secret's ARN is a GitHub environment variable (`PIA_SECRET_ARN`, visible to collaborators only), passed into cloud-init. The repo keeps placeholders.

**The kill switch the owner can pull, fastest first:**
1. **Add the label `operator-stop` to the channel PR.** Both workflows check it first and refuse to act. Removing it restores service.
2. **Disable the two workflows** (GitHub → Actions → each workflow → Disable).
3. **Revoke the Anthropic workspace key, or set the workspace spend limit to $0.** No new session can start.
4. **Cut cloud access:** attach the prepared deny-all policy to `LodestarFleetOperator`, revoke the Linode fleet token, and remove the PIA read from the hybrid role.
5. **Tear down:** comment `/fleet down` (the owner may). The watchdogs remain the backstop.

The runbook (OP-3) gives these as a one-page "Stop everything" list.

## 5. First-run test (the runner verifies it once the operator token exists)
No VM is created.
1. **Channel:** the runner opens the draft channel PR and subscribes to it. The owner adds the GitHub token to the vault (setup step 2).
2. **Round trip:**
   - the runner posts `operator-instruction v1 … kind: query` with `ping`;
   - the bridge starts a session, which answers `operator-report v1 … kind: ack` with `pong` and its session ID;
   - the runner is woken. Pass: under 5 minutes end to end.
3. **Negative:** a comment with the instruction header from an account that isn't allowed doesn't start a session (the bridge's run log shows the refusal).
4. **Control plane, read-only:** the operator posts `/fleet status`, and `fleet.yml` answers with the inventory: no VMs.
5. **Kill switch:** add `operator-stop`, post a `ping`, confirm nothing runs, then remove the label.
6. **Injection test:** OP-5's corpus passes at the routine setting.
7. **Results:** the runner records them in the operator's decisions file. Only then does the first real fleet session go ahead, under #627's Phase 0.

## Owner setup (plain steps; values go only where named, never in the repo or chat)
1. **Anthropic Console:**
   1. Create a workspace, `lodestar-operator`.
   2. Set its spend limit to $200, with an alert at $100.
   3. Create a workspace API key and add it as the `ANTHROPIC_API_KEY` secret in the GitHub environment `cloud-operator` (step 3).
2. **The operator's GitHub token,** after Owner question 1:
   1. Create the account and token that question settles.
   2. In the Console, in the `lodestar-operator` workspace, create a vault with one environment-variable credential: name `GITHUB_TOKEN`, header only, allowed host `api.github.com`.
   3. Delete any local copy of the token.
3. **GitHub, in the repository settings:**
   1. Create the environments `cloud-operator` and `cloud-fleet`, each limited to the `main` branch.
   2. Add the variables `RUNNER_LOGIN`, `OPERATOR_LOGIN`, `AWS_FLEET_ROLE_ARN`, `PIA_SECRET_ARN` and `LODESTAR_AWS_ACCOUNT_ID`, plus the IDs `#625` already takes from the environment (`LODESTAR_AWS_SUBNET_ID`, `LODESTAR_AWS_SECURITY_GROUP_ID`, `LODESTAR_AWS_AMI_ID`, `LODESTAR_AWS_INSTANCE_PROFILE`).
   3. Environment variables are visible to collaborators, not the public.
4. **AWS (us-east-1).** OP-2 commits a CloudFormation template for items 2–6, so these become one reviewed stack, not hand-built resources.
   1. Add the IAM OIDC identity provider for `token.actions.githubusercontent.com` (audience `sts.amazonaws.com`).
   2. Role `LodestarFleetOperator`:
      - trust only `repo:dvortsis/lodestar-v2:environment:cloud-fleet`;
      - the permissions in §4;
      - also a prepared, unattached deny-all policy for the kill switch.
   3. Role `LodestarFleetHybridRole`:
      - trust `ssm.amazonaws.com`;
      - `AmazonSSMManagedInstanceCore`;
      - plus one inline statement allowing `secretsmanager:GetSecretValue` on the `lodestar/pia-vpn` ARN only.
   4. An S3 bucket for Terraform state: versioned, encrypted, public access blocked, readable and writable only by `LodestarFleetOperator` and the owner's admin.
   5. A private S3 bucket for scrubbed evidence, with a 14-day expiry.
   6. A secret `lodestar/akamai-fleet-token` (step 5).
   7. An AWS Budgets alert on the account at a monthly amount you choose.
5. **Akamai:** create a personal access token for the fleet (scopes as in §4, 90-day expiry), and store it only in `lodestar/akamai-fleet-token`.
6. **PIA:** the login is already in `lodestar/pia-vpn`. Confirm its JSON keys are `username` and `password`.

## Slices
Ids, estimated changed lines and dependencies. Every slice is `/security-review`, and every one runs on Opus (credentials and an untrusted-input parser; CLAUDE.md's delegation rules).

| Id | What | Lines | Deps |
|---|---|---|---|
| OP-1 | `fleet.yml` control plane: comment parser (strict grammar, tests and a fuzz-style property test), author and stop-label gates, `limits.yaml`, presets, `concurrency`, status comment. **Dry-run only**: no cloud call | ~600 | X-cloud-infra (#625) |
| OP-2 | Cloud wiring: the CloudFormation template for the OIDC role, hybrid role, deny-all policy and buckets; a non-interactive automation mode for the Deploy and Destroy scripts, accepted only inside `fleet.yml`'s environment; S3 remote state for Terraform; per-Linode SSM hybrid activation in cloud-init; the PIA bootstrap | ~700 | OP-1, X-cloud-infra-2 (it also edits `main.tf` and `variables.tf`) |
| OP-3 | Committed SSM documents for #627's steps: the leak test, block start at a rules commit, arm switch, outage watcher, status, scrubbed copy-off. Includes the scrubber with tests, and the runbook (#627's deliverable) with the "Stop everything" page | ~700 | OP-2 |
| OP-4 | The operator agent as files synced with `ant apply`: agent (system prompt, Sonnet 5.5 medium, toolset with web tools off, bash on `auto`), environment (`limited` networking, `api.github.com` only), vault declared without secrets. Also `operator-wake.yml` (the bridge, with the session budget) and the channel message formats | ~400 | OP-1 |
| OP-5 | The prompt-injection corpus and harness, and the first-run checklist (§5). Results go in a decisions file | ~300 | OP-4 |
| OP-G | Gate: §5 passes, the kill-switch drill passes, and one dry fleet cycle runs (`up` and `down` of the `screen` preset in dry-run). Then the first real fleet session under #627 | gate | OP-1–OP-5, `ext:owner-operator-setup` |

OP-3 and OP-4 can run in parallel; they own different paths. Total is about 2.7k lines in five PRs, each under 1,500.

## Owner questions
1. **The operator's GitHub identity.** A fine-grained token made on your account posts comments as you, so the workflows can't tell the operator from you or the runner by login.
   - **Recommendation:** a separate machine account (GitHub allows one free machine account per person) with no write access. Its comments are then plainly the operator's, and `fleet.yml` can accept `/fleet` from that login only.
   - **What to check:** GitHub may not let a separate account's fine-grained token select a repo in your personal account. If it doesn't, the fallback is a classic token with only `public_repo` on that no-write account. That can only comment, because the account can't push.
   - Which do you want?
2. **"A dedicated test project."** AWS and Akamai have no projects.
   - **AWS:** the real boundary is a separate AWS account in an Organization. Without one, the roles above are the boundary.
   - **Akamai:** the only real boundary is a separate Akamai account, because Linode tokens can't be limited to tagged resources.
   - Do you want separate accounts (recommended if the current ones hold anything else), or the scoped roles and token in the current ones?
3. **Scope of Q3.** `#625`'s access model says agents only read, and every deploy or delete is human-run.
   - Your Q3 answer lets the operator create and destroy VMs. In this design it does so only through `fleet.yml`'s allow-listed commands and limits, and it still holds no cloud credential.
   - `LodestarProvisioner` keeps its MFA, and interactive agents keep the observer role.
   - Confirm this is the scope you meant.
4. **Windows and PIA.** No Windows crawl is planned, so Win gets no PIA access now; a later PR adds it when a Windows arm is booked. Agree?
5. **Inbound SSH on the Linodes.** With SSM, the Linodes need no inbound port.
   - **Recommendation:** drop the admin `/32` SSH rule and keep Linode's LISH console as your way in.
   - That makes X-cloud-infra-2's address list unnecessary for the Linodes. Win would then reach Main's UI over SSM port forwarding instead of SSH, which needs SSM's advanced-instances tier on Main for that session (check its price).
   - Drop it, or keep SSH?
