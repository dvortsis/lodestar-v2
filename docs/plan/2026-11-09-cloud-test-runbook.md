# Cloud VM test runbook (for an unattended agent)

Written 2026-11-09Z for whoever runs the cloud pilot without the original author: an API-driven Claude instance, or a person. It turns the approved plan, [`2026-11-08-cloud-test-plan.md`](2026-11-08-cloud-test-plan.md), into steps you can run alone. Read that plan's "Context" and "Ground rules" first (5 minutes). Docs only; this file starts nothing.

**Authority, in order:** `docs/owner-decisions.md` (§2.19, §2.23, §2.26, §2.51) → `docs/decisions/2026-11-02-CR-SPEC.md` → `docs/handoff.md` §6 → the plan → this runbook. If this runbook disagrees with one of those, they win; write the disagreement in the journal and carry on with the safer reading.

---

## 0. How you operate

### 0.1 Autonomy: what you may decide

| You may, without asking | You must stop and report (status `BLOCKED`) |
|---|---|
| Retry a failed step up to 3 times with backoff (30 s, 2 min, 10 min) | Anything that weakens child-safety or content-safety settings, or turns off the blocklists or rule packs |
| Use a documented fallback from this runbook | Any crawl that is **not** going through PIA, even for a minute (stop that crawl first, then report) |
| Skip a test whose precondition fails, mark it `SKIPPED` with the reason | Spending beyond the cap in §0.5 |
| Shorten a test to fit the clock (§6), marked `SHORTENED` | Creating cloud resources other than the three pilot VMs and the Phase 0 canaries |
| Re-order tests inside a lane | Any change to an owner-set number (budgets, gates). Record the measurement, don't edit the number |
| Tear down a VM early once its lane is finished | Deleting or altering anything in the AWS or Akamai accounts that you didn't create in this pilot |
| Commit evidence and notes to the evidence branch (§7) | Pushing to `main`, opening PRs, or merging. Never. |

### 0.2 Hard rules (no exceptions)
1. **Every crawling session goes through PIA (a US region, no port forwarding).** Never crawl on a VM before the Phase 0 leak test passed on that VM. If the tunnel drops and the kill switch doesn't stop the traffic, run `docker stop` on Lodestar immediately, then report.
2. **Secrets never go in the journal, evidence, commits, command lines you echo, or chat.** Read them from the environment or secret files named in §1.3. If you must show a command, redact values.
3. **Evidence is aggregates only.** No infohashes, torrent names, peer addresses, logs, VPN exit IPs, VM public IPs or hostnames leave a VM or enter git (`docs/handoff.md` §6). `collect.py` already enforces this for its own output. For anything else you write (Lighthouse reports, bench output), run the scrub in §7.2 first.
4. **No fixed sleeps as a "test".** Wait with a loop and a deadline (§0.4). (Waiting between long runs is fine; the rule is about tests.)
5. **One variable at a time.** Only the treatment toggle differs between arms. Same commit, same image digest, same config, everywhere in a round.
6. **Don't touch content.** Metadata only. Never download, open or seed torrent content.
7. **A run is judged by its own evidence.** Never edit, stitch or re-number evidence. A failed run is kept and labelled; a new run uses a new directory.
8. **UTC only** in everything you write (`Z`). No local time zones.

### 0.3 The journal (your memory)
You may be restarted at any time, so everything you need to resume lives in a file. Keep **`~/pilot/journal.md`** on your control host (and mirror it to the evidence branch every 2 h, §7.1). Append one line per event, never rewrite:

```
2026-11-09T14:02Z | H+00:12 | T0.2 | PASS | akamai canary deleted in 21 min, email received
```
Fields: UTC time, hours since pilot start `H`, test id, `PASS` / `FAIL` / `SKIPPED` / `SHORTENED` / `BLOCKED` / `NOTE`, a one-line reason. On start, **read the journal first**; if it exists, resume at the first test without a `PASS`/`SKIPPED`. Also keep `~/pilot/state.json` with: pilot start time, each VM's creation time and expiry time, the pinned commit and image digest, the current segment per lane.

### 0.4 Waiting and polling
Use a helper like this for every wait (the deadline is mandatory):
```bash
wait_for() { # wait_for <seconds> <command...>  -> 0 when the command succeeds
  local deadline=$((SECONDS+$1)); shift
  until "$@"; do [ $SECONDS -ge $deadline ] && return 1; sleep 15; done; }
```
Check each lane at least every 30 minutes during crawls and every 5 minutes during the stop/start of a segment.

### 0.5 Cost caps and the clock
- Akamai: about $3.82 for 48 h (the brief); AWS Windows c6a.xlarge about $15 for 48 h (accepted by the owner). **Cap: $25 total.** If the AWS console or Akamai API shows spend heading above it, tear down the Windows VM first.
- The watchdogs delete VMs at their expiry (tag read in T0.1). **Hard deadline for copying evidence off each VM = expiry minus 4 h.** Copy evidence every 6 h in the meantime (§7.1) so an unexpected deletion loses little.
- Teardown order: Win, Worker, Main. Confirm deletion with the provider (§7.3).

---

## 1. What you have, and what you must be given

