# The Windows test VM (X-test-vm)

> **Current flow:** the stack is deployed, so a session starts at **Phase 2** below, signed in as `lodestar-claude-testvm`. **Phase 1 is historical** (the first deployment, 2026-11-12Z): updating the stack is now the owner's step in [`linux-aws.md`](linux-aws.md) ("One-time setup"). The runner's check list in Phase 2 step 5 records the first runs; take the current checks from your brief.

One AWS Windows Server instance that Claude creates, tests on and deletes. It runs the Windows lane of `docs/plan/2026-11-08-cloud-test-plan.md` (Lane W) and the runner's Windows checks. Decisions and the owner's words: `docs/decisions/2026-11-12-X-test-vm.md`.

| | |
|---|---|
| Machine | `c6a.xlarge` (4 vCPU on 2 cores, 8 GiB), us-east-1, the Lodestar subnet and security group (no inbound), encrypted 100 GiB root, IMDSv2 |
| Image | the latest `Windows_Server-2022-English-Full-Base` from AWS's public parameter `/aws/service/ami-windows-latest/` |
| Cost | about $0.337/h with the licence: about $16.20 for 48 h, plus about $0.60 of disk. At most $100 in total for this cloud over the whole testing period, and tell the owner at about $90 (owner, 2026-11-12T16:58Z, raised 2026-11-15) |
| Backstop | `lodestar-expiration-guard` terminates it at `min(ExpiresAt, start + 48 h)`. Its deletion path is unproven, so Claude deletes it itself and checks |
| Access | SSM Run Command (`AWS-RunPowerShellScript`) only. No RDP, no key pair, no inbound port |

**The AWS connector's sign-in lasts 12 h by default.** AWS Sign-In's *sign-in session duration* is set on the IAM user, up to 90 days. The connector keeps refreshing one-hour tokens for that long, and no long-lived key is created.
- **To lengthen it:** the owner sets it on `lodestar-claude-testvm` in the IAM console, then disconnects and reconnects the connector. The new length applies only to sign-ins made after the change.
- **To cut it short:** disconnect the connector, or revoke the session with `aws signin batch-revoke-sessions`.
- **Without the longer setting:** plan each Windows lane to finish, teardown included, within one sign-in. After it expires, every call fails until the owner reconnects.

**The AWS connector's tool gives up on a script after 60 s.** Keep each call short: send a create, then check its status in separate calls. Never loop inside one call for longer than about 45 s.

## Two phases, because a session reads its connectors when it starts

### Phase 1 (historical): setup, on the owner's one-time admin sign-in
Kept for the record; don't run it. The owner had signed the AWS MCP connector in as their admin. A new session started after that does only this, then stops:
1. **Find the IDs; never write them to the repo.**
   - VPC, subnet, security group and instance profile: the **outputs** of the `lodestar-foundation` stack. The guard stack's live parameters hold only `NotificationEmail`.
   - Subnet: confirm with `DescribeSubnets` in that VPC. If there's more than one, ask the owner.
   - (At the time, the owner's instance profile was a parameter. Since the 2026-11-20 review fixes the stack defines its own Windows and Linux profiles, and the owner's role can't be passed.)
2. **Check the way out to the internet.** Claude Code, GitHub, rustup and Node all need it.
   - Look at the subnet's route table (its explicit association, else the VPC's main one) for `0.0.0.0/0` to an `igw-` or a `nat-`.
   - With an `igw-`, the instance also needs a public IPv4: check the subnet's `MapPublicIpOnLaunch`.
   - Check the security group's egress rules and `DescribeVpcEndpoints`.
   - **If there's no way out, stop and tell the owner.** A NAT gateway costs about $0.045/h plus data, and it's an infrastructure change the owner decides.
