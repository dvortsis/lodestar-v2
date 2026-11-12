# Windows test machine on AWS: owner guide

> **Draft (2026-11-12Z): not yet tried end to end.** The Windows sign-in path below is still being checked; Claude will update this page before you use it.

One disposable Windows Server machine in AWS where a Claude session can build and test Lodestar. You create it and delete it yourself, with your own MFA. Nothing here is automatic and nothing is left open to the internet.

Words used below:
- **Instance**: the AWS name for one virtual machine.
- **Root volume**: the machine's disk. It is billed on top of the machine.
- **MFA**: the 6-digit code from your authenticator app.
- **Fleet Manager Remote Desktop**: a window onto the machine's desktop that opens in the AWS console. It goes through AWS Systems Manager (SSM), so no remote-desktop port is open on the machine.

## 1. What it costs

| Item | Number | Source |
|---|---|---|
| Machine, `c6a.xlarge` with Windows licence, us-east-1 | about $0.337 per hour | `Deploy-AwsWindows.ps1:147`, `docs/plan/2026-11-08-cloud-test-plan.md:70` |
| 48 hours of that | about **$16.18** (0.337 x 48) | `Deploy-AwsWindows.ps1:148` multiplies the rate by the lease hours |
| Root volume, 100 GiB | about $0.50 to $0.70 for 48 hours (my estimate at $0.08 to $0.10 per GiB-month; UNVERIFIED, the script does not set the disk type, `docs/plan/2026-11-08-cloud-test-plan.md:18`) | |
| Public IP address, if the subnet gives one | up to about $0.24 for 48 hours (UNVERIFIED, depends on the subnet) | |
| NAT gateway, if the subnet sends traffic through one | billed per GB; UNVERIFIED, the plan says to check (`docs/plan/2026-11-08-cloud-test-plan.md:71`) | |

Expect about **$17 in total** for a full 48 hours, well under your $100 limit. The hard limit on the lease is 48 hours (`Test-ExpirationPolicy.ps1:50-55`).

Recommendation: use `-RootVolumeGiB 100`. The default is 50, and the plan says 50 is too small for Windows, the Rust build tools, Node, a browser and release builds (`docs/plan/2026-11-08-cloud-test-plan.md:36`). 100 is the most the IAM policy allows (`Deploy-AwsWindows.ps1:30-34`).

## 2. Before you start

Run everything in a normal PowerShell window (Windows Terminal is fine; PowerShell ISE is refused, `Interactive.ps1:21`). You must type the confirmation phrases yourself. Nothing can be piped in.

1. Open PowerShell in your Lodestar checkout, then go to the scripts folder:
   ```powershell
   cd infra\akamai\terraform
   ```
2. Set the five values. They are account-specific, so they are never stored in the repository. The script refuses to run without all five (`Deploy-AwsWindows.ps1:10-17`, `:80-83`, `:103`):
   ```powershell
   $env:LODESTAR_AWS_ACCOUNT_ID        = "<your 12-digit account ID>"
   $env:LODESTAR_AWS_AMI_ID            = "ami-..."      # Windows Server 2022 or 2025 Base
   $env:LODESTAR_AWS_SUBNET_ID         = "subnet-..."
   $env:LODESTAR_AWS_SECURITY_GROUP_ID = "sg-..."       # allows no inbound traffic
   $env:LODESTAR_AWS_INSTANCE_PROFILE  = "<instance profile name>"   # the SSM profile
   ```
   You should already have these from earlier setup. The repository does not store them (`docs/operations/lodestar-cloud-assets.md:24`). The AMI must be one the IAM policy allows: Windows Server 2022 or 2025 Base (`.agents/skills/lodestar-fleet/SKILL.md`, "Provisioning").
3. Sign in with MFA (it asks for the 6-digit code):
   ```powershell
   aws sts get-caller-identity --profile lodestar-provisioner
   ```
   The sign-in lasts about one hour (`.agents/skills/lodestar-fleet/SKILL.md:72`). Repeat this command if a script later says the session is not active.

## 3. Create it

1. Dry run. It checks every parameter and permission and creates nothing:
   ```powershell
   .\scripts\Deploy-AwsWindows.ps1 -DryRun -Hours 48 -RootVolumeGiB 100
   ```
   Success looks like `PASS: DryRunOperation succeeded` (`Deploy-AwsWindows.ps1:171`).
