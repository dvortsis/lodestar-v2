# The 10M benchmark runner: setup

This page is for the owner. It sets up the machine that runs the 10M benchmark (`.github/workflows/bench-10m.yml`), the workflow that checks every §3 size and speed figure on a synthetic store of 10 million torrents. Steps 1–4 are yours: they need the machine or the repository's settings. Step 5 is a test run with the runner.

**What this needs from you:** about two hours, a spare machine (or a virtual machine) you can wipe, and admin access to the repository's settings.

## Why a separate machine, and the risk

GitHub's own runners are too small for 10M (the dataset is about 25 GB, and a build takes hours), and cloud containers varied by about ±50 % between runs. So the benchmark runs on a machine you own, registered as a **self-hosted runner**: a program on your machine that takes jobs from GitHub and runs them.

The risk: **this repository is public, and on a personal (non-organisation) repository GitHub can't restrict a runner to one workflow.** Any workflow file that names the runner's label can send a job to your machine, and that job runs whatever code its branch contains. Four safeguards cover this (your choice, `docs/owner-decisions.md` §2.47):

| Safeguard | What it stops | Step |
|---|---|---|
| (a) An isolated machine that's reset after every job | A hostile job finds nothing to steal, can't reach your network and leaves nothing behind | 1, 2 |
| (b) A ruleset on release lines (`release/v1.0`, `release/v0.5.1`) | Collaborators and other accounts can't create or push the branches the benchmark runs on | 3 |
| (c) The `runner-guard` check | Flags any other workflow that names the label or could reach a self-hosted runner | already in the repo |
| (d) Approval for outside contributors' pull requests | A stranger's fork can't run any workflow until you approve it | 4 |

`runner-guard` **detects** misuse; it doesn't prevent it, because a pushed branch runs its workflows at the same moment the check does. The machine itself is the real barrier, so step 1 matters most.

**Terms used here:**
- **Label:** the name a workflow uses to pick a runner (`runs-on:`). This runner has one label: `lodestar-bench-10m`.
- **Ephemeral runner:** a runner that takes exactly one job and then removes itself from GitHub.
- **Dataset disk:** a disk that keeps the built 10M dataset between runs, so each run doesn't spend hours rebuilding it.

## Before you start

You need:
- **A machine to dedicate:** at least 4 CPU cores, 16 GB of memory, and an **NVMe** SSD with at least **100 GB free** for the dataset disk (the dataset, its working copy and an index rebuild). The §3 budgets are set for 4 cores and NVMe, so a slower disk gives numbers that can't be compared.
- **Ubuntu Server 24.04 LTS** on it, freshly installed.
- **Admin on `dvortsis/lodestar-v2`** for steps 2–4.

**Not this machine:** the Windows soak PC, the experiment host while a crawler run is on, your Unraid server, or any computer you use day to day.

## Step 1 (you): Set up the isolated machine

**Recommended:** a virtual machine on a host you don't otherwise use, or a spare mini PC.

1. Install Ubuntu Server 24.04 LTS. Create one ordinary user, `bench`. Don't copy any SSH keys, password managers, cloud logins or GitHub tokens onto it.
2. Put it on a network of its own: a guest VLAN or guest Wi-Fi, or a firewall rule that lets it reach the internet but **nothing on your home network** (your router, NAS, Unraid server and other PCs).
3. Add the dataset disk and mount it at `/srv/bench`:
   1. List the disks: `lsblk -o NAME,SIZE,MODEL,MOUNTPOINT`. Find the new, empty disk by its **size and model**, and check it has no mount point. It's usually `nvme1n1` on a PC, or `vdb` or `sdb` in a virtual machine.
   2. Format it, using the name you found in place of `nvme1n1`:
      ```sh
      sudo mkfs.ext4 -L bench /dev/nvme1n1
      ```
      > **Warning:** this erases the whole disk. Check the name twice: picking the system disk destroys the machine's install.
   3. Mount it now and at every start:
      ```sh
      sudo mkdir -p /srv/bench
      echo 'LABEL=bench /srv/bench ext4 defaults,nofail 0 2' | sudo tee -a /etc/fstab
      sudo mount -a
      # The container's user (step 2) has uid 1001.
      sudo chown 1001:1001 /srv/bench
      ```
   - **In a virtual machine:** add a second virtual disk of at least 100 GB in your hypervisor first, stored on the host's NVMe drive, then follow the same steps.
   - **Undo:** remove the `LABEL=bench` line from `/etc/fstab` and run `sudo umount /srv/bench`.

   **Result:** `df -h /srv/bench` shows the new disk's size.
