# The 10M benchmark runner: setup

This page is for the owner. It sets up the machine that runs the 10M benchmark (`.github/workflows/bench-10m.yml`), the workflow that checks every §3 size and speed figure on a synthetic store of 10 million torrents. Steps 1, 2 and 4 are yours. Step 3 is yours too unless you ask the runner to prepare it. Step 5 is a test run together.

**What this needs from you:** about two hours, a spare machine (or a virtual machine) you can wipe, and admin access to the repository's settings.

## Why a separate machine, and the risk

GitHub's own runners are too small for 10M (the dataset is about 25 GB, and a build takes hours), and cloud containers varied by about ±50 % between runs. So the benchmark runs on a machine you own, registered as a **self-hosted runner**: a program on your machine that takes jobs from GitHub and runs them.

The risk: **this repository is public, and on a personal (non-organisation) repository GitHub can't restrict a runner to one workflow.** Any workflow file that names the runner's label can send a job to your machine, and that job runs whatever code its branch contains. Four safeguards cover this (your choice, `docs/owner-decisions.md` §2.47):

| Safeguard | What it stops | Step |
|---|---|---|
| (a) An isolated machine that's reset after every job | A hostile job finds nothing to steal, can't reach your network and leaves nothing behind | 1, 2 |
| (b) A ruleset on `release/*` | Only you can create or push release branches, which the benchmark runs on | 3 |
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
   ```sh
   sudo mkdir -p /srv/bench
   # Find the disk with lsblk, format it once (this erases it), then add it to /etc/fstab.
   # The container's user (step 2) has uid 1001.
   sudo chown 1001:1001 /srv/bench
   ```
4. Install Docker from Ubuntu's packages: `sudo apt install docker.io`. Each job will run in a new container that is deleted afterwards (step 2).

**Result:** from the machine, `curl -sI https://github.com` answers, and `ping` to your router's address or your NAS fails.

**Why it matters:** if a hostile job ever runs, this is everything it can touch. With no credentials and no route to your network, the most it can do is waste the machine's time or tamper with benchmark numbers.

## Step 2 (you): Register the runner as ephemeral

Each job runs in a fresh container that registers itself for **one** job, with only the label and none of GitHub's default labels (`self-hosted`, `linux`, `x64`), so no other workflow can match it by accident.

1. On the machine, create `/srv/bench-runner/Dockerfile`:
   ```dockerfile
   FROM ubuntu:24.04
   RUN apt-get update && apt-get install -y --no-install-recommends \
         build-essential pkg-config curl ca-certificates git python3 \
       && rm -rf /var/lib/apt/lists/*
   RUN useradd -m runner && mkdir /bench && chown runner /bench
   USER runner
   WORKDIR /home/runner
   # Paste the download, checksum and extract lines from GitHub's
   # "New self-hosted runner" page (Linux, x64) here, each as a RUN line.
   ```
   Build it: `sudo docker build -t lodestar-bench-runner /srv/bench-runner`.
2. In GitHub, open **Settings → Actions → Runners → New self-hosted runner**, choose **Linux**, and copy the token from the **Configure** box. The token is valid for one hour.
3. Start one job's runner:
   ```sh
   sudo docker run --rm -it -v /srv/bench:/bench lodestar-bench-runner bash -c '
     ./config.sh --url https://github.com/dvortsis/lodestar-v2 --token PASTE_TOKEN \
       --ephemeral --no-default-labels --labels lodestar-bench-10m \
       --name bench-$(date +%s) --unattended && ./run.sh'
   ```
   `--rm` deletes the container when the job ends, so the next job starts clean. Only `/srv/bench` (the dataset disk) survives.

**Result:** **Settings → Actions → Runners** lists `bench-…` as **Idle**, with the single label `lodestar-bench-10m`. After one job it disappears by itself.

**Safe choice:** start runners by hand like this. Nothing on the machine can register a runner without you, so it holds no GitHub credential at all. The weekly run waits in the queue until you start a runner (GitHub cancels a job that waits more than 24 hours).

**Optional, automatic restarts:** a loop on the machine can fetch a new registration token from GitHub's API before each job. That needs a **fine-grained token** limited to this one repository with **Administration: read and write**, stored on the machine outside the container (`chmod 600`, owned by root, never passed into `docker run`). This is weaker than the safe choice: a job that escaped its container would find a token that can change the repository's settings. If you want it, ask the runner for the script, and use a virtual machine rather than a container so an escape is much harder.

**Undo:** in **Settings → Actions → Runners**, remove the runner. On the machine, `sudo docker rm -f $(sudo docker ps -aq)`, and wipe `/srv/bench` if you're retiring it.

## Step 3: Protect release branches

The benchmark runs on pushes to `release/*`, so only you should be able to create or push those branches.

1. Open **Settings → Rules → Rulesets → New ruleset → New branch ruleset**.
2. Name it `release branches`, set **Enforcement status** to **Active**, and leave the bypass list empty except for yourself (**Repository admin**).
3. Under **Target branches**, choose **Add target → Include by pattern** and enter `release/*`.
4. Tick **Restrict creations**, **Restrict updates**, **Restrict deletions** and **Block force pushes**.
5. Select **Create**.

**Result:** a session or collaborator that tries `git push origin HEAD:release/test` is refused.

**Optional, a hard block on pull requests:** add `runner-guard` to the required status checks in the `main` ruleset. A pull request that names the label then can't merge. The trade-off: every pull request waits for one more check, about a minute.

**Undo:** set the ruleset's **Enforcement status** to **Disabled**, or delete it.

## Step 4 (you): Require approval for outside contributors

1. Open **Settings → Actions → General**.
2. Under **Approval for running fork pull request workflows from contributors**, choose **Require approval for all external contributors** (older wording: "all outside collaborators").
3. Select **Save**.

**Result:** a pull request from someone else's fork shows **Approve and run workflows** and runs nothing until you approve it. Never approve a fork's run that changes anything under `.github/`.

**Why it matters:** this is the only safeguard against strangers. Without it, a first-time contributor's pull request could run a workflow that names the label.

## Step 5 (together): The first run

1. Start a runner (step 2, point 3).
2. Open **Actions → Bench 10M → Run workflow**, keep **Branch: main**, tick **Run only the smoke step**, and select **Run workflow**.
3. Wait a few minutes. The smoke step builds a 20,000-torrent dataset in the job's own folder and runs both tools on it.
4. Then start a new runner and run it again **without** the tick. The first full run builds the 10M dataset on `/srv/bench` and takes many hours (allow a day). Later runs reuse it and take a few hours.

**What a pass looks like:**
- The job **bench 10M (dedicated runner)** is green.
- The run's summary has a table with one row per report, each verdict **pass** or **incomplete**. Incomplete is expected for now: some rows stay unavailable until their features land.
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
| `is not empty and has no dataset.json` | Something else is in `/srv/bench/dataset-10m`. The job never deletes what it didn't build: move it away yourself. |
| The `runner-guard` check fails on a pull request | A workflow names the label or could reach a self-hosted runner. Tell the runner, and don't merge it. |