2. Create it:
   ```powershell
   .\scripts\Deploy-AwsWindows.ps1 -Hours 48 -RootVolumeGiB 100
   ```
3. Read the plan box it prints (type, disk, cost, expiry time). To go ahead, type exactly `DEPLOY 1 AWS WINDOWS INSTANCE` (`Deploy-AwsWindows.ps1:179`). Anything else cancels and creates nothing.
4. It prints `Instance provisioned: i-...`. Note that ID. It is also saved in `state\aws-fleet-state.json`.

Facts about the script:
- It refuses to run if any instance tagged `Project=Lodestar` is already pending, running, stopping or stopped (`:121-134`). One machine at a time.
- The disk is encrypted. The machine requires the newer, safer metadata mode (`HttpTokens=required`, `:161-162`).
- The machine gets tags `Project=Lodestar`, `Environment=Test`, `ManagedBy=Terraform` and `ExpiresAt` (`:149-155`).
- It needs only AWS itself. It does not use Terraform state, an S3 bucket, the fleet-control stack, GitHub OIDC or the cleanup broker. It calls plain `aws ec2` commands under the `lodestar-provisioner` and `lodestar-admin` profiles (`:87`, `:105`, `:121`, `:156-165`).

Wait about 5 to 10 minutes after creation. Windows needs that long to start and to register with SSM (my estimate; UNVERIFIED).

## 4. Connect: Fleet Manager Remote Desktop (no open port)

1. Sign in to the AWS console as your usual administrator (with MFA). Set the region (top right) to **N. Virginia (us-east-1)**.
2. Search for **Systems Manager** and open it. In the left menu choose **Fleet Manager**.
3. Find the instance ID from step 3 in the list. If it is missing or says "Connection lost", wait a few minutes and refresh. If it never appears, see "If something fails" below.
4. Select it, then **Instance actions**, then **Connect with Remote Desktop**.
5. Choose the **Key pair** or **User credentials** option. Choose **Session Manager credentials / Use the Fleet Manager console session**, whichever the page offers, as the Windows built-in `Administrator` has no password you know. UNVERIFIED: the repository never says how the Windows password is set. If the page insists on a password, ask before going further. Do not open any port to work around it.
6. Select **Connect**. A Windows desktop opens in your browser tab.

UNVERIFIED: whether your console user (not the `LodestarProvisioner` role) is allowed to use Fleet Manager Remote Desktop. The repository only states that `LodestarProvisioner` has SSM Session Manager (`.agents/skills/lodestar-fleet/SKILL.md:67`). The plan names Fleet Manager as the route (`docs/plan/2026-11-08-cloud-test-plan.md:221`) but nothing in the repository proves it works on this account.

A fallback that is documented: a command-line session, from PowerShell on your PC (needs the Session Manager plugin for the AWS CLI; UNVERIFIED that it is installed):
```powershell
aws ssm start-session --target i-... --region us-east-1 --profile lodestar-provisioner
```
(`.agents/skills/lodestar-fleet/SKILL.md:156`)

## 5. Inside the machine

Open **PowerShell** on the remote desktop (Start menu, type `PowerShell`).

