# The Windows test VM (X-test-vm)

One AWS Windows Server instance that Claude creates, tests on and deletes. It runs the Windows lane of `docs/plan/2026-11-08-cloud-test-plan.md` (Lane W) and the runner's Windows checks. Decisions and the owner's words: `docs/decisions/2026-11-12-X-test-vm.md`.

| | |
|---|---|
| Machine | `c6a.xlarge` (4 vCPU on 2 cores, 8 GiB), us-east-1, the Lodestar subnet and security group (no inbound), encrypted 100 GiB root, IMDSv2 |
| Image | the latest `Windows_Server-2022-English-Full-Base` from AWS's public parameter `/aws/service/ami-windows-latest/` |
| Cost | about $0.337/h with the licence: about $16.20 for 48 h, plus about $0.60 of disk. At most $100 in total for this cloud (owner) |
| Backstop | `lodestar-expiration-guard` terminates it at `min(ExpiresAt, start + 48 h)`. Its deletion path is unproven, so Claude deletes it itself and checks |
| Access | SSM Run Command (`AWS-RunPowerShellScript`) only. No RDP, no key pair, no inbound port |

## Two phases, because a session reads its connectors when it starts

### Phase 1: setup, on the owner's one-time admin sign-in
The owner has signed the AWS MCP connector in as their admin. A new session started after that does only this, then stops:
1. **Find the IDs; never write them to the repo.**
   - VPC and security group: `DescribeStacks` on `lodestar-expiration-guard`, parameters `LodestarVpcId` and `WindowsSecurityGroupId`.
   - Subnet: `DescribeSubnets` in that VPC. If there's more than one, ask the owner.
   - Instance profile and its role: `ListInstanceProfiles`. Expect one Lodestar profile; check that the role has `AmazonSSMManagedInstanceCore`.
2. **Check the way out to the internet.** Claude Code, GitHub, rustup and Node all need it.
   - Look at the subnet's route table (its explicit association, else the VPC's main one) for `0.0.0.0/0` to an `igw-` or a `nat-`.
   - With an `igw-`, the instance also needs a public IPv4: check the subnet's `MapPublicIpOnLaunch`.
   - Check the security group's egress rules and `DescribeVpcEndpoints`.
   - **If there's no way out, stop and tell the owner.** A NAT gateway costs about $0.045/h plus data, and it's an infrastructure change the owner decides.
3. **Deploy `infra/dev-vm/aws-claude-testvm.json`** as stack `lodestar-claude-testvm` (`CreateStack`, `CAPABILITY_NAMED_IAM`), with the three IDs as parameters. Wait for `CREATE_COMPLETE`.
4. **Nothing else.** Never call `secretsmanager:GetSecretValue`. Don't touch the watchdog or guard stacks, Terraform state, OIDC or any other IAM.
5. **Tell the owner to swap the connector** (below) and say they're done with the admin sign-in.

**The owner's swap (about 3 minutes):**
1. In the AWS console, go to IAM → Users → `lodestar-claude-testvm` → Security credentials.
2. **Enable console access** with a custom password. Keep it in your password manager.
3. **Assign MFA device** → Authenticator app.
4. **Sign out of the AWS console,** or use a private window.
5. Go to claude.ai → Settings → Connectors → AWS MCP → disconnect, then connect. Sign in as **IAM user**: your account ID, `lodestar-claude-testvm`, the password, then the MFA code.

### Phase 2: create, test, delete, as `lodestar-claude-testvm`
A new session, started after the swap. First check `GetCallerIdentity`: the ARN must end in `user/lodestar-claude-testvm`. If it doesn't, stop.
1. **One at a time:** refuse if any `Project=Lodestar` instance is pending, running, stopping or stopped.
2. **Launch**, matching `infra/akamai/terraform/scripts/Deploy-AwsWindows.ps1` (lines 155–164):
   - the image, subnet, security group and instance profile;
   - `MetadataOptions.HttpTokens=required`;
   - `/dev/sda1` with `VolumeSize=100, Encrypted=true`;
   - tags `Project=Lodestar`, `Environment=Test`, `ManagedBy=Terraform`, `ExpiresAt=<now + lane length, at most 48 h, %Y-%m-%dT%H:%M:%SZ>` on both the instance and the volume.
   - Record the instance ID locally only.
3. **Wait** for `DescribeInstanceInformation` to show it `Online`, with a 15 min deadline.
4. **Bootstrap with Run Command** (`executionTimeout` up to 7200 s per step):
   - Git for Windows, rustup with stable MSVC, the Visual Studio 2022 Build Tools (`Microsoft.VisualStudio.Workload.VCTools`, `--quiet --wait`), and Node 22;
   - a clone of `https://github.com/dvortsis/lodestar-v2` to `C:\src\lodestar-v2`.
   - Record `rustc -V`, `node -v` and the free disk space.
5. **The runner's Windows checks, in order:**
   - **a.** `windows_service::a_portable_restart_serves_again_on_the_same_port`: loop it on `main`, then on #745's head (b20bb8b3 or later). Report pass and fail counts.
   - **b.** MSI smoke of the current v0.5.2 candidate (`packaging/windows/smoke.ps1`): install, start, stop, clean restart, stored > 0. The MSI comes from that commit's CI, with its SHA-256 recorded.
   - **c.** The update helper's upgrade and rollback, and its Application event-log lines (PK-win-upd, X-event-log-notice).
   - **d.** Service stop timing during an update, once #734 has merged.
   - **No crawl on Windows** (plan Lane W, item 5). Numbers only leave the VM: no torrent names, IDs or IPs.
6. **Report each finding to the runner before writing any fix.**
7. **Teardown:**
   - `TerminateInstances`, then wait for `terminated`.
   - Check that `DescribeVolumes` with `tag:Project=Lodestar` shows nothing left.
   - Check `ce:GetCostAndUsage` for the day's EC2 cost.
   - Tell the runner and the owner.

## When the testing is over
The owner can remove Claude's AWS access in two steps: disconnect the AWS MCP connector, then delete the `lodestar-claude-testvm` stack in CloudFormation, which removes the user.