### 1.1 The fleet
| Name | Where | Spec | Role |
|---|---|---|---|
| **Win** | AWS us-east-1, EC2 `c6a.xlarge` | Windows Server 2022+, 4 vCPU, 8 GiB, up to 100 GiB EBS | MSI checks, benchmarks, UX probes. **No crawling** unless T-W7 runs (then PIA only) |
| **Main** | Akamai Newark, `g6-standard-4` | Ubuntu 24.04, 4 vCPU, 8 GiB, 160 GB | Crawler, Docker + gluetun (PIA) |
| **Worker** | Akamai Newark, `g6-nanode-1` | Ubuntu 24.04, 1 vCPU, **1 GiB**, 25 GB | Low-resource crawler, Docker + gluetun (PIA) |

### 1.2 Assumptions (check each in T0.0 and journal the result)
- You have a shell on a control host with `git`, `ssh`, `curl`, `python3` (3.10+), `jq`, and the AWS CLI and Akamai (`linode-cli`) if you were given their credentials.
- You can reach each VM (SSH key for Linux; SSM or RDP/WinRM for Windows).
- **The provisioning code (Terraform and PowerShell) is not in this repository.** If the VMs don't exist when you start, check whether the operator gave you its location. If not, status `BLOCKED: VMs not provisioned and no provisioning code supplied`. Do not improvise infrastructure.
- This repository (`dvortsis/lodestar-v2`) is cloned on the control host and on each VM that builds from source (Win).

### 1.3 Secrets the operator must provide (names are suggestions; use what you were given)
`PIA_USER` and `PIA_PASS` (OpenVPN credentials), cloud API credentials for status and teardown, the SSH key, and a Lodestar admin password you will choose and keep in a `chmod 600` file on each VM (`~/pilot/admin-password`, never in the journal). If a credential is missing, mark the dependent tests `BLOCKED` and continue with the rest.

### 1.4 Repo facts you will use (verified 2026-11-09)
- Image: `ghcr.io/dvortsis/lodestar-v2` (tags: `latest`, `nightly` from main, `0.5.1`, and release tags). The image is distroless: **no shell inside**; use `docker logs`, and `docker exec lodestar /lodestar healthcheck` only if the binary path exists (else the HTTP `/healthz`).
- First-run setup token is in the container log (`docker logs lodestar | grep -i token`); Web UI on 8090; Prometheus `/metrics`, health `/healthz`.
- Treatment switches (env, default off): `LODESTAR_DHT_COORDINATED_ADMISSION` (CR-ADM), `LODESTAR_DHT_UTP` (CR-4), `LODESTAR_DHT_ROUTE_BACKOFF`, `LODESTAR_DHT_TARGET_PREFIXES`; `LODESTAR_DHT_SAFE_RESTART=true` and `LODESTAR_DHT_WITNESS_DIR=<dir outside the data dir>` for multi-segment blocks; `LODESTAR_INTENSITY=low|balanced|aggressive`; `LODESTAR_DHT_PORT`; `LODESTAR_HTTP_BIND`.
- Collector: `docs/soak/tools/linux/collect.py` (Python stdlib only; `--help` was read on 2026-11-09). Analyzer: `docs/soak/tools/analyze_cr0.py`. Windows collector: `docs/soak/tools/cr0-soak.ps1`.
- Bench commands as CI runs them (`.github/workflows/ci.yml`): `cargo run --release -p lodestar-bench --bin size-report -- --torrents 50000 --gate`; `... --bin search-bench -- --torrents 200000 --gate`; `... --bin relevance -- --gate`; `... --example reporting-overhead -- --gate`; `cargo test --release -p lodestar-filter --test props -- --ignored --nocapture budget`.

---

## 2. Phase 0: preflight (H+0 to H+2). Nothing crawls before this passes.

### T0.0 Control-host check
Run: `git --version; python3 --version; jq --version; ssh -V; docker --version` (docker is needed on the VMs, not necessarily here).
Pass: all present. Contingency: install what's missing with the OS package manager; if you have no install rights, mark dependent tests `BLOCKED`.

### T0.1 Inventory and expiry
List the three VMs through the provider APIs. Record each VM's creation time and **expiry tag/value** in `state.json`; compute the hard deadlines in §0.5.
Pass: each VM exists and has an expiry. Contingency: if a VM has no expiry tag, **tell the operator** (`NOTE`) and treat 48 h from creation as the deadline; if no VMs exist, see §1.2.

### T0.2 Watchdog canary (closes "never tested end to end")
1. Create one throwaway nanode and one small EC2 (`t3.nano` or the smallest the account allows), each tagged exactly as the watchdog's eligibility rules require, with expiry **1 h from now**. If you don't know the tag scheme: read it from the existing pilot VMs' tags (T0.1) and copy it.
2. `wait_for 3600+1800 ...` for deletion: the AWS guard runs every 15 min, the Akamai guard every 30 min. Poll the provider API every 5 min.
3. Check the alert email arrived (ask the operator if you cannot read it; mark `NOTE: email unverified`).
4. List what's left behind (Akamai firewall, volumes) and record the manual cleanup commands in the journal.
Pass: both canaries gone within expiry + one interval + 10 min. Fail: delete them yourself, journal `FAIL`, and **continue the pilot with a manual deletion plan** (you will tear down yourself, §7.3, at the earliest time the lanes allow, and never rely on the watchdog). Don't wait for this to finish before T0.3 to T0.5; run them in parallel.

