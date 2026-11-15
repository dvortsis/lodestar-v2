# Linux test instances on AWS (X-test-vm)

Linux test lanes can run on AWS as well as on Linode. A Claude cloud session starts the instance, runs every command through SSM Run Command, and deletes it. Nobody has to connect from a PC, and the owner types nothing per run. The decision is in `docs/decisions/2026-11-12-X-test-vm.md` ("Later decisions (2026-11-15)").

Once the instance is bootstrapped, `linux-lane.md` applies unchanged: numbers only leave the machine, crawls are leads, no fixed sleeps, report before fixing, no PRs and no pushes from the instance.

## One-time setup (the owner)

1. **Update the stack.** In CloudFormation → `lodestar-claude-testvm` → Update → Replace existing template, use `infra/dev-vm/aws-claude-testvm.json` at the commit Claude names. Keep the parameters, tick the IAM acknowledgement, and submit.
2. **Enter the PIA login.** In Secrets Manager → `lodestar-claude-testvm/pia-login` → Retrieve secret value → Edit, set `username` and `password`, and save.
   - Only the Linux instance role can read it. Claude's own user is denied all of Secrets Manager.

## What each lane does

All calls go through the AWS connector's `run_script` as `lodestar-claude-testvm`. First check `GetCallerIdentity`: the ARN must end in `user/lodestar-claude-testvm`. If it doesn't, stop.

1. **Count first.** At most four Linux instances and one Windows instance run at once. Refuse if four `Project=Lodestar` Linux instances are already pending, running, stopping or stopped.
2. **Launch:**
   - the image from `ssm:GetParameter` `/aws/service/canonical/ubuntu/server/24.04/stable/current/amd64/hvm/ebs-gp3/ami-id`;
   - `c6a.xlarge` (4 vCPU, 8 GiB);
   - `IamInstanceProfile={Name: lodestar-claude-testvm-linux}`;
   - `MetadataOptions={HttpTokens: required, HttpPutResponseHopLimit: 1}`, so containers can't reach the metadata service;
   - `/dev/sda1` with `VolumeSize=100`, `VolumeType=gp3`, `Encrypted=true`. A block with `ROLE=main` needs 50 GiB free for its collector after the image build, and the test user can't grow a volume later (`ec2:ModifyVolume` is denied);
   - the network interface with a public IPv4, exactly as `windows.md` Phase 2 step 2 says (the subnet has no NAT; the security group allows no inbound traffic);
   - the tags `Project=Lodestar`, `Environment=Test`, `ManagedBy=Terraform`, `ExpiresAt=<now + lane length, at most 24 h>` and `Name=claude-dev-<yyyymmdd>-<role>` on the instance and the volume.
3. **Wait** for `DescribeInstanceInformation` to show it `Online`, with a 15 min deadline.
4. **Bootstrap** with `AWS-RunShellScript` (`executionTimeout` 3600):

   ```bash
   apt-get update -y && apt-get install -y git
   git clone https://github.com/dvortsis/lodestar-v2.git /opt/lodestar-src
   bash /opt/lodestar-src/infra/dev-vm/aws-linux-bootstrap.sh claude-dev-<yyyymmdd>-<role>
   ```

   - It installs what `cloud-init.yaml.tmpl` installs on a Linode, creates the `lodestar` user with `~/lodestar-v2`, and writes `ok` to `/var/lib/devvm-bootstrap.status`.
   - It also makes the metadata service reachable by root only, so the `lodestar` user and the containers can't use the instance role.
5. **PIA,** as root:

   ```bash
   /opt/lodestar-src/infra/dev-vm/pia-login.sh --aws-secret lodestar-claude-testvm/pia-login
   ```

   - It reads the login into memory with the instance role and writes `/home/lodestar/lodestar/secrets/wg0.conf`. Nothing is kept on disk. Run it again for a fresh key.
   - **Never print, copy or read the secret.** Check only the exit code and the script's last line.
   - **Run it again immediately before `start-block.sh` or `leak-test.sh`.** A key registered 17 min before gluetun first used it got no handshake reply; a fresh one worked at once.
   - Exit 0 means the file was written. Exit 5 ("empty username or password") means the owner hasn't filled the secret in yet, and exit 7 that it couldn't be read. Neither prints a `FAILED` line. Exits 10–16 are PIA's own stages, printed as `FAILED (<n>)`.
6. **Run the lane** from `linux-lane.md`, as `lodestar`: `sudo -iu lodestar bash -lc '<command>'`.
   - `sudo -i` joins a multi-line command into one line, so put anything longer than one line in a script file and run that.
   - **Launch anything that runs `tcpdump` (the leak test, start-block, your own captures) with `systemd-run --wait --pipe --collect`.** See the AppArmor pitfall below.
   - A command over a few minutes runs in `tmux` on the instance (`sudo -iu lodestar tmux new -d -s lane '<command> > ~/results/<name>.log 2>&1'`), and the session polls the log's tail in later calls.
   - The connector gives up after 60 s, so keep each call under 45 s.
7. **Teardown:**
   - `TerminateInstances`, then wait for `terminated`.
   - `DescribeVolumes` with `tag:Project=Lodestar` must show none of the lane's volumes.
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
