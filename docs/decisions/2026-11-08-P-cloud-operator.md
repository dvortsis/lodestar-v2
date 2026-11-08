# P-cloud-operator decisions (#631)

The plan is `docs/plan/2026-11-08-cloud-operator.md`: an AI operator on Anthropic's hosted agent runs the cloud test fleet, and the runner manages it through comments on one draft PR. Docs only.

**Where it comes from.** It follows the owner's answers Q1–Q6 (relayed by the dashboard session, 2026-11-08) and the owner's follow-up in this session at about 18:05Z:
- the owner took the first draft's recommendations;
- the owner confirmed create/destroy;
- the owner chose and built the MFA-protected PIA broker;
- at about 18:20Z, the owner said they can't enter MFA for each session ("I can't be entering the MFA for each session") and that the runner manages the control plane ("I don't need to approve all the control agent's code. Let the runner manage the control agent").

**Owner-approved loosenings.** Seven loosenings of `#625`'s access model are named in the plan's "Loosenings of `#625`, named" table, each with the owner answer that approved it:
1. unattended create and destroy of Linodes;
2. Terraform run without the typed phrase, through the control plane's own entry point; the `.ps1` scripts stay phrase-only;
3. unattended root step scripts over SSH;
4. the PIA login read unattended by the `deliver-pia` job, not only behind MFA;
5. control-plane changes merging on the runner's review, with no owner approval;
6. the operator App's Pull requests write, which reaches every PR in the repo, beyond Q4's "comments only". It's contained by the stop variable, unedited-comment trust, no state in comments, and a tripwire: `pull_request_target` and `issue_comment` triggers plus a sweep of issue events and reviews;
7. per-job SSH ingress from the job's GitHub-hosted runner `/32`, with runtime firewall writes, including a Linode firewall token (whole-firewall scope) behind `LodestarPiaDelivery`, which the reaper audits hourly.

All seven are for the control plane only. The sixth and seventh are inferred from Q4, and from Q1 and Q3, rather than quoted, so the runner flags them to the owner.

**Nothing else loosens an owner or security decision, or changes an owner-set number.** The refinements below narrow the answers, and the runner flags them (§2.19).

- **The operator holds no cloud or VPN credential, although Q1 allows it to.**
  - **Why:** vault credentials are substituted at egress, so AWS SigV4 inside the sandbox fails, and raw keys there would be exposed to anything the agent runs.
  - **What it holds:** a 1-hour GitHub App installation token, refreshed by the bridge before each wake.
  - **How it acts:** it posts `/fleet` commands that `fleet.yml` checks and carries out.
- **A GitHub App instead of a machine account or a personal token.**
  - **Why not the alternatives:** a collaborator on a personal-account repo always has write access, and a separate account's fine-grained token may not reach this repo. The only fallback was a classic `public_repo` token, which isn't limited to this repo.
  - **The App's scope:** installed on this repo only, with Pull requests write and Metadata read, and no Issues or Contents.
  - **What that scope still allows beyond Q4's "comments only":** editing others' comments, changing labels, and closing or retitling PRs.
  - **So:**
    - the soft stop is the repository variable `OPERATOR_STOP`, which the App can't change;
    - only unedited comments are trusted;
    - `fleet.yml` keeps its state in S3, never in comments;
    - the token lasts 1 hour and is revoked when the session ends.
- **PIA uses the owner's broker, made unattended at the owner's request.**
  - **Withdrawn first:** the first draft's SSM hybrid activation, because it gave each Linode an AWS identity and the owner's rule is that Linodes get none.
  - **The reader:** a separate `deliver-pia` job, in its own `pia-delivery` environment, assumes `LodestarPiaDelivery` by OIDC. That role's permissions are `GetSecretValue` on the exact `lodestar/pia-vpn` and `lodestar/akamai-firewall-token` ARNs, plus `s3:GetObject` on `fleet.json`; the fleet role can't read it, and the operator never sees it.
  - **The transfer:** the job masks the values and streams them over SSH stdin to pinned host keys, into root-only files in `/run`.
  - **On the VM:** the WireGuard configuration is made there, and the credentials are deleted.
  - **Exits:** they come from a private `PIA_SERVERS` secret, distinct for Main and Worker and never the Unraid host's.
  - **Consistent with #627:** WireGuard material moves only host to host over SSH.
  - **The delivery job's own boundary:**
    - its environment allows `main` only, and it repeats the author, edit and stop gates itself;
    - its role's trust requires `aud` and a `sub` naming `fleet.yml@refs/heads/main`;
    - it takes targets and pins only from `fleet.json`, never from comments or arguments;
    - it runs inside the fleet lock, and its firewall rule is API-managed and labelled by run;
    - it reads the secret with boto3, masks each value, and discards the remote output;
    - it delivers only to a fleet whose pinned commit is an ancestor of `main`;
    - its role lives in the secret's own account.
  - **The Unraid host's exit** is re-confirmed against `PIA_SERVERS` when each fleet session is booked.
  - **Residual risk:** a malicious merged change to the delivery code could leak the password, and `main`'s protection needs no approvals, so the merge guard is procedural. The same holds for any newly merged workflow that declares one of the three environments and reads their secrets. The guards are the runner's `/security-review` on those paths, the separate role and environment, and a dedicated test-only PIA login. The owner's MFA path stays for manual use.