### T0.3 Base setup on Main and Worker
On each Linux VM, as the operator's non-root user:
```bash
sudo apt-get update && sudo apt-get install -y docker.io docker-compose-v2 python3 git chrony jq
sudo systemctl enable --now docker chrony
git clone --depth 50 https://github.com/dvortsis/lodestar-v2 ~/lodestar-v2   # public or via the operator's access
```
Worker only (1 GiB): add swap so a spike doesn't trigger the OOM killer:
```bash
sudo fallocate -l 2G /swapfile && sudo chmod 600 /swapfile && sudo mkswap /swapfile && sudo swapon /swapfile
```
Pin the build: pick the newest `main` commit whose CI is green (`git log origin/main`, check the GitHub check status), record its 40-character hash, and pull that image's digest (`docker pull ghcr.io/dvortsis/lodestar-v2:nightly` and `docker inspect --format '{{index .RepoDigests 0}}' ...`). Use the same digest on both VMs. **Contingency:** if no image matches a green commit, use `0.5.1` and mark every treatment test `SKIPPED: switches not in 0.5.1 image` (check first: run `lodestar --help`-equivalent by `docker run --rm <image> --version`; and grep `crates/lodestar/src/config.rs` at that commit for `LODESTAR_DHT_COORDINATED_ADMISSION`). If the registry is unreachable, build from source (`cargo build --release -p lodestar` after `npm ci && npm run build` in `web/`), which takes much longer; journal it.

### T0.4 PIA tunnel with gluetun (Main and Worker)
Create `~/pilot/compose.yml` (secrets from a `chmod 600` env file, not inline):
```yaml
services:
  gluetun:
    image: qmcgaw/gluetun
    container_name: gluetun
    cap_add: [NET_ADMIN]
    devices: ["/dev/net/tun:/dev/net/tun"]
    env_file: [~/pilot/pia.env]          # VPN_SERVICE_PROVIDER, OPENVPN_USER, OPENVPN_PASSWORD
    environment:
      - VPN_SERVICE_PROVIDER=private internet access
      - VPN_TYPE=openvpn
      - SERVER_REGIONS=US East               # any US region; US regions have no port forwarding
      - FIREWALL_INPUT_PORTS=8090
    ports: ["127.0.0.1:8090:8090"]
    restart: unless-stopped
  lodestar:
    image: ghcr.io/dvortsis/lodestar-v2@sha256:<pinned digest>
    container_name: lodestar
    network_mode: "container:gluetun"
    depends_on: [gluetun]
    stop_grace_period: 120s
    volumes: ["lodestar-data:/data", "/srv/witness:/witness"]
    environment:
      - TZ=Etc/UTC
      - LODESTAR_INTENSITY=balanced        # Worker: low
      - LODESTAR_DHT_SAFE_RESTART=true
      - LODESTAR_DHT_WITNESS_DIR=/witness
      # treatment switch is added per segment (§4)
volumes: {lodestar-data: {}}
```
Check the gluetun wiki for the current variable names for PIA if gluetun refuses to start (`docker logs gluetun`); variable names change. **Do not set `FIREWALL_VPN_INPUT_PORTS`**: no inbound forwarding exists in US regions.
`mkdir -p /srv/witness` on the host first. Don't start `lodestar` yet.

### T0.5 Leak test (the gate for any crawl)
Run on each VM, with gluetun up and Lodestar **stopped**:
1. `docker run --rm --network container:gluetun curlimages/curl -s https://ifconfig.me` → the address must **differ** from the VM's own public address (`curl -s https://ifconfig.me` on the host). Do not write either address anywhere.
2. Kill switch: with gluetun running, take its tunnel interface down: `docker exec gluetun sh -c 'ip link set tun0 down'` (use `wg0` if the tunnel is WireGuard; `docker exec gluetun ip -br link` lists it). Then run `docker run --rm --network container:gluetun curlimages/curl -s -m 10 https://ifconfig.me`. It must **fail or time out**. Then `docker restart gluetun`, wait for healthy, and repeat step 1.
3. Lodestar can't bypass the tunnel: it has no network of its own (`docker inspect lodestar --format '{{.HostConfig.NetworkMode}}'` must print `container:gluetun` or the container's id).
4. Host SSH must still work from the operator's allow-listed address (you're connected, so it does).
Pass: step 1 differs, step 2 fails closed and recovers, step 3 as stated. **Fail = no crawl on that VM.** Contingency: try another US region once (`SERVER_REGIONS`); if it fails twice, the VM does the non-crawl tests only (benchmarks, UX probes), and you journal `BLOCKED`.
On Windows (only needed for T-W7): install the PIA client or a WireGuard profile with the kill switch on; the same two checks (the address differs; with the tunnel disabled, `Invoke-WebRequest https://ifconfig.me -TimeoutSec 10` fails). If you can't get a Windows kill switch, **skip T-W7**.

