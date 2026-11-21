# Linux test instances on AWS (X-test-vm)

Linux test lanes run on AWS (since 2026-11-15Z; the earlier Linode flow is historical). A Claude cloud session starts the instance, runs every command through SSM Run Command, and deletes it. Nobody has to connect from a PC, and the owner types nothing per run. The decision is in `docs/decisions/2026-11-12-X-test-vm.md` ("Later decisions (2026-11-15)").

This is the current Linux flow; the Linode flow in `linux-lane.md` is historical. Once the instance is bootstrapped, `linux-lane.md`'s lane steps (sections 0–4) apply with the changes below: numbers only leave the machine, crawls are leads, no fixed sleeps, report before fixing, no PRs and no pushes from the instance.

## One-time setup (the owner)

1. **Update the stack.** In CloudFormation → `lodestar-claude-testvm` → Update → Replace existing template, use `infra/dev-vm/aws-claude-testvm.json` at the commit Claude names. Keep the other parameters, tick the IAM acknowledgement, and submit.
   - **Before updating to the 2026-11-20 template,** delete every instance tagged `Purpose=X-test-vm` (EC2 → Instances, filter by that tag). Instances launched under the old policy have no `LaunchedBy` tag, which can't be added later, so Claude could no longer stop, delete or run commands on them.
   - **Sessions launch nothing until the update shows `UPDATE_COMPLETE`:** the old policy refuses a launch carrying `LaunchedBy`.
   - **`UbuntuImageOwnerId`:** Canonical's public publisher account, which owns the official Ubuntu images; it isn't one of yours. To find it, open Systems Manager → Parameter Store → Public parameters, look up `/aws/service/canonical/ubuntu/server/24.04/stable/current/amd64/hvm/ebs-gp3/ami-id`, then open that image in EC2 → AMIs and copy its **Owner**. It's a parameter so the repository holds no account numbers.
   - **After the 2026-11-20 review fixes** the template no longer takes `InstanceRoleName`: the stack creates its own Windows instance profile (`lodestar-claude-testvm-windows`, the SSM agent policy only), and the user may pass only the stack's two roles. CloudFormation drops the old parameter on update; nothing else to type.
   - The managed policy's description still says "the Windows test instance only". Leave it: changing it makes CloudFormation replace the policy, and the replacement fails because the name is fixed. The policy's statements are what count.
2. **Enter the PIA login.** In Secrets Manager → `lodestar-claude-testvm/pia-login` → Retrieve secret value → Edit, set `username` and `password`, and save.
   - Only the Linux instance role can read it directly. Claude's own user is denied the Secrets Manager API, but it can still reach the secret indirectly: Run Command runs as root on a Linux test instance, and root there can use the instance role. Claude's sessions are trusted, by rule only, never to read, print or copy it.

## What each lane does

All calls go through the AWS connector's `run_script` as `lodestar-claude-testvm`. First check `GetCallerIdentity`: the ARN must end in `user/lodestar-claude-testvm`. If it doesn't, stop. Keep its `UserId` for the `LaunchedBy` tag (in the session only; never write it to the repository).