3. **Deploy `infra/dev-vm/aws-claude-testvm.json`** as stack `lodestar-claude-testvm` (`CreateStack`, `CAPABILITY_NAMED_IAM`), with the IDs as parameters. Wait for `CREATE_COMPLETE`.
4. **Nothing else.** Never call `secretsmanager:GetSecretValue`. Don't touch the watchdog or guard stacks, Terraform state, OIDC or any other IAM.
5. **Tell the owner to swap the connector** (below) and say they're done with the admin sign-in.

**The owner's swap (about 3 minutes):**
1. In the AWS console, go to IAM → Users → `lodestar-claude-testvm` → Security credentials.
2. **Enable console access** with a custom password. Keep it in your password manager.
3. **Assign MFA device** → Authenticator app.
4. **Sign out of the AWS console,** or use a private window.
5. Go to claude.ai → Settings → Connectors → AWS MCP → disconnect, then connect. Sign in as **IAM user**: your account ID, `lodestar-claude-testvm`, the password, then the MFA code.

### Phase 2: create, test, delete, as `lodestar-claude-testvm`
A new session, started after the swap. First check `GetCallerIdentity`: the ARN must end in `user/lodestar-claude-testvm`. If it doesn't, stop. Keep its `UserId` for the `LaunchedBy` tag (in the session only; never write it to the repository).
1. **One Windows instance at a time:** refuse if a Windows instance tagged `Purpose=X-test-vm` is pending, running, stopping or stopped. Count with the filters `tag:Purpose=X-test-vm` and `platform=windows`; up to four Linux test instances may run alongside it.
2. **Launch**, following `infra/akamai/terraform/scripts/Deploy-AwsWindows.ps1` (lines 155–164) except for the profile and tags below:
   - the image, and the instance profile **`lodestar-claude-testvm-windows`** (the stack's Windows role: the SSM agent policy only). Never the Linux profile (`docs/decisions/2026-11-12-X-test-vm.md`, 2026-11-20 review fixes);
   - **a public IPv4 address:** `NetworkInterfaces=[{DeviceIndex: 0, SubnetId, Groups: [SG], AssociatePublicIpAddress: true}]`, with no top-level `SubnetId` or `SecurityGroupIds`.
     - **Why:** the subnet routes `0.0.0.0/0` to an internet gateway but has `MapPublicIpOnLaunch=false`, and there's no NAT gateway and no VPC endpoint. Without a public address, the instance can't reach SSM or the internet.
     - It's still unreachable from outside, because the security group allows no inbound traffic. The address costs about $0.005/h.
     - `Deploy-AwsWindows.ps1` doesn't do this yet; that's a follow-up for the runner.
   - `MetadataOptions={HttpTokens: required, HttpPutResponseHopLimit: 1, HttpProtocolIpv6: disabled}` (the policy refuses a launch without the first two; IAM has no condition key for the IPv6 endpoint, so the third is a rule for the sessions);
   - `/dev/sda1` with `VolumeSize=100, Encrypted=true`;
   - tags `Project=Lodestar`, `Environment=Test`, `ManagedBy=Terraform`, `Purpose=X-test-vm`, `LaunchedBy=<the UserId from GetCallerIdentity>`, `ExpiresAt=<now + lane length, at most 48 h, %Y-%m-%dT%H:%M:%SZ>` on both the instance and the volume. The policy lets this user stop, delete or run commands only on instances whose `LaunchedBy` is its own `UserId`, and refuses a launch without it.
   - Default tenancy (leave `Placement.Tenancy` unset): the policy refuses dedicated tenancy.
   - Record the instance ID locally only.
3. **Wait** for `DescribeInstanceInformation` to show it `Online`, with a 15 min deadline. Then take the root volume's id for teardown (step 7): from `DescribeInstances`, the `BlockDeviceMappings` entry whose `DeviceName` is the instance's `RootDeviceName` (`/dev/sda1` for this launch), `Ebs.VolumeId`. Keep it in the session only: never write it to a file, a report, a PR or a message (the runner's rule for test lanes: no instance or volume ids in records).
4. **Bootstrap with Run Command** (`executionTimeout` up to 7200 s per step):
   - Git for Windows, the Visual Studio 2022 Build Tools (`Microsoft.VisualStudio.Workload.VCTools`, `--quiet --wait`), rustup with stable MSVC, and Node 22;
   - a clone of `https://github.com/dvortsis/lodestar-v2` to `C:\src\lodestar-v2`.
   - Record `rustc -V`, `node -v` and the free disk space.
5. **The runner's Windows checks, in order:**
   - **a.** `windows_service::a_portable_restart_serves_again_on_the_same_port`: loop it on `main`, then on #745's head (b20bb8b3 or later). Report pass and fail counts.
   - **b.** MSI smoke of the current v0.5.2 candidate (`packaging/windows/smoke.ps1`): install, start, stop, clean restart, stored > 0. The MSI comes from that commit's CI, with its SHA-256 recorded.
   - **c.** The update helper's upgrade and rollback, and its Application event-log lines (PK-win-upd, X-event-log-notice).
   - **d.** Service stop timing during an update, once #734 has merged.
   - **No crawl on Windows** (plan Lane W, item 5). Numbers only leave the VM: no torrent names, IDs or IPs.
   - **Bootstrap pitfalls found on the first runs:**
     - install the Visual Studio Build Tools before rustup;
     - Run Command doesn't see tools installed earlier in the session until `PATH` is reloaded from the registry;
     - when picking the Node download from SHASUMS, index the matched line as a list: a single match is a string, and indexing it picks one character;
     - run `smoke.ps1` as administrator under PowerShell 7;
     - Edge won't start under SYSTEM, which Run Command uses, so browser suites use Playwright's own Chromium;
     - time with Python's `perf_counter`, never `monotonic`, which ticks every 15.6 ms on Windows;
     - building Lodestar's tests on `c6a.xlarge` (8 GiB) can run out of memory: two `rustc` processes of about 5.6 GiB each with the default 1.9 GiB pagefile hung Windows and dropped SSM (2026-11-20Z). Set a 16 GiB pagefile and reboot first, build with `cargo test -j1`, and time nothing while it builds (stops took 18–40 s instead of 0.4 s).
6. **Report each finding to the runner before writing any fix.**
7. **Teardown:**
   - **Have the root volume's id** from step 3. If the session doesn't (it resumed after its sign-in expired), read it now, before `TerminateInstances`, as step 3 says. If the instance is already gone (the expiration guard ended it), its mappings are empty: skip the check by id, rely on the tag check below, and tell the runner the check by id couldn't run.
   - `TerminateInstances`, then wait for `terminated`, with a 10 min deadline. If it isn't `terminated` by then, tell the runner.
   - **Check that volume by its id:** `DescribeVolumes` with `VolumeIds=[<that id>]`.
     - It fails with `InvalidVolume.NotFound` (in `run_script`, a raised `ClientError`: catch it and read `e.response['Error']['Code']`), or returns the volume as `deleted`: it's gone.
     - It returns it as `deleting`: check again in later calls until it's gone, with a 10 min deadline.
     - `available`, `in-use` or any other state (`error`, say) means the volume survived, and still `deleting` at the deadline means its deletion didn't finish. Either way, the test user's policy has no `ec2:DeleteVolume`, so don't try: tell the runner, without the id, and the owner deletes it.
   - **The tag check is a backstop, not the test:** `DescribeVolumes` with `tag:Purpose=X-test-vm` lists every lane's test volumes (Linux lanes' too, all with the same `LaunchedBy`), and it finds nothing if the volume never got its tags. An `available` one there is a leftover, not necessarily this lane's: tell the runner, without its id. Never filter by `Project`: the owner's fleet volumes share it.
   - Check `ce:GetCostAndUsage` for the day's EC2 cost.
   - Tell the runner and the owner.

## When the testing is over
The owner can remove Claude's AWS access in two steps: disconnect the AWS MCP connector, then delete the `lodestar-claude-testvm` stack in CloudFormation, which removes the user.