---

## 3. Lane M (Main) and Lane K (Worker): the crawl blocks

### 3.1 The design (read once)
- **Treatment: CR-ADM** (`LODESTAR_DHT_COORDINATED_ADMISSION=true`). **Control: all switches off.**
- A **block** is four segments on one fresh data volume, with the safe-restart witness kept across them (CR-SPEC rule 2): **Main runs ABBA, Worker runs BAAB** (A = control, B = CR-ADM). With identical segment lengths, at any hour one VM runs each arm, so the hour's effect shows up as a concurrent control.
- **Segment lengths** (CR-SPEC rule 1, A/B kind): the first segment is a fresh directory (6 h suppression + 60 min warm-up + **6 h** window = 13 h); every later segment is a resumed restart (60 min warm-up + 6 h window = 7 h). A block = 13 + 7 + 7 + 7 = **34 h**, ending at about H+37 if segments start at H+3 (leave switch-over gaps ≤ 10 min).
- Results are **exploratory** (ground rule 1 of the plan). Label every report so.

### 3.2 T-M1 Smoke (first 30 minutes after start, Main and Worker)
```bash
cd ~/pilot && docker compose up -d && docker compose ps
wait_for 300 curl -fs http://127.0.0.1:8090/healthz
docker logs lodestar 2>&1 | grep -i "setup token" | head -1   # do not paste the token anywhere
```
Complete first-run setup in the UI (via an SSH tunnel, §4.1) using the admin password; keep the wizard defaults (the safe choices), and leave all rule packs and the child-safety core as they come. Do **not** import anything.
Pass: `/healthz` ok; the dashboard shows nodes within 10 min (`curl -s localhost:8090/metrics | grep -c dht_`); no restarts in `docker compose ps`.
Fail: `docker logs lodestar | tail -50`; common causes: gluetun not healthy yet (wait up to 5 min), DNS (restart gluetun then Lodestar), the volume (`docker volume inspect`). Retry 3 times, then `BLOCKED`.

### 3.3 T-M2 Segment procedure (repeat for each of the four segments)
Segment 1 uses a fresh volume. For segments 2 to 4, **keep the volume and the witness directory** and change only the treatment variable.
1. Choose the arm: per the schedule (Main ABBA, Worker BAAB). For arm B add `LODESTAR_DHT_COORDINATED_ADMISSION=true` to the `lodestar` environment; for arm A remove it.
2. Start Lodestar (segment 1) or `docker compose up -d lodestar` (later segments; this recreates it with the new env).
3. **Immediately** start a new collector for this segment, in a new empty directory (`~/evidence/seg<N>`), reading `--help` first if anything fails:
```bash
mkdir -p ~/evidence && python3 ~/lodestar-v2/docs/soak/tools/linux/collect.py \
  --mode docker --container lodestar --url http://127.0.0.1:8090 \
  --root ~/evidence/seg<N> --commit <40-hex pinned commit> \
  --run-kind ab --treatment adm --baseline-hours 6 --intensity balanced \
  --spec-commit <commit that contains docs/decisions/2026-11-02-CR-SPEC.md> \
  --password-file ~/pilot/admin-password --storage-path /var/lib/docker \
  --checklist auto_update=excluded --checklist backup=excluded \
  --next-run auto_update=<UTC date 30 days out> --next-run backup=<UTC date 30 days out> \
  --require-gluetun --stop > ~/evidence/seg<N>.out 2>&1 &
```
   Notes: `--require-gluetun` is accepted with the A/B kind only if the collector allows it; if it refuses, drop it and journal why (the tunnel is checked separately in §3.4). Use `--intensity low` on Worker. Check how the collector labels the control arm: `grep -n "treatment" docs/soak/tools/linux/README.md docs/soak/tools/linux/collect.py | head -20` and `docs/decisions/2026-11-02-CR-SPEC.md`. If arm A must omit `--treatment`, omit it. If the collector rejects a flag combination, add `--rehearsal` (it marks the run as non-acceptance, which is what this pilot is anyway) and journal it.
   The collector needs the admin password only on the loopback or private URL it's given; `chmod 600 ~/pilot/admin-password`.
