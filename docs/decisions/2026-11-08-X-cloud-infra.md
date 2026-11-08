# X-cloud-infra decisions (from #623)

The owner asked the runner to review #623 ("Reconcile Lodestar AWS and Akamai infrastructure", written on the owner's behalf by another tool) and merge it if everything passes. This slice brings #623's content to `main` as fresh commits on `slice/X-cloud-infra`, so the live AWS resource IDs in #623's only commit never enter `main`'s history. It supersedes #623. Nothing here changes an owner decision; the access model below is new and is flagged to the owner (§2.19).

## What is imported
- **`infra/aws/expiration-guard/deployed/template.json`:** the CloudFormation stack `lodestar-expiration-guard`. A Lambda every 15 minutes terminates EC2 instances tagged `Project=Lodestar`, `Environment=Test`, `ManagedBy=Terraform`, in the expected VPC and security group, of type `c6a.xlarge`, once `ExpiresAt` or 48 hours after launch has passed. Its role may describe instances (`ec2:DescribeInstances` has no resource-level permissions, so `Resource: "*"` is the only form) and terminate only instances carrying those three tags in its own account and region. `NotificationEmail` is `NoEcho` with no default; `LodestarVpcId` and `WindowsSecurityGroupId` are required parameters with no defaults.
- **`infra/akamai/watchdog/deployed/`:** the stack `lodestar-akamai-watchdog`. A Lambda every 30 minutes reads a Linode token from Secrets Manager (`lodestar/akamai-token`) and deletes Linodes that carry the tags `lodestar` and `test`, a `lodestar-` label, region `us-east`, an approved type, were created after `MIN_CREATED_UTC` and are at least 48 hours old. It re-reads each one before deleting, deletes at most 4 a run, refuses to act on more than 10 eligible, and confirms each deletion with a 404. **It deletes instances only**, never firewalls, volumes or reserved addresses. Its role can read only that secret, write only its log group and publish only to `lodestar-expiration-alerts`. `index.py` is the source; `build_template.py` inlines it into the template and reproduces the committed JSON byte for byte.
- **`infra/akamai/terraform/`:** the Linode Terraform (off by default: no main VM, zero workers), cloud-init, a VPN gateway example and the PowerShell operator scripts.
- **`.agents/skills/lodestar-fleet/SKILL.md`:** an agent skill for the fleet, and **`docs/operations/lodestar-cloud-assets.md`**, the operational source of truth.

## The templates are representations, not deployed copies
- **Parameterized, not byte-identical.** Account-specific values were replaced by parameters or AWS pseudo-parameters (`${AWS::AccountId}`, `${AWS::Region}`). A future stack update must go through a reviewed change set with the existing parameter values supplied by the human.
- **One deliberate source change.** The watchdog's API path allowlist admitted `/networking/firewalls?page=`, a path the code never calls; it is removed, with a test that the guard rejects it. The deployed Lambda keeps the extra entry, harmless, until the next stack update.

## Not verified
- **End-to-end deletion** by either reaper: only non-destructive inventory and dry-run checks have passed.
- **Terraform state reconciliation.** The live fleet's state is private and not in Git. `terraform apply` from a fresh checkout could create a second fleet or recreate watchdog-deleted machines; the docs say not to apply until state ownership is reconciled.
- **Whether the stacks are still live**, or still match these templates. Nothing in this slice ran against AWS or Akamai.
- **`terraform fmt -check` and `terraform validate`**, and the PowerShell scripts: neither Terraform nor PowerShell is available in the cloud session that prepared this slice, and no CI job runs them. The Python watchdog tests ran and pass.
- **The AMI.** Whether the previously hard-coded AMI is a public Amazon base image couldn't be checked without AWS access, so it is a parameter like the other IDs.

## Agent access model
- **Agents only read.** An agent may inspect, plan, estimate cost and report. Every deploy, reboot, remote session, state rewrite or delete is **human-run only**, marked 🔒 in the skill, and an agent never runs one.
- **AWS:** agents use `LodestarObserver` (`ec2:Describe*` only, with explicit denies for provisioning, termination, SSM and IAM). `LodestarProvisioner` is assumable only by the human `lodestar-admin` with MFA; an explicit deny stops the agent user from assuming it.
- **Akamai:** agents may run `terraform plan` and `terraform output` with a read-only token the human supplies, never `apply` or `destroy`.
- **Typed confirmation, no bypass.** Every paid or destructive script asks the human to type a phrase, compared case-sensitively. #623's `Destroy-AwsWindows.ps1 -Force` skipped it; that switch is removed. With more than one candidate instance the script now stops instead of picking the first.

## No live identifiers in the repository
- **The repository is public**, so the skill, scripts and docs hold no account IDs or resource IDs (subnet, security group, AMI, instance role or instance profile). #623 had a security group, a subnet, an AMI, and an instance role and instance profile with CloudFormation-generated suffixes.
- **Where the values live now.** `Deploy-AwsWindows.ps1` takes them as parameters or from `LODESTAR_AWS_ACCOUNT_ID`, `LODESTAR_AWS_AMI_ID`, `LODESTAR_AWS_SUBNET_ID`, `LODESTAR_AWS_SECURITY_GROUP_ID` and `LODESTAR_AWS_INSTANCE_PROFILE`. It has no defaults and validates their format before any AWS call. The docs use placeholders (`<SUBNET_ID>`, `<SECURITY_GROUP_ID>`, `<INSTANCE_ROLE_NAME>`).
- **#623's branch still shows the old values** until the owner deletes it.

## `.gitignore`
- **Repo-wide patterns are limited to Terraform** (`.terraform/`, state, plans, `terraform.tfvars`, `*.auto.tfvars`).
- **Everything else is scoped to `infra/**`:** credentials, keys, `.env`, local state, logs and archives. #623's repo-wide `**/*.pem`, `**/*.key`, `**/*.log`, `**/secrets/**` and `**/.env.*` matched 7 tracked files: TLS test fixtures under `crates/sources-importers` and soak evidence logs under `docs/soak/`. New files of those kinds would have been silently ignored. `git ls-files -ci --exclude-standard` prints nothing.

## Smaller corrections
- **`Lodestar-Fleet.ps1 reconcile`** marked the AWS instance destroyed on *any* CLI error, an expired session included. It now does so only when EC2 reports `terminated` or `InvalidInstanceID.NotFound`.
- **`Destroy-AwsWindows.ps1`** treated a failed inventory call as "fleet is clean"; it now stops.
- **The VPN example's `TZ`** named a local time zone; it is `Etc/UTC` (`docs/owner-decisions.md` §2.35).
- **Stale text fixed.** The Terraform README no longer says the project has no automatic deletion, and no longer refers to a ZIP package or a personal budget. The skill no longer refers to paths that don't exist (`cleanup\...`), a personal `Downloads` folder, or agents operating under `LodestarProvisioner`.