4. Install Docker from Ubuntu's packages: `sudo apt install docker.io`. Each job will run in a new container that is deleted afterwards (step 2).

**Result:** from the machine, `curl -sI https://github.com` answers, and `ping` to your router's address or your NAS fails.

**Why it matters:** if a hostile job ever runs, this is everything it can touch. With no credentials and no route to your network, the most it can do is waste the machine's time or tamper with benchmark numbers.

## Step 2 (you): Register the runner as ephemeral

Each job runs in a fresh container that registers itself for **one** job, with only the label and none of GitHub's default labels (`self-hosted`, `linux`, `x64`), so no other workflow can match it by accident.

1. On the machine, create `/srv/bench-runner/Dockerfile`:
   ```dockerfile
   FROM ubuntu:24.04
   ARG RUNNER_VERSION
   ARG RUNNER_SHA256
   # As root: the tools the benchmark builds with, then the runner, checked
   # against its published checksum, and the libraries it needs (ICU among them).
   RUN : "${RUNNER_VERSION:?pass --build-arg RUNNER_VERSION}" \
    && : "${RUNNER_SHA256:?pass --build-arg RUNNER_SHA256}" \
    && apt-get update && apt-get install -y --no-install-recommends \
         build-essential pkg-config curl ca-certificates git python3 \
    && useradd -m -u 1001 runner && mkdir -p /bench /home/runner/actions-runner \
    && cd /home/runner/actions-runner \
    && curl -fsSLo runner.tar.gz \
         "https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/actions-runner-linux-x64-${RUNNER_VERSION}.tar.gz" \
    && echo "${RUNNER_SHA256}  runner.tar.gz" | sha256sum -c - \
    && tar xzf runner.tar.gz && rm runner.tar.gz \
    && ./bin/installdependencies.sh \
    && chown -R runner:runner /home/runner /bench \
    && rm -rf /var/lib/apt/lists/*
   USER runner
   WORKDIR /home/runner/actions-runner
   ```
2. Find the current runner version and its checksum: open `github.com/actions/runner/releases`, take the newest release's version number (for example `2.330.0`, without the `v`), and copy the **SHA-256** listed in its notes for `actions-runner-linux-x64-<version>.tar.gz`.
3. Build the image with both:
   ```sh
   sudo docker build --build-arg RUNNER_VERSION=<version> --build-arg RUNNER_SHA256=<sha-256> \
     -t lodestar-bench-runner /srv/bench-runner
   ```
   **Result:** the build shows `runner.tar.gz: OK` on the way, and ends with `naming to docker.io/library/lodestar-bench-runner` (or `Successfully tagged lodestar-bench-runner:latest` on older Docker). A checksum mismatch stops the build: don't work around it, check the version and checksum again.
   - Rebuild with the newest version about once a month: GitHub refuses runners that fall too far behind.