4. Pass-through checks every 30 min: `tail -3 ~/evidence/seg<N>.out`, `jq .status ~/evidence/seg<N>/status.json`, disk (`df -h`), `docker stats --no-stream lodestar`, tunnel (`docker logs --since 30m gluetun | grep -ci "error\|restart"`).
5. At the end of the window (the collector finishes, or at the segment's planned end), end it cleanly: `touch ~/evidence/seg<N>/stop.request`, wait for `result.json` (`wait_for 300 test -f ~/evidence/seg<N>/result.json`), then analyze:
```bash
python3 ~/lodestar-v2/docs/soak/tools/analyze_cr0.py ~/evidence/seg<N> > ~/evidence/seg<N>/analysis.md
python3 ~/lodestar-v2/docs/soak/tools/analyze_cr0.py ~/evidence/seg<N> --format json --output ~/evidence/seg<N>/analysis.json
```
   (The analyzer refuses an output file inside the evidence directory if it exists already, and an exit status of 2 means unreadable evidence. A status 0 is **not** a pass.)
6. Switch to the next segment within **10 minutes**: stop = `docker compose stop lodestar` (grace 120 s; wait for exit), recreate with the new arm, start the next collector (step 3).

### 3.4 What to judge in each segment (record in the journal)
From `analysis.md`: valid / invalid / void (CR-SPEC rule 5/6.5), window mean of `searchable_new_total{origin="network"}` per minute, worst hour, p95 `discovery_to_searchable_seconds` and coverage (below 98 % = "not measured"), fetch success rate, `dht_peer_rejection_reasons_total` and eviction counts, peak RSS (Worker: the limit is the 1 GiB machine), routing-table size (outbound-only with no port forward, so expect lower), tunnel health (any `gluetun` restart is an event: journal it and mark the affected hour).
Reference numbers (CR-0, Windows, home IP, **not comparable**; for orientation only): about 12.3 stored/min mean over 24 h, about 4.3 % fetch success, 98.9 % of pool evictions before an accepted lookup. The v0.6 gate (100/min, no hour under 70, p95 ≤ 10 min) is **not** judged on cloud runs.

### 3.5 Block verdict (after segment 4)
Per VM: mean per-arm window rate (A segments vs B segments), the paired differences, and the concurrent-control view: in each segment, Main's arm vs Worker's opposite arm (labels only; the VMs differ in size, so the ratio is a **lead**, never a result). Write `~/evidence/block-summary.md`: the table, validity of each segment, and one of `LEAD_POSITIVE` / `LEAD_NEGATIVE` / `INCONCLUSIVE` (with the reason). Don't call anything "significant"; two blocks of four segments can't prove it.

### 3.6 Worker specifics (1 GiB)
- Expect memory pressure. If `docker stats` shows Lodestar over about 600 MiB or the swap used above 512 MiB for 30 min, journal `NOTE` and keep going; the collector stops the run itself at 1 GiB anon for five samples: that is a **finding** ("does not fit 1 GiB at low"), not a failure of yours. Keep the evidence, note the time, and start a fresh block only if more than 12 h remain.
- The architecture describes a low-resource mode (trigram field and file index off). **Check whether a setting for it exists** (`grep -n -i "low.resource\|trigram" crates/lodestar/src/config.rs`; the Settings screen). If it doesn't exist at the pinned commit, journal `NOTE: low-resource mode not implemented; ran intensity low` and don't invent a setting.
- Don't run the Lighthouse or search load against Worker; only against Main.

### 3.7 Contingencies for lanes M and K
| Event | Action |
|---|---|
| gluetun unhealthy or restarted | `docker compose restart lodestar` after gluetun is healthy (Lodestar stays attached to the old network namespace otherwise). Journal the time. The segment's affected hour is excluded; if more than 2 restarts in a segment, mark the segment `invalid` and move on |
| Collector crashed | Never restart into the same directory. Start the next collector in a new directory and journal the gap; the segment becomes `invalid` for the window it missed (CR-SPEC rule 6.6) |
| Lodestar exited or OOM-killed | `docker logs lodestar --tail 100` (a redacted summary into the journal), `docker compose up -d lodestar`; this is an identity change: close the segment, record, continue with the next arm's slot |
| Disk below 20 GB | Stop; copy evidence; do not delete the data volume unless a block is lost anyway |
| A segment runs long at the clock (§6) | Shorten the later segments (`SHORTENED`): minimum viable arm = 60 min warm-up + 3 h window, `--rehearsal` required; never shorten the first segment's suppression |
| No new torrents after the warm-up + 2 h | Check tunnel (T0.5 step 1), `curl localhost:8090/metrics | grep dht_routing_nodes`, a US-region switch (new gluetun region, recreate both containers, new block) once; else `BLOCKED` on that VM |
| VM about to expire (expiry − 4 h) | Stop the collector cleanly, analyze, copy evidence off (§7.1) |

---

## 4. Lane L (live-ingest UX) from Win against Main

Start only after Main's segment 1 passed T-M1 and has at least 5,000 stored torrents (`curl -s localhost:8090/metrics | grep -i stored`); otherwise the data set is too small to mean anything (journal `SHORTENED` and run on what exists).

### 4.1 Reaching Main without exposing it
Main's UI is not opened to the internet. From Win (PowerShell with OpenSSH client):
```powershell
ssh -N -L 18090:127.0.0.1:8090 <user>@<main-address> -i <key>      # keep running in a background job
```
Then test against `http://127.0.0.1:18090`. (This adds an encrypted hop; the numbers are real-network, and not LAN. Journal that.) Contingency: if SSH forwarding fails, run the probes **on Main** against `127.0.0.1:8090` and mark `NOTE: no cross-VM latency`.

### T-L1 Lighthouse (mobile and desktop) against live data
On Win, with Node 22 and Chrome/Edge available (`winget install OpenJS.NodeJS.LTS Google.Chrome` if needed):
```powershell
cd lodestar-v2\web; npm ci
npx lighthouse http://127.0.0.1:18090/search --preset=perf --only-categories=performance --chrome-flags="--headless=new --no-sandbox" --output=json --output-path=..\..\pilot\lh-mobile-1.json   # default form factor is mobile (4G simulated)
npx lighthouse http://127.0.0.1:18090/search --preset=desktop --only-categories=performance --chrome-flags="--headless=new --no-sandbox" --output=json --output-path=..\..\pilot\lh-desktop-1.json
```
Run each **three times** (change the file suffix) and take the median. Targets (`web/lighthouserc*.json`): mobile LCP ≤ 2500 ms, desktop LCP ≤ 1000 ms, TBT ≤ 100 ms, CLS ≤ 0.02, total bytes ≤ 102,400. Note the app needs the admin session; if `/search` redirects to a login or setup page, sign in once in a Chrome profile and pass `--chrome-flags="--user-data-dir=<profile>"` or record `NOTE: lighthouse measured the login page`; both are acceptable if labelled. The SSH tunnel inflates LCP by one round trip; also run the same Lighthouse **on Main** against loopback (only if Chrome is installed there; otherwise skip) and report both.
Contingency: if Lighthouse cannot launch Chrome on Windows Server, install Chrome stable; if that fails, use Playwright's bundled Chromium (`npx playwright install chromium` is allowed on **Win** here since there's no preinstalled browser; the "never run it" rule in `web/e2e/README.md` applies only to the cloud coding sessions).

