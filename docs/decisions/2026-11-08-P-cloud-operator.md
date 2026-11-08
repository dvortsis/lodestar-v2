# P-cloud-operator decisions (#631)

The plan is `docs/plan/2026-11-08-cloud-operator.md`: an AI operator on Anthropic's hosted agent runs the cloud test fleet, and the runner manages it through comments on one draft PR. Docs only.

**Where it comes from.** It follows the owner's answers Q1–Q6 (relayed by the dashboard session, 2026-11-08) and the owner's follow-up in this session at about 18:05Z:
- the owner took the first draft's recommendations;
- the owner confirmed create/destroy;
- the owner chose and built the MFA-protected PIA broker.

**Owner-approved loosenings.** Three loosenings of `#625`'s access model are named in the plan's "Loosenings of `#625`, named" table, each with the owner answer that approved it:
1. unattended create and destroy of Linodes;
2. Terraform's non-interactive mode, without the typed phrase;
3. unattended root step scripts over SSH.

All three are for the control plane only.

**Nothing else loosens an owner or security decision, or changes an owner-set number.** The refinements below narrow the answers, and the runner flags them (§2.19).

- **The operator holds no cloud or VPN credential, although Q1 allows it to.**
  - **Why:** vault credentials are substituted at egress, so AWS SigV4 inside the sandbox fails, and raw keys there would be exposed to anything the agent runs.
  - **What it holds:** a 1-hour GitHub App installation token, refreshed by the bridge before each wake.
  - **How it acts:** it posts `/fleet` commands that `fleet.yml` checks and carries out.
- **A GitHub App instead of a machine account or a personal token.**
  - **Why not the alternatives:** a collaborator on a personal-account repo always has write access, and a separate account's fine-grained token may not reach this repo. The only fallback was a classic `public_repo` token, which isn't limited to this repo.
  - **What the App gives:** installed on this repo only, with Issues and Pull requests write and no contents, it meets Q4 exactly, and its comments are distinct and notify the owner.
- **PIA uses the owner's broker, unchanged.** The first draft proposed SSM hybrid activation; that's withdrawn, because it gave each Linode an AWS identity and the owner's rule is that Linodes get none.
  - **Host keys:** pinned through cloud-init, not trusted on first use.
  - **Exits:** the owner picks the PIA server per bridge run, so exits stay distinct, stay off the Unraid host's exit, and stay private.
  - **On the VM:** the WireGuard configuration is made there, and the credentials are deleted.
  - **Consistent with #627:** WireGuard material moves only host to host over SSH.
- **SSH stays on the Linodes:** from the owner's admin `/32`, and from a job's own `/32` only while that job runs. The first draft's recommendation to drop SSH for SSM no longer applies (see the previous item).
- **The first version runs `screen` only (Main and Worker).**
  - The Windows lanes stay human-run, so the control plane has no EC2, SSM, `iam:PassRole` or root SSM access at all.
  - A Windows preset needs its own slice and approval.
- **Public logs carry nothing private.** Every ID and ARN is an environment secret, cloud-tool output goes only to a private bucket, plan output and user data are never printed, and there are no artifacts. OP-1 tests this.
- **Limits are code in the control plane:**
  - one preset and one fleet;
  - no extend;
  - a $40 monthly fleet-compute cap;
  - allow-listed steps;
  - author-ID gates (not logins), with the filter job before any secret;
  - busy refusal instead of a cancelling queue;
  - one commit per fleet.
- **The bridge enforces spend and never leaves a session stalled:**
  - a rate limit (6 an hour, 60 a day);
  - the $200 total, summed from session `list_cost`;
  - session budgets ($3 routine, $15 triage);
  - it denies approval pauses, and posts the escalation itself on a budget stop, an error or a timeout.

  These are runner parameters under §2.26.
- **Terraform state moves to an S3 backend,** with Terraform 1.10 or later for native locking. An unattended control plane can't use the operator's local state, and `#625` already blocks `apply` until state ownership is reconciled.
- **Models:** Sonnet 5.5 at medium (set explicitly; its default is high) for the routine loop, and Opus 5.5 at high (set explicitly; its default is medium) for triage. Max is never used.
  - **Deployment gate:** a prompt-injection corpus, each item 5 times, escalating Sonnet medium → Sonnet high → Opus medium → Opus high. If Opus at high fails, the runner stops and tells the owner.
  - **For the runner:** how the agent's config file squares with CLAUDE.md's rule on model identifiers.
- **Evidence:** an allow-list scrubber, on the VM and again in the control plane. Scrubbed aggregates reach the runner as a result comment, and the repo only through a reviewed PR.
- **Residual risk, stated:** the runner shares the owner's login with every Claude session. Any session posting as the owner could wake the operator. The rate limit, the budgets, the $200 cap and the control plane's limits bound it.

## Flagged for the owner
- **Open question 1:** keep the per-session MFA step for PIA (recommended), or approve an unattended variant later.
- **Open question 2:** require the owner's approval on the control plane's files (recommended).
- **New runner parameters (§2.26):** the session budgets, the wake rate limit and the $40 monthly fleet-compute cap.
