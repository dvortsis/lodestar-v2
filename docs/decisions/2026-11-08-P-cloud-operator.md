# P-cloud-operator decisions

The plan is `docs/plan/2026-11-08-cloud-operator.md`: an AI operator on Anthropic's hosted agent runs the cloud test fleet, and the runner manages it through comments on one draft PR. Docs only.

**Where it comes from.** It follows the owner's answers Q1–Q6, relayed by the dashboard session on 2026-11-08. Every refinement below **narrows** what those answers allow; none loosens an owner decision or changes an owner-set number. They're the runner's to flag (§2.19).

**Changes to `#625`'s model.** `#625`'s agent access model ("agents only read") is changed only as the owner's Q3 allows: fleet commands run through a committed, limit-checked workflow. Owner question 3 asks the owner to confirm that scope.

- **The operator holds no cloud or VPN credential, although Q1 allows it to.**
  - **Why:** Managed Agents vault credentials are substituted at egress, so AWS SigV4 signing inside the sandbox fails. Raw keys in the sandbox would be exposed to anything the agent runs.
  - **What it holds:** only a GitHub token, in a vault, header-only, limited to `api.github.com`.
  - **How it acts:** it posts `/fleet` commands. A committed workflow (`fleet.yml`) checks them against hard limits and runs the committed scripts.
- **Limits are code in the control plane, not prompt rules:**
  - presets only, one fleet at a time;
  - Win's expiry at most 42 h, no extend;
  - a monthly VM-hour cap;
  - allow-listed SSM documents;
  - author and stop-label gates.
- **AWS access without stored keys.** GitHub OIDC assumes `LodestarFleetOperator`, scoped to tagged c6a.xlarge instances, `Lodestar-*` SSM documents, hybrid activations, `iam:PassRole` on one role, the state bucket and the Linode fleet token. It can't read the PIA secret and has no IAM. `LodestarProvisioner` keeps its MFA.
- **The Linodes get the PIA login through SSM hybrid activation, not an IAM user key.**
  - A single-use activation, expiring within 1 h, gives each Linode the hybrid role. That role may read only `lodestar/pia-vpn`.
  - SSM rotates its credentials, and deregistration at `down` revokes it.
  - The alternative, a scoped IAM user, would leave a long-lived key in Linode metadata and Terraform state.
  - Neither the operator nor the control plane reads the password.
- **Win gets no PIA access** while no Windows crawl is planned (Owner question 4).
- **A separate Linode token for the control plane** (`lodestar/akamai-fleet-token`), so revoking it never stops the Akamai watchdog.
- **Terraform state moves to an S3 backend.** An unattended control plane can't use the operator's local state, and `#625` already blocks `apply` until state ownership is reconciled.
- **One session per wake, with a hard session budget:** $3 routine, $15 triage, as the runner's §2.26 parameters. Durable state lives in the PR, so an injected instruction can't persist between wakes.
- **Models:** Sonnet 5.5 at medium for the routine loop, and Opus 5.5 at high (set explicitly; its default is medium) for triage. Max is never used. A prompt-injection corpus gates deployment, escalating Sonnet medium → Sonnet high → Opus medium → Opus high, and stopping for the owner if Opus at high fails.
- **The total budget is enforced twice.** There's the Console workspace limit ($200, alert at $100, as the runner proposes). Console workspace limits are monthly and the owner's $200 is a total, so the runner also stops the operator at $200 cumulative.
- **Data hygiene:** only the committed `copy-off` step moves data. Its scrubber runs on the VM and again in the control plane. Evidence goes to a private bucket and reaches the repo only through a runner-reviewed PR.

## Flagged for the owner
- The five owner questions in the plan:
  1. the operator's GitHub identity;
  2. separate AWS and Akamai accounts;
  3. the Q3 scope;
  4. no PIA on Win;
  5. dropping inbound SSH on the Linodes.
- The session budgets and the 200 VM-hour monthly cap are new runner parameters (§2.26).