### T-L2 API latency while the crawl is ingesting
From Win through the tunnel, authenticated (use the session cookie from a normal login, never printing it), run **200 sequential** requests per query class and compute p50/p95/p99. The API's search, suggest and facets endpoints are described by the OpenAPI document (`GET /api/v1/openapi.json` or `crates/lodestar/src/bin/openapi.rs` output; read it, don't guess paths). Query classes: typeahead prefix (`ubu`), keyword (`ubuntu`), a filter query (`size:>2GB res:2160p`), and the architecture §17 file query (`file:(ext:mkv size:>2GB) res:2160p seeders:>=1`). Targets are the §3 table; the 10M numbers don't apply to a store of tens of thousands, so report them as "observed at N torrents" and **compare p95 at the start and at the end of the crawl** (the trend is the finding). Do it at about H+10, H+20 and H+30.
Contingency: if the API requires a token you don't have, mark `SKIPPED: auth` and rely on T-L1.

### T-L3 Browser behaviour on a real, crawl-grown store (manual probe, scripted where possible)
Using Playwright from `web/` against the tunnel URL, capture (a) time from typing to first results, (b) scroll a long results list for 10 s and record dropped frames via the Chrome performance trace, (c) open a torrent with the most files (sort or filter by file count) and confirm the file tree appears without a long task over 200 ms (`PerformanceObserver` for `longtask`). Write a small script in `~/pilot/ux-probe.mjs`; don't commit it unless it works end to end. The repository's e2e `fixture` project is **not** usable here: it needs a fixture backend and its data directory (`E2E_FIXTURE_DIR`), not a live server.
Pass/fail is judged against §10.1 (INP < 100 ms, 60 fps scroll). If you can't produce numbers, `SKIPPED` with the reason: partial numbers labelled as such are better than guesses.

### T-L4 Restart drill on Main (once, at about H+20)
This is the segment switch (§3.3 step 6) observed on purpose: record that suppression is skipped after the restart (metric `dht_sample_suppression_seconds` reads 0 at the first scrape, reason `resumed`), that the store's torrent count is unchanged, and that the UI session survives. Pass: reason `resumed` and no data loss. Fail (suppression restarts for 6 h): journal `FAIL` with the witness directory contents listing (names only), and continue; the block's later segments become `invalid`, so run the remaining arms as non-comparable and say so.

---

## 5. Lane W (Windows, no crawling)

### T-W0 Access and tools
Connect (RDP or SSM). Install, in this order, journaling each: Git, Node 22 LTS, Python 3.12, Rust via rustup (`rust-toolchain.toml` in the repo pins the version), Visual Studio Build Tools 2022 with the "Desktop development with C++" workload (needed for the MSVC linker; about 10 to 20 min), WiX Toolset 3 (only if building the MSI). Clone the repo and check out the **pinned commit**. If the C++ build tools won't install, status `BLOCKED` for T-W2 to T-W3 and T-W4 builds; T-W1 still works with the released MSI.

### T-W1 MSI install, service and uninstall on a clean Server
Download the released `lodestar-<version>-x86_64.msi` (the v0.5.1 release) and, if a newer build exists as a CI artifact, that too as the "upgrade" MSI; verify each against the release page's `SHA256SUMS` (`Get-FileHash <file> -Algorithm SHA256`). In an **elevated** PowerShell:
```powershell
cd lodestar-v2
.\packaging\windows\smoke.ps1 -Msi C:\pilot\lodestar-0.5.1-x86_64.msi -UpgradeMsi C:\pilot\lodestar-<higher>.msi -Zip C:\pilot\lodestar-0.5.1-x86_64-windows-portable.zip
```
The script checks install, files, service, firewall rules (6881/udp, private networks only), `/healthz` on its own port, restart, upgrade, uninstall, and `REMOVE_DATA=1`. Also run `packaging\windows\test-secure-datadir.ps1` and `test-service-sid.ps1`. Observe and journal separately, as a human would see them: whether SmartScreen/Defender flags the unsigned MSI (owner §2.23 says unsigned is expected), and any Windows Server-specific firewall difference.
Pass: smoke exits 0. Fail: copy the last 60 lines of its output into the journal (it may include paths but no secrets), and the failing step; don't edit the script. Contingency: if there's no upgrade MSI, run with `-Msi` and `-Zip` only and mark `SHORTENED`.