4. In GitHub, open **Settings → Actions → Runners → New self-hosted runner**, choose **Linux**, and copy the token from the **Configure** box. It's a short-lived registration token: valid for one hour.
5. Start one job's runner, and paste the token when it asks (it isn't shown):
   ```sh
   sudo docker run --rm -it --security-opt no-new-privileges -v /srv/bench:/bench lodestar-bench-runner bash -c '
     read -rsp "Token: " T; echo
     ./config.sh --url https://github.com/dvortsis/lodestar-v2 --token "$T" \
       --ephemeral --no-default-labels --labels lodestar-bench-10m \
       --name bench-$(date +%s) --unattended && unset T && exec ./run.sh'
   ```
   - `--ephemeral` makes the runner take one job and then remove itself from GitHub. `--rm` deletes the container when it ends, so the next job starts clean. Only `/srv/bench` (the dataset disk) survives.
   - Registering writes the runner's own credentials (`.runner`, `.credentials`, `.credentials_rsaparams`) inside the container. Every job step can read them, but they're good only for this one runner and this one job, and `--rm` deletes them with the container.
   - To leave the runner going during a run that lasts hours, press **Ctrl-P** then **Ctrl-Q**. That detaches your terminal without stopping it. Or start it inside `tmux`.
   - Typing the token at the prompt keeps it out of your shell history, out of the job's process list and out of the container's environment. Never put it on the command line or in an `--env-file`: a job can read both (`/proc/1/cmdline`, `/proc/1/environ`) and use the token to register runners of its own.

**Result:** **Settings → Actions → Runners** lists `bench-…` as **Idle**, with the single label `lodestar-bench-10m`. After one job it disappears by itself.

**Safe choice:** start runners by hand like this. Nothing on the machine can register a runner without you, so it holds no GitHub credential at all. The weekly run waits in the queue until you start a runner (GitHub cancels a job that waits more than 24 hours).

**Optional, automatic restarts:** a loop on the machine can fetch a new registration token from GitHub's API before each job. That needs a **fine-grained token** limited to this one repository with **Administration: read and write**, stored on the machine outside the container (`chmod 600`, owned by root, never passed into `docker run`). This is weaker than the safe choice: a job that escaped its container would find a token that can change the repository's settings. If you want it, ask the runner for the script, and use a virtual machine rather than a container so an escape is much harder.

**Undo:** in **Settings → Actions → Runners**, remove the runner. On the machine, `sudo docker rm -f $(sudo docker ps -aq)`, and wipe `/srv/bench` if you're retiring it.

## Step 3 (you): Protect release branches

The benchmark runs on pushes to release lines: branches named like `release/v1.0` or `release/v0.5.1`. The backport branches that sessions push under `release/` (such as `release/rc4-docs` or `release/v052-rc1`) don't match, so they never start it.

**What this does and doesn't stop:** Claude sessions push with your GitHub account, and you are on the bypass list, so the ruleset doesn't stop them. It stops anyone else with write access. Sessions are covered by the other safeguards: the isolated machine, `runner-guard` and the rule that only the bench workflow names the label.

1. Open **Settings → Rules → Rulesets → New ruleset → New branch ruleset**.
2. Name it `release branches`, set **Enforcement status** to **Active**, and leave the bypass list empty except for yourself (**Repository admin**).
3. Under **Target branches**, choose **Add target → Include by pattern** and enter `release/v*.*`. Add a second pattern, `release/v*.*.*`. They need a dot after the version number, so they cover at least the branches the workflow runs on (`release/v1.0`, `release/v0.5.1`), and not `release/v052-rc1`.
4. Tick **Restrict creations**, **Restrict updates**, **Restrict deletions** and **Block force pushes**.
5. Select **Create**.

**Result:** the ruleset lists both patterns as **Active**. A collaborator who tries `git push origin HEAD:release/v9.9` is refused.

**Recommended: make `runner-guard` a required check.** In the `main` ruleset, under **Require status checks to pass**, add `runner-guard`. Otherwise a pull request can merge while the guard is red, because only `ci-ok` is required today. The trade-off: every pull request waits for one more check, about a minute.
- **Its limit:** a pull request runs its own workflows and its own copy of the guard, so the guard is a tripwire, not a wall. Until a planned change makes one CI setting fixed, review these yourself before they merge:
  - the guard's own files (`.github/scripts/runner_guard.py`, its test, `runner-guard.yml`);
  - `ci.yml`'s `changes` or `test` job;
  - `.github/scripts/ci_changes.py`, or any file it imports from `.github/scripts/`;
  - anything the `changes` job runs before `ci_changes.py`, including any new or changed file under `docs/operations/imports/pilots/smoke/`.