1. **Count first.** At most four Linux instances and one Windows instance run at once. Count instances with the filter `tag:Purpose=X-test-vm` (the owner's own fleet instances share `Project=Lodestar`, so never count by that), and refuse if four Linux ones are already pending, running, stopping or stopped.
2. **Launch:** (the count of four is a rule for the sessions; the policy doesn't enforce it, and nothing but the per-cloud spend check bounds cost)
   - the image from `ssm:GetParameter` `/aws/service/canonical/ubuntu/server/24.04/stable/current/amd64/hvm/ebs-gp3/ami-id`;
   - `c6a.xlarge` (4 vCPU, 8 GiB);
   - `IamInstanceProfile={Name: lodestar-claude-testvm-linux}`;
   - `MetadataOptions={HttpTokens: required, HttpPutResponseHopLimit: 1, HttpProtocolIpv6: disabled}`, so containers can't reach the metadata service (the policy requires the first two; IAM has no condition key for the IPv6 endpoint, so the third is a rule for the sessions);
   - `/dev/sda1` with `VolumeSize=100`, `VolumeType=gp3`, `Encrypted=true`. A block with `ROLE=main` needs 50 GiB free for its collector after the image build, and the test user can't grow a volume later (`ec2:ModifyVolume` is denied);
   - the network interface with a public IPv4, exactly as `windows.md` Phase 2 step 2 says (the subnet has no NAT; the security group allows no inbound traffic);
   - the tags `Project=Lodestar`, `Environment=Test`, `ManagedBy=Terraform`, `Purpose=X-test-vm`, `LaunchedBy=<the UserId from GetCallerIdentity>`, `ExpiresAt=<now + lane length, at most 24 h>` and `Name=claude-dev-<yyyymmdd>-<role>` on the instance and the volume. The policy lets Claude stop, delete or run commands only on instances tagged `Purpose=X-test-vm` whose `LaunchedBy` is its own `UserId`, so the owner's own fleet instances, which share the other tags, stay out of reach;
   - default tenancy (leave `Placement.Tenancy` unset): the policy refuses dedicated tenancy.
3. **Wait** for `DescribeInstanceInformation` to show it `Online`, with a 15 min deadline.
4. **Bootstrap** with `AWS-RunShellScript` (`executionTimeout` 3600):

   ```bash
   apt-get update -y && apt-get install -y git
   git clone https://github.com/dvortsis/lodestar-v2.git /opt/lodestar-src
   bash /opt/lodestar-src/infra/dev-vm/aws-linux-bootstrap.sh claude-dev-<yyyymmdd>-<role>
   ```

   - It installs what `cloud-init.yaml.tmpl` installs on a Linode, creates the `lodestar` user with `~/lodestar-v2`, and writes `ok` to `/var/lib/devvm-bootstrap.status`.
   - It also makes the metadata service reachable by root only, and leaves `lodestar` with no sudo and outside the `docker` group, so code running as `lodestar` (build scripts, npm packages) can't use the instance role. Run builds as `lodestar` and everything else as root through SSM.
5. **PIA,** as root:

   ```bash
   /opt/lodestar-src/infra/dev-vm/pia-login.sh --aws-secret lodestar-claude-testvm/pia-login
   ```

   - It reads the login into memory with the instance role and writes `/opt/lodestar-run/secrets/wg0.conf`. Nothing else is kept on disk. Run it again for a fresh key.
   - It refuses unless `/opt/lodestar-run` and every folder above it are owned by root and writable by root only.
   - **Never print, copy or read the secret.** Check only the exit code and the script's last line.
   - **Run it again immediately before `start-block.sh` or `leak-test.sh`.** A key registered 17 min before gluetun first used it got no handshake reply; a fresh one worked at once.
   - Exit 0 means the file was written. Exit 5 ("empty username or password") means the owner hasn't filled the secret in yet, and exit 7 that it couldn't be read. Neither prints a `FAILED` line. Exits 10–16 are PIA's own stages, printed as `FAILED (<n>)`.
6. **Run the lane** from `linux-lane.md`, with one change: **everything root runs lives in `/opt/lodestar-run`, never under `~lodestar`.**
   - Copy the fleet stack (`compose.yaml`, `leak-test.sh`, `block-lib.sh`, `start-block.sh`, `watcher.sh`, `outage-watcher.sh`) as root from `/opt/lodestar-src/infra/fleet/steps/vm/` into `/opt/lodestar-run` (root:root, 0755 folders, 0644 or 0755 files), and run it from there as root.
   - **Give `leak-test.sh` its own work folder, never `/opt/lodestar-run`.** The test installs its work folder's `compose.yaml` into the stack, which is `/opt/lodestar-run`. Pointing both at the same folder makes that copy fail, and the test stops at `compose_installed`. The fleet's step bootstrap uses a fresh folder each run too. As root:

     ```bash
     state=$(systemctl show -p ActiveState --value lodestar-leaktest)
     case $state in inactive|failed) ;; *) echo "a leak test is still running ($state)"; exit 1 ;; esac
     systemctl reset-failed lodestar-leaktest 2>/dev/null || true
     rm -f /root/leaktest.log
     rm -rf /opt/lodestar-leaktest
     install -d -o root -g root -m 0755 /opt/lodestar-leaktest
     install -o root -g root -m 0644 /opt/lodestar-run/compose.yaml /opt/lodestar-run/block-lib.sh /opt/lodestar-leaktest/
     systemd-run --unit=lodestar-leaktest -p StandardOutput=truncate:/root/leaktest.log \
       bash /opt/lodestar-run/leak-test.sh /opt/lodestar-leaktest /opt/lodestar-run
     ```

     - The first argument is the work folder; the second is the stack, which holds `secrets/wg0.conf`.
     - The first two lines refuse to start unless an earlier run has fully ended (`inactive` or `failed`, not `deactivating`), so its folder isn't deleted under it. The log is removed, and `truncate:` empties it again on start, so it only ever holds this run.
     - The test takes several minutes, longer than one connector call, so it runs as a background unit. Poll it in later calls with `systemctl is-active lodestar-leaktest; tail -n 3 /root/leaktest.log`.
     - While the first line reads `active` or `deactivating`, the test is still running. Once it reads `inactive` or `failed`, the run is over:
       - a last log line of `ok leak_test` is a pass;
       - `fail leak_test` is a failure, with the failed names above it;
       - a log that ends with neither means the test was killed or hit an error. Read `systemctl status lodestar-leaktest`, and count it as a failure.
   - `lodestar` owns nothing root executes or reads secrets from. Otherwise any code running as `lodestar` (a build script, an npm package) could rewrite a script or `compose.yaml` (say, `network_mode: host`) before root runs it, and reach the instance role.
   - Builds and the lane's own work run as `lodestar`: `sudo -iu lodestar bash -lc '<command>'`. Results go to `~lodestar/results`; root only reads them.
   - `sudo -i` joins a multi-line command into one line, so put anything longer than one line in a script file and run that.
   - **Launch anything that runs `tcpdump` (the leak test, start-block, your own captures) with `systemd-run --wait --pipe --collect`.** See the AppArmor pitfall below.
   - A command over a few minutes runs in `tmux` on the instance (`sudo -iu lodestar tmux new -d -s lane '<command> > ~/results/<name>.log 2>&1'`), and the session polls the log's tail in later calls.
   - The connector gives up after 60 s, so keep each call under 45 s.
7. **Teardown:**
   - **Take the root volume's id first.** Before `TerminateInstances`, read it from `DescribeInstances`: `BlockDeviceMappings`, the entry whose `DeviceName` is `/dev/sda1`, `Ebs.VolumeId`. Keep it in the session only: never write it to a file, a report, a PR or a message (no instance or volume ids in the repository's records).
   - `TerminateInstances`, then wait for `terminated`.
   - **Check that volume by its id:** `DescribeVolumes` with `VolumeIds=[<that id>]`.
     - It fails with `InvalidVolume.NotFound`, or returns the volume as `deleted`: it's gone.
     - It returns it as `deleting`: check again in later calls until it's gone, with a 10 min deadline.
     - Anything else (`available`, `in-use`, or still `deleting` at the deadline) means the volume survived. The test user's policy has no `ec2:DeleteVolume`, so don't try: tell the runner, without the id, and the owner deletes it.
   - **The tag check is a backstop, not the test:** `DescribeVolumes` with `tag:Purpose=X-test-vm` lists every lane's test volumes, and every lane tags them with the same `LaunchedBy`, so another lane's may be there. Only one with this lane's `Name` that is `available` is a leftover of this lane, and it finds nothing if the volume never got its tags. Never filter by `Project`: the owner's fleet volumes share it.
   - Report the instance hours. `c6a.xlarge` Linux costs about $0.15/h, plus about $0.02/h for the volume and address.

## Pitfalls

- **The connector's sign-in** lasts 12 h unless the owner raised it (`docs/decisions/2026-11-12-X-test-vm.md`). A lane that may outlast it sets `ExpiresAt` to its end, so the expiration guard deletes the instance if the session can't.
- **Run Command output is capped at 24,000 characters.** Fetch tails only.
- **`AWS-RunShellScript` runs commands with `/bin/sh`,** so bash syntax fails. Send a script as a quoted heredoc (`cat > /root/lane.sh <<'EOF' … EOF`), then run it with `bash`. The connector refuses `import base64`, so encoding a script doesn't work.
- **A root-owned file can't overwrite a `lodestar` one in `/tmp`** (the sticky bit). Remove the old file first.
- **Git refuses `~lodestar/lodestar-v2` when run as root,** because the owner differs. Pass `-c safe.directory='*'` to git, or run the command as `lodestar`.
- **AppArmor stops SSM commands from signalling `tcpdump`.** The SSM agent is a snap, so its commands carry the label `snap.amazon-ssm-agent.amazon-ssm-agent`, and Ubuntu's enforced `tcpdump` profile refuses signals from it, even `kill -9` (the denials are in `dmesg`). `leak-test.sh` then hangs on its captures until its 900 s limit and leaves them running. A command started with `systemd-run` runs unconfined and isn't affected.
- **Lodestar's port is only reachable through gluetun's network namespace,** not from the host: `nsenter -t "$(docker inspect -f '{{.State.Pid}}' gluetun)" -n curl -s http://127.0.0.1:8090/healthz`.
- **The connector refuses file paths outside `/tmp` inside `run_script` code,** even `/dev/null`.
- **Timings on `c6a.xlarge`:** the bootstrap takes 2–4 min, and building Lodestar's image from `main` about 10–12 min.
- **A lane that needs the VPN can't start until the secret is filled in.** Check with one `pia-login.sh --aws-secret` run before the image build, and say so at once if it exits 5. Then run it again just before the block (step 5).
- **`ExpiresAt` is a backstop, not the plan.** The session deletes its own instance.
- **A 6 h observation:** `SEGMENT_HOURS` takes only 2 or 3, so restart the collector with `--baseline-hours 6`.
- **Routing-table size** is only in the authenticated `/api/status`, not in `/metrics`.