### T-W2 Benchmarks at spec scale (Win is dedicated vCPU; nothing else may run on it)
Run from `lodestar-v2` after `cargo build --release -p lodestar-bench` finishes (first build 20 to 40 min; do it during T-W1). **Do not run any other work while a bench runs**, and close the SSH tunnel jobs and Lighthouse runs (they conflict). Record CPU steal impossible on Windows; instead record `Get-Counter '\Processor(_Total)\% Processor Time'` before the run to confirm the box is idle (< 5 %).
```powershell
cargo run --release -p lodestar-bench --bin size-report -- --torrents 50000 --gate
cargo run --release -p lodestar-bench --bin search-bench -- --torrents 200000 --gate     # same as CI: compare with CI's result
cargo run --release -p lodestar-bench --bin relevance -- --gate
cargo test --release -p lodestar-filter --test props -- --ignored --nocapture budget
cargo run --release -p lodestar-bench --example reporting-overhead -- --gate           # about 17 min
cargo run --release -p lodestar-bench --bin search-bench -- --torrents 1000000          # no --gate: a measurement
cargo run --release -p lodestar-bench --bin search-bench -- --torrents 10000000 --dir D:\bench10m   # 10M: needs about 25 GB; run once, no --gate
```
Use `--help` on each bin to confirm flags first (bench bins `--dir`, `--seed`, `--only`, `--no-hot` exist; flags were read from source, not run). Repeat the 200k run **three times** and report the median p95 per query class. For 10M, one run is enough; mark it "single run". Report each result against the §3 p95 table, labelling the hardware "EBS gp3, 4 vCPU, not NVMe, Windows". The filter budget is calibrated to an EPYC 7763 (`docs/decisions/2026-10-23-F-perf.md`); a failure on this CPU is a **finding** to record with the measured µs/torrent, not a regression, unless the same test fails on Main after the crawl (T-M3).
Contingency: out of disk for 10M → reduce to 5M and mark `SHORTENED`; a gate fails → record the numbers, rerun once to see whether it is noise, then continue.

### T-W3 Playwright suite on Windows (the repo's own tests)
```powershell
cd web; npm ci; npm run build
npx playwright install chromium      # allowed on this Windows VM only
npx playwright test --project=core --ignore-snapshots
npx playwright test --project=keyboard-only --project=reduced-motion --ignore-snapshots
```
Snapshots are baselined on CI's Linux container, so pass `--ignore-snapshots` (that's what the README says for non-CI browsers). Pass: no failures; each failure is a real finding (Windows rendering, path or line-ending bug): copy the test name and the first error line to the journal.

### T-W4 Windows flake hunt
Run the Rust tests that wait on conditions in a loop, because Windows is CI's slowest platform: `cargo test --workspace` once for a baseline (about 15 to 40 min); then run any test that fails twice more in isolation (`cargo test -p <crate> <name>`); report the test names, failure messages and how often each failed out of three. Don't fix; the runner decides what to do. Stop after 90 minutes total.

### T-M3 (after Main's crawl ends, optional) Bench on Main
Only after T-M segments are all analyzed and `docker compose down` is done, run `size-report` and the 200k `search-bench` on Main (Linux) to compare Linux and Windows on similar vCPUs. Needs Rust there; skip it if more than 50 minutes of Main's life remain unmapped.

### T-W7 (optional, only if everything above finished and at least 8 h of Win's life remain) Windows crawl arm
Only through PIA (§2 T0.5, Windows part). Use the pinned binary built in T-W0; run `docs/soak/tools/cr0-soak.ps1` as documented there (`-Exe`, `-Root`, `-Commit`, `-Ui embedded|stub`, `-BuildRecord`; it needs a build record JSON from the build tooling and refuses short baselines: `-BaselineHours 24` minimum). If you can't satisfy its build-record requirement or the window doesn't fit, `SKIPPED: no valid 24 h window`. Don't run a crawl that isn't collected.

---

## 6. Clock (hours since pilot start; keep `state.json` in step)

| Window | Main | Worker | Win |
|---|---|---|---|
| H+0 to H+2 | T0.0 to T0.5 | T0.0 to T0.5 | T0.1; T-W0 installs |
| H+2 to H+3 | T-M1 smoke; segment 1 starts (Main = A) | T-M1 smoke; segment 1 starts (Worker = B) | T-W1 MSI smoke; start the bench build |
| H+3 to H+16 | Segment 1 (13 h) | Segment 1 (13 h) | T-W2 benches (finish by H+8); T-W3 |
| H+8 to H+16 | | | T-L1 first run when Main has 5,000+ stored; T-L2 at about H+10 |
| H+16 | Switch to segment 2 | Switch to segment 2 | |
| H+16 to H+23 | Segment 2 (7 h) | Segment 2 (7 h) | T-W4 flake hunt; T-L2 at about H+20 |
| H+23 to H+30 | Segment 3 (7 h) | Segment 3 | T-L1 second run; T-L2 at about H+30 |
| H+30 to H+37 | Segment 4 (7 h) | Segment 4 | T-L3 browser probe; T-W7 is skipped (no 24 h window fits); otherwise idle |
| H+37 to H+40 | Final analysis, T-M3 | Final analysis | Final Lighthouse run on the largest store; copy results; tear down Win |
| H+40 to H+44 | Copy evidence, tear down Worker, then Main | | |
| H+44 to H+48 | Spare buffer. Nothing is meant to run. | | |