**Undo:** set the ruleset's **Enforcement status** to **Disabled**, or delete it.

## Step 4 (you): Require approval for outside contributors

1. Open **Settings → Actions → General**.
2. Under **Approval for running fork pull request workflows from contributors**, choose **Require approval for all external contributors** (older wording: "all outside collaborators").
3. Select **Save**.

**Result:** a pull request from someone else's fork shows **Approve and run workflows** and runs nothing until you approve it.

**The rule: while the bench runner is registered, don't approve any fork pull request's workflow run.** A fork's change to almost any file that CI runs (a test, a script, not only `.github/`) can make a workflow reach the bench machine, and you can't check all of that by eye. To test a contributor's change, copy it to a branch of your own after reading it. As an extra check, a fork pull request that changes anything under `.github/` is never approved.

**Why it matters:** this is the only safeguard against strangers. Without it, a first-time contributor's pull request could run a workflow that names the label.

## Step 5 (together): The first run

1. Start a runner (step 2, points 4 and 5).
2. Open **Actions → Bench 10M → Run workflow**, keep **Branch: main**, tick **Run only the smoke step**, and select **Run workflow**.
3. Wait a few minutes. The smoke step builds a 20,000-torrent dataset in the job's own folder and runs both tools on it.
4. Then start a new runner and run it again **without** the tick. The first full run builds the 10M dataset on `/srv/bench` and takes many hours (allow a day). Later runs reuse it and take a few hours.

**What a pass looks like:**
- The job **bench 10M (dedicated runner)** is green.
- The run's summary has a table with one row per report. For the full run, each verdict is **pass** or **incomplete**. Incomplete is expected for now: some rows stay unavailable until their features land.
- The two smoke rows say **(not gated)**. The smoke `size-report` row shows **fail (not gated)** for the per-torrent size rows: 20,000 torrents are too few for those budgets. That's expected; the smoke step only proves both tools run.
- The artifact **bench-10m-‹commit›** holds the JSON reports (`smoke-size.json`, `smoke-search.json`, and after a full run `size.json`, `size-low.json`, `search.json`, `search-low.json`). They're kept 90 days.

Tell the runner when the smoke run passes: that's the `bench-runner` condition the 1.0 10M gate waits for.

## What a job can leave behind

Only the dataset disk persists. A hostile job could tamper with the dataset there. It holds no secrets, so the damage is limited to wrong benchmark numbers. Each run checks the dataset's key and that its index covers every torrent, and a change to the generator rebuilds it from scratch. If you ever suspect tampering, delete `/srv/bench/dataset-10m` and the next run rebuilds it.

## Troubleshooting

| What you see | What to do |
|---|---|
| The job waits with **Waiting for a runner to pick up this job** | Start a runner (step 2). Check that its label is exactly `lodestar-bench-10m`. |
| `/bench isn't mounted` | Run the container with `-v /srv/bench:/bench`, and check `/srv/bench` is owned by uid 1001, the container's user (`sudo chown 1001:1001 /srv/bench`). |
| `The dataset's key or store schema changed: rebuilding it` | Expected after a generator or schema change. The run takes many hours. |
| The build stops at `curl` with `404` | The version was typed with its leading `v`, or doesn't exist. Use the number alone, for example `2.330.0`. |
| `config.sh` stops with `Couldn't find a valid ICU package` | The image was built without `installdependencies.sh`. Rebuild it from the Dockerfile above. |
| `is not empty and has no dataset.json` | Something else is in `/srv/bench/dataset-10m`, or a machine crash interrupted a build's first step. The job never deletes what it didn't build: check the folder, then delete or move it yourself. |
| The `runner-guard` check fails on a pull request | A workflow names the label or could reach a self-hosted runner. Tell the runner, and don't merge it. |