- **SSH stays on the Linodes:** from the owner's admin `/32`, and from a job's own `/32` only while that job runs. The first draft's recommendation to drop SSH for SSM no longer applies (see the previous item).
- **The first version runs `screen` only (Main and Worker).**
  - The Windows lanes stay human-run, so the control plane has no EC2, SSM, `iam:PassRole` or root SSM access at all.
  - A Windows preset needs its own slice and approval.
- **Public logs carry nothing private.** Every ID and ARN is an environment secret, cloud-tool output goes only to a private bucket, plan output and user data are never printed, and there are no artifacts. OP-1 tests this.
- **Commit arguments must be ancestors of `main`,** not just 40 hex characters, because a fork PR's commit is fetchable. Image digests come only from that commit's files, and the per-fleet pin lives in the state bucket.
- **Lifetime is enforced twice.** An hourly reaper in `fleet.yml` runs `down` at the expiry in `fleet.json` (at most 44 h), and also reaps any Lodestar Linode older than 44 h by creation time. `OPERATOR_STOP` exempts `down` and the reaper. The watchdog follows at 48 h; it's pointed at the fleet's account in setup, and OP-G proves it with #627's 0a canary, because its deletion path is unproven (#625).
- **Limits are code in the control plane:**
  - one preset and one fleet;
  - no extend;
  - a $40 monthly fleet-compute cap;
  - allow-listed steps;
  - author-ID gates (not logins), with the filter job before any secret;
  - S3 conditional-write locks, with a busy refusal, instead of GitHub `concurrency`, which cancels pending runs;
  - one commit per fleet.
- **The bridge enforces spend and never leaves a session stalled:**
  - a rate limit (6 an hour, 60 a day);
  - the $200 total, summed from session `list_cost`;
  - session budgets ($3 routine, $15 triage);
  - it denies approval pauses, and posts the escalation itself on a budget stop, an error or a timeout.

  These are runner parameters under §2.26.
- **OIDC trust is pinned to `aud` and to a full `sub` (`StringEquals`) naming the environment and the exact workflow at `refs/heads/main`:** `fleet.yml` for the fleet and PIA roles, and `operator-wake.yml` or `operator-apply.yml` for the bridge role. None of these workflows has `workflow_call`. The repo's subject template is customized to include `job_workflow_ref`.
- **A third OIDC role, `LodestarOperatorBridge`,** gives the bridge and `operator-apply.yml` the lock prefix and the lock-file object only, never the state bucket.
- **`deliver-pia` reads only `fleet.json`** (addresses, fingerprints, firewall IDs, pin, expiry), never the whole Terraform state, which holds host private keys. It reaches the Linodes with a firewall-only Linode token that opens its own `/32`.
- **`fleet.yml` has no `workflow_call` and uses no reusable workflow,** so the `job_workflow_ref` pin can't be sidestepped.
- **The runner never holds the Anthropic workspace key.** `ant apply` runs from `operator-apply.yml` in `cloud-operator`, which renders the model from the variables at apply time. `claude-lock.json` stays in the private bucket, because it holds resource IDs.
- **Terraform state moves to an S3 backend,** with Terraform 1.10 or later for native locking. An unattended control plane can't use the operator's local state, and `#625` already blocks `apply` until state ownership is reconciled.
- **Models:** Sonnet 5.5 at medium (set explicitly; its default is high) for the routine loop, and Opus 5.5 at high (set explicitly; its default is medium) for triage. Max is never used.
  - **Deployment gate:** a prompt-injection corpus, each item 5 times, stopped at $50 of the $200, escalating Sonnet medium → Sonnet high → Opus medium → Opus high. If Opus at high fails, the runner stops and tells the owner.
  - **Model IDs stay out of the repo** (the runner's ruling): they're the Actions variables `OPERATOR_MODEL_ROUTINE` and `OPERATOR_MODEL_TRIAGE`, and committed files name only the variables.
- **Job SSH rules use one mechanism, the Linode API under the fleet lock,** labelled by run. Terraform keeps only the admin rule, and stale rules are swept by Actions-API status.
- **The injection harness can't act:** its own vault with a dummy token, no route to `api.github.com`, attempts detected from tool-call events, and no runs while a fleet exists.
- **Evidence:** an allow-list scrubber, on the VM and again in the control plane. Scrubbed aggregates reach the runner as a result comment, and the repo only through a reviewed PR.
- **Residual risk, stated:** the runner shares the owner's login with every Claude session. Any session posting as the owner could wake the operator. The rate limit, the budgets, the $200 cap and the control plane's limits bound it.

## Flagged for the owner
- **Both open questions are answered** (no per-session MFA; the runner manages the control plane). The runner asks for a fresh Opus `/security-review` on any change to the control-plane paths.
- **New runner parameters (§2.26):** the session budgets, the wake rate limit and the $40 monthly fleet-compute cap.
