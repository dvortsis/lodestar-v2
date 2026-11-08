# X-cloud-infra: bring PR #623 (Lodestar AWS and Akamai infrastructure) to a mergeable state

**Who decided:** the owner asked the runner to review #623 and merge it if everything passes. On 2026-11-08 the owner told the runner to "spin up a 5th session" for it. That's a one-off exception to the 4-slot limit (§2.42), on the owner's own instruction.
**Model:** Opus. **Review:** a fresh Opus reviewer plus `/security-review`, because this is cloud infrastructure, an agent skill that controls paid resources, and a public repository.
**Steward:** follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. The runner is session_01EYhgh4JX3AmHzKXPjYCoUg; message it with the Claude Code Remote MCP `send_message` tool.

## Starting point
- **PR:** dvortsis/lodestar-v2#623, "chore(infra): Reconcile Lodestar AWS and Akamai infrastructure".
  - Head `a01bd12a77b7788a1feb3da46c6cd7655ce53fef` on `chore/lodestar-cloud-reconciliation-20261108`: one commit, 31 files, 2,970 added lines, base main 29107fc8.
  - Another tool (not a Claude session) wrote it on the owner's behalf.
- **CI:** green, but CI runs nothing on `infra/`. No job runs terraform, the Python tests or the PowerShell tests.
- **The repository is PUBLIC.** Treat anything that lands on `main` as published permanently.

## The runner's review findings (all blocking)
1. **Live AWS identifiers, although the PR says they were scrubbed.** `docs/operations/lodestar-cloud-assets.md` also claims they were.
   - `.agents/skills/lodestar-fleet/SKILL.md`:
     - the security group `sg-0db038e39ea004fb2`;
     - the subnet `subnet-0cced923e2d20e864`, which also appears truncated in the diagram;
     - the instance role with its CloudFormation-generated suffix, `lodestar-foundation-SsmInstanceRole-HRnWC1gAZsqn`.
   - `infra/akamai/terraform/scripts/Deploy-AwsWindows.ps1` (~lines 109–111): `$amiId`, `$subnetId` and `$securityGroupId` are hard-coded, and `tests/Test-LodestarAutomation.ps1` repeats the AMI.
   - **Fix:** make them parameters (script `param()` with no live defaults, or environment variables) and use placeholders in docs (`<SUBNET_ID>`, `<SECURITY_GROUP_ID>`, `<INSTANCE_ROLE_NAME>`). A public AMI ID is fine as a documented default only if it is a public Amazon base image; check that it is, or make it a parameter too.
2. **The personal path:** the SKILL.md diagram has `Project: Downloads\lodestar-akamai-terraform`. Replace it with the repository path `infra/akamai/terraform`. Sweep the whole PR for any other personal or local paths and user names.
3. **The `.gitignore` is too broad.** The new repo-wide patterns (`**/*.key`, `**/*.pem`, `**/*.log`, `**/secrets/**`, `**/.env.*`) match 7 files already tracked on `main`:
   - `crates/sources-importers/src/bitmagnet/testdata/{ca.pem,other-ca.pem,server.key,server.pem}`;
   - three soak evidence `.log` files under `docs/soak/`.

   New test fixtures and evidence of those kinds would be silently ignored. **Fix:**
   - scope the credential, state and log patterns to `infra/**` (and `**/.terraform/`, `**/*.tfstate*`, `**/*.tfplan`, `**/terraform.tfvars`, which are safe repo-wide);
   - then check that `git ls-files -ci --exclude-standard` prints nothing.
4. **The docs contradict themselves.**
   - SKILL.md's diagram says the Akamai watchdog cleans orphaned firewalls. The PR body, `lodestar-cloud-assets.md` and the Lambda code say it reaps instances only.
   - The inline Lambda also allowlists `/networking/firewalls?page=` paths it never calls. Remove the dead allowlist entry or explain it.
   - SKILL.md says agents have "zero access" to `LodestarProvisioner`, yet its direct-ops examples run read-only commands with `--profile lodestar-provisioner`. Point read-only examples at the observer profile. Label every privileged command (deploy, reboot, terminate, SSM session) as **human-run only, never by an agent**.
5. **Repository process (CLAUDE.md):**
   - Add **`docs/decisions/2026-11-08-X-cloud-infra.md`**. Adding cloud infrastructure and a repository agent skill that can control paid resources is a new decision. Record: what's imported, what is NOT verified (end-to-end deletion, state reconciliation), the agent-access model (observer read-only, provisioner human with MFA), and that the templates are parameterized representations, not deployed copies.
   - **Size:** 2,970 lines is over the ~1,500-line guide. Either state the reason in the PR description (mostly imported templates and scripts, reviewed as one unit) or split it, for example into (a) the AWS expiration guard, (b) the Akamai watchdog, and (c) the Terraform, scripts, skill and docs. Your call; say which in the PR.
   - All times UTC, and no model identifiers anywhere.

## Checks the runner hasn't done yet (yours)
- `index.py` matches the Lambda inlined in `lodestar-akamai-watchdog.json` byte for byte, and `build_template.py` reproduces the JSON exactly. Run it and `git diff --exit-code`.
- `python3 -m pytest infra/akamai/watchdog/deployed/test_watchdog.py` (or `python3 -m unittest`) passes.
- Run `terraform fmt -check -recursive` and `terraform validate` if terraform is available in the container. If it isn't, say so; don't install anything outside the package registries the environment allows.
- Read every PowerShell script for:
  - anything that runs a destructive or paid action without the typed human confirmation;
  - credentials passed on a command line, written to disk or logged;
  - `Invoke-Expression` on input.
- Review both CloudFormation templates for IAM least privilege (no `*` actions or resources where a scoped one works), the Lambda's delete scope (tag, label, region, type and age checks), and that `NoEcho` parameters have no literal defaults.
- A secret scan of the final diff:
  - no 12-digit account IDs, emails, IPs outside the documentation ranges, keys or tokens;
  - no live resource IDs.

## Branch and history (important on a public repo)
The live IDs are in #623's only commit. **Don't rewrite that branch** (it isn't yours, and no force-push). Instead:
1. Create **`slice/X-cloud-infra`** from current `origin/main`.
2. Bring the fixed files over as fresh commits, so the live IDs never enter `main`'s history.
3. Open a new PR titled `X-cloud-infra: Lodestar AWS and Akamai infrastructure (from #623)`, and say in its description that it supersedes #623.
4. Don't close #623 yourself. The runner closes it at merge, and the owner deletes the old branch: the IDs stay visible on that branch until they do.

If you find a real credential (a key, token or password), stop. Tell the runner at once and push nothing.

## Owns
- `infra/**`, `.agents/skills/lodestar-fleet/**`, `docs/operations/lodestar-cloud-assets.md`, the infra section of `.gitignore`, and `docs/decisions/2026-11-08-X-cloud-infra.md`.

**Not yours:** any change to deployed cloud resources. Never run anything against AWS or Akamai: no credentials exist in this container, and none may be requested. Don't add CI jobs without asking the runner first.

## When done
CI green on all platforms → message the runner with the PR number and head SHA.