UNVERIFIED for all of this section: the machine needs outbound internet (to claude.ai, github.com and GitHub's downloads). The scripts do not set the subnet's routes, and the repository never says whether the subnet has a NAT gateway or a public address. The security group is described as denying all inbound traffic (`Deploy-AwsWindows.ps1:144`); its outbound rules are not described. The machine does need outbound access to reach SSM at all, so some route exists. If a download below fails, stop and tell Claude.

1. **Install Git for Windows.** Claude Code on Windows needs it. In PowerShell:
   ```powershell
   winget install --id Git.Git -e
   ```
   If `winget` is not found (UNVERIFIED on Windows Server), open Edge, go to https://git-scm.com/download/win and run the installer with the default choices. Then close and reopen PowerShell.
2. **Install Claude Code:**
   ```powershell
   irm https://claude.ai/install.ps1 | iex
   ```
   Close and reopen PowerShell so the `claude` command is found.
3. **Get the code:**
   ```powershell
   mkdir C:\src
   cd C:\src
   git clone https://github.com/dvortsis/lodestar-v2
   cd lodestar-v2
   ```
4. **Log in.** Run `claude`. When it starts, type `/login` and follow the prompts. The sign-in link opens in the machine's browser, or you can copy the link to your own PC's browser. Use your own Claude account. Never paste a password or key into a chat. No AWS credentials go on this machine (`docs/decisions/2026-11-12-X-test-vm.md:23`). Type `/exit` to leave.
5. **Start remote control** in `C:\src\lodestar-v2`:
   ```powershell
   claude remote-control --name lodestar-win --permission-mode auto
   ```
   Keep this window open. If you close it, the session ends. The session then shows up in your Claude app as `lodestar-win`. UNVERIFIED: these exact flags are not used anywhere in the repository, so if the command rejects one, run `claude remote-control --help`.
6. Closing the browser tab with the remote desktop does not stop the session. Do not shut Windows down or sign out. Sign-out can stop programs.

Claude may run commands on this machine only. That is your decision of 2026-11-12 (`docs/decisions/2026-11-12-X-test-vm.md:6`). It covers no other machine, and no IAM, Terraform, or account change.

## 6. Delete it

Do this as soon as the tests are done. Do not wait for the 48 hours.

1. Sign in again if needed (section 2, step 3). Then in `infra\akamai\terraform`:
   ```powershell
   .\scripts\Destroy-AwsWindows.ps1
   ```
2. It finds the instance from the state file, or by its `Project=Lodestar` tag (`Destroy-AwsWindows.ps1:55-81`). If it finds more than one, it stops. Run it again with `-InstanceId i-...` (`:73-76`).
3. Type exactly `DESTROY 1 AWS WINDOWS INSTANCE` (`:112`). There is no way to skip this.
4. It waits until AWS reports `terminated`, then prints `Teardown complete` (`:131-147`). If the wait fails it says termination is not verified. Then check the console as below.

### Check nothing is left (AWS console, us-east-1)

1. **EC2, Instances**: the instance shows `Terminated`. (A terminated row stays visible for about an hour. That is normal and costs nothing.)
2. **EC2, Elastic Block Store, Volumes**: no volume tagged `Project=Lodestar`. The root volume should be deleted with the instance. UNVERIFIED: the script does not set delete-on-termination itself, and Windows AMIs normally set it for the root volume. The test plan also expects "its root volume is gone" (`docs/plan/2026-11-08-cloud-test-plan.md:105`). If a volume remains, tell Claude. Do not delete anything you did not create.
3. **EC2, Elastic IPs**: none. The script never creates one.

## 7. The backstop if you forget

A separate AWS function, `lodestar-expiration-guard`, runs every 15 minutes (`docs/operations/lodestar-cloud-assets.md:9`). It terminates a machine at the earlier of its `ExpiresAt` tag and 48 hours after it first started (`docs/plan/2026-11-08-cloud-test-plan.md:18`). It only acts on a `c6a.xlarge` in the Lodestar network and Windows security group with the three `Project`, `Environment` and `ManagedBy` tags (`infra/aws/expiration-guard/deployed/index.py:14`, `:55-57`). The machine this script creates fits that.

Do not rely on it. The repository says that **actual expired-instance deletion has not been proven** (`docs/operations/lodestar-cloud-assets.md:50`). Delete the machine yourself and run the console check above. Even if the guard fails, the cost is bounded at about $0.337 per hour, so a missed week would be about $57, still under your limit but not by much.

## If something fails

- **"Missing or invalid -AmiId" (or Subnet, SecurityGroup, InstanceProfile):** an environment variable is not set in this PowerShell window. Set it again (section 2).
- **"lodestar-provisioner session is not active":** repeat the MFA sign-in.
- **"Deployment halted ... Active Lodestar instance(s) already exist":** an earlier machine exists. Destroy it first (section 6).
- **Dry run fails with an access error:** stop. Do not change IAM or any account setting. Send the full message to Claude.
- **The instance never appears in Fleet Manager:** it needs the SSM instance profile and outbound internet. Stop and send the message to Claude. Do not open any inbound port.