**Decision clock:** if any segment is more than 2 h late at a checkpoint, shorten the later segments (§3.7); if the pilot is more than 6 h behind at H+24, run segments 2 and 3 only as a "AB" pair and skip segment 4; at the deadline in §0.5, finish whatever you have: partial evidence labelled as partial is the correct result.

---

## 7. Evidence, scrubbing and teardown

### 7.1 Copy-off (every 6 h, and at the deadline)
Evidence lives in `~/evidence` on each Linux VM and `C:\pilot` on Win. Every 6 h, `rsync -a` (Linux) or `scp` (Win) to the control host's `~/pilot/collected/<vm>/`. Commit **only** what is listed here to a **new branch** `cloud-pilot/2026-11` (never `main`): the collector evidence directories (`run.json`, `samples.jsonl`, `metrics/`, `status.json`, `disk.jsonl`, `result.json`, `analysis.*`), `block-summary.md`, the benchmark outputs, the Lighthouse JSON after scrubbing, and the journal. Path in the repo: `docs/soak/evidence/2026-11-cloud-pilot/<vm>/`. If you cannot push (no repo access), archive to the operator's location and journal where; **never leave evidence only on a VM**.

### 7.2 Scrub before any commit
```bash
cd ~/pilot/collected
grep -rIEn '([0-9]{1,3}\.){3}[0-9]{1,3}|\b[0-9a-f]{40}\b|\b[0-9a-f]{64}\b|PIA|password|token|secret|ifconfig' . \
  | grep -v 'run.json' | head -50
```
The commit hash and image digest in `run.json` and the journal are fine (that's provenance); public IPs, VPN exit addresses, hostnames, tokens and credentials are not. Remove or replace anything the grep finds in files you wrote (Lighthouse JSON includes the URL and request headers: strip them with `jq 'del(.requestedUrl,.finalUrl,.finalDisplayedUrl,.configSettings)'`; the artifacts hold a lot, so keep only the `audits` metrics you report). Re-run until clean.

### 7.3 Teardown (verify, don't assume)
Order: Win, Worker, Main. For each VM: confirm evidence copied (compare file counts and `sha256sum`), then delete via the provider API or let the watchdog do it **only** if T0.2 passed. Afterwards list resources and confirm none remain: AWS (`aws ec2 describe-instances --region us-east-1 --filters Name=instance-state-name,Values=pending,running,stopped` and orphaned volumes `describe-volumes --filters Name=status,Values=available`), Akamai (`linode-cli linodes list`, `linode-cli firewalls list`, `linode-cli volumes list`). Delete the pilot's orphaned firewalls and volumes yourself (only those you or the pilot created, identified by the pilot tag). Journal each deletion.

### 7.4 The final report (`docs/soak/evidence/2026-11-cloud-pilot/REPORT.md`)
Sections: (1) what ran, what was skipped or shortened, and why; (2) Phase 0 results (watchdog, leak test); (3) block summaries per VM and the verdict word from §3.5; (4) UX numbers vs budgets (Lighthouse, API latency, browser probe), each labelled with its conditions; (5) benchmark table vs the §3 targets with hardware labels; (6) Windows findings (MSI, Playwright, flakes); (7) anomalies and costs; (8) **recommendations**, each tied to a number, in the form "slice or follow-up, evidence, confidence". Open with a status line: `COMPLETE`, `PARTIAL (reason)` or `BLOCKED (reason)`. Never overstate: cloud runs are leads (plan ground rule 1).
Send the REPORT's last status line to the operator, if you have a way to; otherwise it is the last line of the journal.

---

## 8. Quick contingency index

| Symptom | Section |
|---|---|
| A VM doesn't exist | §1.2: `BLOCKED`, ask for provisioning |
| Watchdog canary not deleted | T0.2: manual deletion plan; don't rely on watchdog |
| Leak test fails | T0.5: no crawl on that VM |
| gluetun/PIA won't connect | T0.4/T0.5: check wiki for variable names; try another US region once |
| No torrents appearing | §3.7 |
| OOM on Worker | §3.6: a finding, not a failure |
| Collector refuses flags | §3.3 step 3: read `--help`; use `--rehearsal` |
| Out of time | §6 decision clock |
| Out of disk | §3.7; reduce bench size |
| Windows toolchain won't install | T-W0: `BLOCKED` for builds; T-W1 still runs |
| Can't push evidence | §7.1: archive elsewhere, journal location |
| Anything touching safety/security/owner numbers | §0.1: stop and report |
