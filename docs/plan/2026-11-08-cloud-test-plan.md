# Cloud VM test plan: concurrent lanes on the 48 h pilot fleet, and the cloud screen

Planning note, 2026-11-08Z, for the build runner. Docs only; it starts no VM and changes no code.

**Status: approved, revised 2026-11-08Z.** The first version's open questions were answered that morning (see Decisions). A review the same day checked it against:
- the deployed infrastructure (`dvortsis/lodestar-v2#625`, which supersedes `#623`);
- CR-SPEC and the repo's tools.

The owner approved the two changes to their answers that came out of it: uTP moves to the next session, and a standing cloud screen is added. Every change is listed in [Review changes](#review-changes-2026-11-08z) and recorded in `docs/decisions/2026-11-08-P-cloud-test-plan.md`.

**Authority:** `docs/owner-decisions.md` §2.19, §2.23, §2.25, §2.26, §2.35, §2.51; `docs/decisions/2026-11-02-CR-SPEC.md`; `docs/build-plan.md` §0.5; `docs/handoff.md` §6; `docs/operations.md` §4.3; architecture §3, §10.1, §17, §19.8.

## Context
We now have a disposable three-VM fleet:

| VM | What it is | Access | Expiry backstop |
|---|---|---|---|
| **Win** | AWS c6a.xlarge, us-east-1: 4 vCPU on **2 physical cores** (SMT), 8 GiB, Windows Server, 50 GiB encrypted EBS root (type not set by the script; check with `describe-volumes`) | SSM only; the security group allows no inbound traffic | `lodestar-expiration-guard`, every 15 min: terminates at `min(ExpiresAt tag, launch + 48 h)`. Scope: c6a.xlarge in the Lodestar VPC and Windows security group, tagged `Project=Lodestar`, `Environment=Test`, `ManagedBy=Terraform` |
| **Main** | Linode g6-standard-4, us-east: 4 shared vCPU, 8 GiB, 160 GB, Ubuntu 24.04 | SSH from one admin `/32` only; all other inbound dropped | `lodestar-akamai-watchdog`, every 30 min: deletes at **48 h after creation**, whatever the tags say. Scope: label `lodestar-*`, tags `lodestar` and `test`, region `us-east`, type g6-standard-4 or g6-nanode-1, created after 2026-11-08T00:00Z. At most 4 deletions a run. It leaves firewalls, volumes and reserved IPs behind |
| **Worker** | Linode g6-nanode-1, us-east: 1 vCPU, 1 GiB, 25 GB, Ubuntu 24.04 | as Main | as Main |

Each VM has its own public IP; Main and Worker crawl only through PIA. No VM is running yet.

**The safety net is unproven.** Both watchdogs are deployed, but neither has deleted a real instance yet. `#625` adds an IAM condition and a `TOKEN_REQUIRED` flag; neither changes the rules above, and neither is deployed until a reviewed stack update.

**Where the infrastructure lives:** the provisioning scripts and templates are in `#625` (`infra/`, `docs/operations/lodestar-cloud-assets.md`).

The goal is the owner's standing one: **better performance and UX** (`docs/owner-decisions.md` §2.26). Until now every live crawl ran on one Windows host with one public IP, one run at a time (`docs/decisions/2026-11-02-CR-SPEC.md`). That is slow. A 24 h window after the 6 h suppression and 1 h warm-up takes a day and a half per data point, and there were no Linux, low-RAM or second-IP numbers. Three machines with three IPs give parallel lanes, a real Linux/Docker target, a 1 GiB target, and a real Windows target.

## Prerequisites (all before hour 0)
1. **Infrastructure:** `#625` is merged, and the Akamai Terraform state is reconciled. Its own README forbids `terraform apply` before that, because a fresh state can recreate machines.
2. **Win can reach Main.**
   - **The gap:** Main's firewall allows TCP/22 only from one admin `/32` (`ssh_admin_ipv4_cidr` takes exactly one). Lanes L3 and W3b need Win's egress `/32` allowed on TCP/22 as well.
   - **The fix:** a list variable, as a follow-up slice to `#625`. It widens Main's SSH ingress by exactly one `/32` and goes through that slice's review.
   - **Without it:** the cross-VM UX steps are skipped, not opened up some other way.
3. **Disk on Win:** the 50 GiB root has to hold Windows, MSVC Build Tools, the Rust toolchain, Node, a browser and the release builds. The 10M corpus alone was about 19 GB on disk (`docs/decisions/2026-11-05-X-search-files-4.md`). The 10M step runs only if the same follow-up slice makes the root volume size a parameter and it's set higher. Otherwise the step is skipped, and the report says so. Check free space before each step.
4. **Two PIA WireGuard configurations,** for two different exit servers in one US region: one for Main, one for Worker.
   - Two Lodestar crawlers behind one exit IP are "another crawler on the public IP" (CR-SPEC 6.5). Neither exit may be one the Unraid experiment host uses during the session.
   - Other PIA customers on the same exit are not controlled. That's one more reason cloud numbers are leads.
   - Configurations are copied host to host over SSH only, never through Git, Terraform, cloud-init or chat. Local copies are deleted at teardown.
5. **A frozen build:** one source commit, the Docker image pinned by digest, and the MSI from the same commit's CI. Record the commit, the digest and the binaries' SHA-256. The build must have `LODESTAR_DHT_COORDINATED_ADMISSION` (it's on `main`).
6. **A named operator and booked times.** The owner books the 48 h and names who watches the run. The booking lists the UTC times of the three arm switches (about hours 16, 23 and 30; the switches themselves are scripted, see 0e). If the admin IP changes, Linode's LISH console is the out-of-band way in.

## Ground rules (fit the existing contracts)
1. **Cloud crawl numbers are leads, not gate evidence.**
   - **Why:** CR-SPEC fixes the acceptance run to one node, one public IP, a fresh data directory and the Balanced preset, with nothing else on the host or the IP (rule 6.5). Cloud runs differ: a VPN exit IP, shared CPU, a different machine size. They screen candidates. The CR-COMB acceptance run stays on the experiment host (Decision 1).
   - **The experiment host** is the owner's Unraid box, with the DHT port not forwarded and UPnP off (owner §2.51). Build-plan §0.5 requires an identical network setup every run.
   - **Every crawling session goes through PIA, in a US region, which has no port forwarding.** So the DHT port isn't forwarded, the same as on the Unraid host. The cloud VMs still differ in IP, CPU and location, so cloud yields are never compared directly with Unraid arms.
   - **The use is** to screen candidates (see [The cloud screen](#the-cloud-screen-standing-stage)) and to run the arms the Unraid box can't: Linux, 1 GiB, a second IP.
2. **Use the existing tools and run them as their READMEs say:**
   - Crawls: `docs/soak/tools/linux/collect.py` with `--run-kind ab`, judged by `docs/soak/tools/analyze_cr.py`. `analyze_cr0.py` is the older baseline analyzer and is used only for the CR-0 comparison.
   - Windows: `packaging/windows/smoke.ps1`.
   - Benchmarks: the `bench/` bins `search-bench`, `size-report` and `relevance`, the `reporting-overhead` example (`cargo run --release -p lodestar-bench --example reporting-overhead -- --gate`), and the filter budget test (`cargo test --release -p lodestar-filter --test props -- --ignored --nocapture budget`).
   - Browser: `web/lighthouserc*.json` and `web/e2e`.
3. **One variable changes at a time,** and the same build runs on every VM. Main (4 vCPU) and Worker (1 vCPU) are not like-for-like, so each VM's results are analysed on their own and never pooled.
4. **Measurements never overlap.**
   - Benchmarks never run on a crawling VM.
   - On Win, one measuring job runs at a time: a benchmark, the Lighthouse/e2e runs, the flake loops.
   - Nothing sends load to Main or Worker during their blocks. That includes opening the UI, Insights or `/metrics` by hand (CR-SPEC 6.11).
5. **Content safety:** the child-safety core stays on and unmodified. Metadata only: nothing is downloaded or seeded.
   - **Nothing identifying leaves a VM:** no infohashes, names, peer addresses or logs. Main's and Worker's disks are encrypted (Linode disk encryption) and Win's volume is encrypted EBS; all of it is destroyed with the VM.
   - **This includes browser artifacts.** Against a crawl-grown store, Playwright traces, screenshots and HTML reports, and Lighthouse reports (they embed screenshots and a filmstrip), show real torrent names. Run with `--trace off` and no screenshots or video. Copy off numbers only. Delete the reports before teardown.
6. **Scrub before publishing** (handoff §6): machine names, Linode and AWS IPs and instance IDs, PIA exit IPs, server names and region. The collector already keeps only an IP verdict (`reference`/`unchanged`/`changed`), never the address.
7. **No UI port on the internet.**
   - Lodestar's `8090` is published on the gluetun container as `127.0.0.1:8090:8090` on Main's host, and reached only over SSH (`ssh -L`).
   - Win uses its own key pair, created for this session and deleted at teardown.
   - No SSH agent forwarding.
8. **Cost cap:**
   - **Akamai:** about $3.82 for 48 h (one main plus one worker) and $0.36 for the canary.
   - **AWS:** c6a.xlarge on Windows costs about $0.337/h with the licence (`#625` corrected the scripts). With `ExpiresAt` at hour 42 the most Win can cost is about $14.50 plus the volume, which is within the owner's accepted ~$15.
   - **NAT:** check whether Win's subnet sends egress through a NAT gateway, which bills per GB.
   - **Teardown:** stop each VM as soon as its lane is done.
9. **Provider terms:** `#625` lists Akamai's AUP as not checked for a DHT workload. Through PIA, Akamai sees one WireGuard flow per VM, and abuse reports go to PIA. If Akamai, AWS or PIA raises any complaint, stop crawling at once and tell the owner.
10. **Times are UTC everywhere** (§2.35), including gluetun's `TZ`. `#625`'s example uses another zone; set `TZ=UTC`.

## Stop conditions
- **The leak test fails:** no crawl starts on that VM.
- **A watchdog canary isn't deleted:**
  - Delete the canary by hand.
  - The fleet runs only with the owner's explicit OK and a manual-teardown reminder, because the 48 h backstop is unproven.
- **The tunnel drops in a block:** the segment is invalid (CR-SPEC 6.5). The outage watcher (0e) logs it. A gluetun restart also needs a Lodestar restart (`docs/operations.md` §4.3), which ends the block.
- **The OOM killer acts on Worker:** that's a finding, not a failure. Record which process died (`dmesg`), don't restart inside the block, and write the block off.
- **A complaint from any provider:** stop, as in rule 9.

## Phase 0: prove the safety net and the tunnel
**0a. Akamai canary (start at least 49 h before hour 0; about $0.36).**
- **Why a 49 h lead:** the watchdog has no expiry tag. It deletes in-scope Linodes only at 48 h after creation.
- **Create it in Cloud Manager or with the API, not with the fleet's Terraform.** An out-of-band deletion would leave that state stale.
  - Label `lodestar-canary-<yyyymmdd>`, tags `lodestar` and `test`.
  - Region us-east, type g6-nanode-1.
  - A random root password and no SSH key.
  - A drop-all-inbound firewall attached.
- **Check the scope the same day:** invoke the Lambda with `{"dry_run": true}`. The log must show `would_delete` for the canary only after it's 48 h old; before that it must be scanned and not eligible. This proves the token and the scope at once. The token-missing path only logs a warning until `#625`'s `TOKEN_REQUIRED` is deployed, so read the result, don't assume it.
- **Check between 48 h and 49 h after creation:**
  - the Linode API returns 404 for it;
  - the CloudWatch log has `deleted_verified` for its id;
  - the SNS email arrived.
- **Then delete the orphaned firewall by hand.** Write those steps down (the runbook in Deliverables).
- **A faster deletion test** needs a test-mode change to the deployed watchdog (a shorter age for one label). That's a change to a deployed safety service: the owner decides it, through a reviewed change set, not this plan.

**0b. AWS canary (hour −1; about $0.20).**
- **The canary must be in the guard's scope:** a c6a.xlarge in the Lodestar VPC and security group, with the three tags. A smaller instance is skipped as `out_of_scope`. So use the real script: `Deploy-AwsWindows.ps1 -ExpiresAt <now + 20 min>` (its minimum lease is 15 min).
- **Within about 35 min, confirm:**
  - the instance is `terminated`;
  - its root volume is gone;
  - the email arrived. It says "termination initiated", so confirm the final state in the console.
  - A `{"dry_run": true}` invocation beforehand shows `would_terminate` once the tag has passed.
- **The script refuses** while any Lodestar instance is pending, running, stopping or stopped. The canary must be fully terminated before Win is created.
- **Create Win with `-ExpiresAt` at hour 42,** so the backstop sits just past the planned teardown.

**0c. Tunnel setup (hours 0–2, Main and Worker).**
- **Use the documented path** (`docs/operations.md` §4.3; `packaging/unraid/lodestar-gluetun.xml`):
  - gluetun with PIA through gluetun's custom WireGuard provider, with Lodestar on `network_mode: service:gluetun`;
  - the gluetun image pinned by digest, not `:latest`;
  - the WireGuard endpoint fixed to one server, so a reconnect keeps the same exit IP;
  - `TZ=UTC`.
- **Firewall settings:** `FIREWALL_VPN_INPUT_PORTS` stays empty, because there's no forwarding. If the published `8090` doesn't answer on Main's loopback, add `FIREWALL_INPUT_PORTS=8090` and record why.
- **Lodestar has no DHT-level kill switch of its own;** the kill switch under test is gluetun's firewall.
- **Check how long a PIA WireGuard registration lasts.** The block runs about 34 h, and a manually negotiated peer may not stay valid that long. If it needs renewing, renew only outside blocks.

**0d. Leak test (Main and Worker).**
- **Where to test from:** inside the shared network namespace, with a throwaway container on `--network container:gluetun`.
- **With the tunnel up:**
  - the IPv4 echo returns the PIA exit;
  - an IPv6 request fails. Every Linode has a public IPv6 address, so this matters;
  - DNS answers only through gluetun.
- **With the tunnel down:**
  - How: block the WireGuard endpoint in the host's `DOCKER-USER` chain. gluetun keeps running, so its firewall is what's tested.
  - IPv4, IPv6, DNS and an outbound UDP send from the namespace all fail.
  - A capture on gluetun's side of the Docker bridge (its veth), before NAT and excluding the WireGuard endpoint, shows nothing leaving the namespace.
  - The host's own traffic (NTP, DNS, apt, IPv6 neighbour discovery) isn't Lodestar's. Leave it out of the capture and check the host's IPv6 separately.
  - Then remove the block and confirm the tunnel recovers.
- **SSH:** confirm SSH from the allow-listed IP still works throughout.
- **Stopping the gluetun container is a different case.** The namespace disappears and Lodestar must be restarted (§4.3). Test it once, outside any block.

**0e. Pre-stage (hours 0–2).**
- **Stop automatic updates once cloud-init has patched the VM.** Disable `unattended-upgrades` and the `apt-daily` and `apt-daily-upgrade` timers.
  - In Docker mode the analyzer fails every segment unless both checklist items are `excluded` with a next-run time (CR-SPEC 6.5, `analyze_cr.py` `host_checklist`). So every collector gets `--checklist auto_update=excluded --next-run auto_update=<UTC after teardown>`.
  - Each also gets `--checklist backup=excluded --next-run backup=<UTC after teardown>`. Linode backups are off (`backups_enabled=false`).
- **Images and directories:**
  - pull the pinned images;
  - create the witness directory outside the data directory, as a bind mount (CR-SPEC 2.3), and a new, empty evidence root per segment;
  - set `oom_score_adj: -500` on gluetun and the collector, so that on Worker the kernel picks Lodestar rather than the tunnel or the evidence.
- **The arm-switch script, on each Linode.**
  - It waits for the running collector's `result.json` with the window complete.
  - It then runs `docker compose up -d --no-deps lodestar` with the next arm's environment, so gluetun isn't recreated.
  - It then starts the next segment's collector with a new `--root`.
  - The operator only watches. A switch must finish within 10 minutes (CR-SPEC 2.3); the script logs its own timings.
- **The outage watcher, on each Linode.** It follows `docker events` and gluetun's log. On any VPN restart or tunnel-down line it runs `collect.py note --root <current run> --category link_outage` at once, because rule 6.11 counts notes only when they're written at the time.
- **On Win:** install MSVC Build Tools, the Rust toolchain and Node, and build the bench and test binaries. Record `rustc -V` and the binaries' SHA-256.

## Lanes (all run concurrently, one per VM)

### Lane L: Main, the crawler yield lane (Linux, Docker, PIA)
Runs the Linux packaging as shipped (`docker compose`, the Unraid-equivalent image) on a fresh data directory.
- **Start (hour 2–3), in this order, within minutes:**
  1. Start Lodestar on the fresh data directory.
  2. Read the setup token from its log and open the UI over `ssh -L`.
  3. Accept the Legal Notice and set the admin password (the collector stops with `notice_required` otherwise).
  4. Write the password to a `chmod 600` file.
  5. Start the collector.
  - Set crawler switches by environment only; the collector refuses a `config.toml`.
  - L1 smoke happens here: `/healthz`, and DHT nodes and the routing table growing. Then leave the UI alone (rule 4).
- **L2: one CR-ADM block (Decision 3).**
  - **The design:** ABBA (control, CR-ADM on, CR-ADM on, control), 6 h segments. Every other switch stays at its shipped default; `LODESTAR_DHT_SAFE_RESTART=true` for the whole block. Each arm switch is a clean stop and a resumed start within 10 minutes, followed by a 60 min warm-up (CR-SPEC 2.3, 2.4).
  - **Timing:** from container start, 6 h suppression + 1 h warm-up + 4 × 6 h + 3 × (switch + 1 h) ≈ 34.5 h. It starts by hour 3 and ends by about hour 37.5.
  - **The collector,** one process per segment. It runs in **gluetun's network namespace** (`--network container:gluetun`), so that its public-IP check sees the PIA exit and `--url http://127.0.0.1:8090` reaches Lodestar on the shared loopback. The command: `--mode docker --container lodestar --run-kind ab --treatment adm --intensity balanced --stop --commit <sha> --spec-commit <sha> --password-file … --root <new dir per segment> --proc-root /host/proc --storage-path <data> --storage-path <witness>`, plus the checklist flags from 0e.
  - **The Docker socket** is mounted as a unix socket. Never use a TCP socket proxy inside gluetun's namespace, because Lodestar shares that loopback.
  - **No `--require-gluetun` or `--leg gluetun`:** CR-SPEC run kinds refuse both (`collect.py` `prepare_spec`). Instead:
    - before each start, check that `docker inspect` shows the Lodestar container's NetworkMode as `container:<gluetun>`;
    - after it, check that `run.json` has `network.gluetun_network_parent: true`.
  - **Judged by** `analyze_cr.py`, mechanism checks included (CR-SPEC 5.1: a segment with no admission grants is void, not negative).
  - **One block is half of CR-SPEC's minimum of two.** So the verdict uses the words in Verification 3, never an outcome under rule 6.3.
- **Metrics:**
  - `searchable_new_total{origin="network"}` per minute;
  - `discovery_to_searchable_seconds` p95 and coverage;
  - the funnel (`fetch_*`), admission grants and refusals, evictions;
  - RSS and cgroup `anon`;
  - the routing-table size.
  - Compare against the stored CR-0 baseline (about 717 stored/h on Windows; target 100/min) as context only, since the host, IP and network differ.
- **After the block (from about hour 37.5):** the fourth collector's `--stop` ends Lodestar. Restart it with the control settings: with `safe_restart` that's a `resumed` start. Run one observer collector without `--stop` and without a run kind, so the post-block hours are recorded but never judged.
- **L3 live-ingest UX/perf (hours 37.5–39; from Win, see W3b):**
  - Main is ingesting into its largest store.
  - Measure search p95, typeahead, facets and Insights, and run the architecture §17 query (`file:(ext:mkv size:>2GB) res:2160p seeders:>=1`).
  - It's the real "reporting overhead" case; compare with the `reporting-overhead` numbers. Label every number as post-block.
- **L4 stop/restart:** covered by the block's three arm switches. Each must show reason `resumed`, with suppression skipped and data intact. A switch that isn't `resumed` is published (CR-SPEC 2.3).
- **Failure drills (hours 39–40, after W3b):** drop the tunnel, restart the container, stop gluetun.
  - Confirm nothing leaves outside the tunnel.
  - After each restart, confirm a `resumed` start, counters that carry on from their stored values, and no data lost.

### Lane K: Worker, the 1 GiB lane
- **No low-resource mode exists in the server yet.**
  - Architecture §3 describes it, but nothing implements it. M8-PERF-1 only plans a low-resource bench mode.
  - So Worker runs the **same image at the shipped Balanced preset**, in lockstep with Main's block: the same arm at the same hours, switched by the same script. It's analysed as its own block.
  - This answers the question that matters before any low-resource default is chosen: does a 1 GiB box survive the defaults, and at what yield?
- **The collector** is as on Main, with storage guardrails that fit the 25 GB disk. The defaults want 50 GiB free and would refuse to start. Use `--min-free-bytes`, `--warn-free-bytes` and `--stop-free-bytes` of 12, 8 and 4 GiB.
- **Checks:**
  - cgroup `anon` and RSS against the 1 GiB machine (256 MB swap);
  - OOM kills, and which process died (`dmesg`, container exit 137);
  - swap use and CPU saturation on one vCPU, with gluetun's WireGuard encryption counted separately;
  - yield per vCPU;
  - whether the UI stays usable (checked once, after the block).
  - The collector's 1 GiB memory stop rule can't fire on this box: the kernel acts before `anon` reaches 1 GiB. The OOM record is the finding.
- **Worker mode (§19) is not in this session.**
  - It needs the WK-10 epic finished (WK-10e is still open) and a released image with the join flow.
  - It also needs the cluster port `8091/tcp` (mTLS) reachable from Worker to Main. Both crawl behind PIA without forwarding, so that needs one of two infra changes:
    - a firewall rule plus a gluetun outbound-subnet exception on Worker and an input port on Main's gluetun;
    - or a WireGuard overlay between the two, which architecture §19.2 prefers.
  - Book it as its own session. Its checks: worker RSS ≤ 256 MB, ≤ 3 KB per torrent on the wire, grant latency p95 (WAN, recorded apart from the LAN target), and kill and partition drills with §19.8's funnel reconciliation.
- **Both Linodes are in Newark,** so any WAN latency is optimistic; note it in the report.

### Lane W: Win, the Windows lane plus UX and benchmarks
Win is the measuring box: it never crawls in this session, and it runs one measuring job at a time (rule 4).
1. **Install and service (hours 0–2):**
   - **What it is:** `packaging/windows/smoke.ps1` already passes in CI (windows-latest) and in the v0.5.1 release verdict, so this repeats it on a clean Windows Server image, elevated, instead of a hosted runner.
   - **The steps:**
     - install, service start, `/healthz`;
     - the secure data directory (`secure-datadir.ps1`, `dacl-check.ps1`, `service-sid.ps1`);
     - upgrade over the previous MSI;
     - uninstall with `REMOVE_DATA=1`.
   - **The SmartScreen/Defender experience** of the unsigned MSI (owner §2.23: no code signing) needs a desktop and a downloaded file. Use Fleet Manager Remote Desktop over SSM (no inbound needed) and download the MSI in Edge, so it carries the Mark of the Web.
   - **Also check** the firewall rules on a Server SKU.
2. **Benchmarks (hours 2–10; three repeats each, median, CPU steal flagged):**
   - **What runs:** `size-report --gate`, `search-bench` at 200k (to compare with CI) and 1M, `relevance --gate`, the `reporting-overhead` example (17 min), and the filter budget test. CI calibrates the filter budget to an EPYC 7763.
   - **Label every result** "c6a.xlarge, 2 cores / 4 threads, 8 GiB, EBS <type>, Windows". §3's reference is 4 cores and NVMe, so these are leads.
   - **10M** runs only if Prerequisite 3 gave a larger volume, in its own slot (hours 24–37). Earlier 10M runs used 15 GiB RAM, so 8 GiB results aren't comparable to them.
   - **The 1.0 gate (G-v1-10m) stays on a dedicated runner** (`docs/plan/2026-11-06-m8-hardening.md` OQ1).
3. **Browser UX:**
   - **The repo's configs can't target Main as they are.** `lighthouserc*.json` starts the local preview server (the mock backend), the Playwright `core` projects run against that mock, and `fixture` asserts on the fixture dataset.
   - **(a) Hours 10–13, on Win alone:** run the repo suites as shipped, as the first Windows Server numbers.
     - Use `--ignore-snapshots`, because the visual baselines are Linux-only.
     - Use a browser downloaded on Win, or point `PW_CHROMIUM_PATH` at Edge. Call the results Edge numbers only if Edge was the browser.
     - Firefox is exploratory.
   - **(b) Hours 37.5–39, against Main** over the SSH tunnel (Prerequisite 2):
     - Lighthouse with a run-specific config: `collect.url` set to the tunnelled Main, no `startServerCommand`, signed in. The repo thresholds are the yardstick: mobile LCP ≤ 2500 ms, TBT ≤ 100 ms, CLS ≤ 0.02, ≤ 100 KB; desktop LCP ≤ 1000 ms.
     - A small run-specific Playwright script for the interactions only a real store shows: a throttled 4G profile, INP < 100 ms, 10k-row virtual list scroll at 60 fps, SSE insert batching while Main ingests, drawer prefetch on hover, and the file-tree Web Worker on a torrent with more than 2k files.
     - Rule 5 applies: no traces, no screenshots, numbers only. The script itself holds no data and goes in the evidence directory.
4. **Windows CI flake reproduction (hours 13–24, and 24–37 when there's no 10M run):** loop the timing-sensitive tests on Win, the slowest platform CI uses, to find flakes that don't show on Linux runners. Report a flake to the runner before writing a fix (`CLAUDE.md`, multi-session rules).
5. **No Windows crawl this session.**
   - It would break rule 4 on the measuring box.
   - A PIA client's kill switch can also cut the SSM agent, which is the only way into Win, so a dropped tunnel locks the box until a reboot.
   - Book a Windows arm separately, with split tunnelling for the SSM endpoints and the same leak test.

## Cross-lane experiments that need concurrency
| Experiment | How the three VMs help |
|---|---|
| CR-ADM on two machine sizes | Main and Worker run the same ABBA block in the same hours: one lead at 4 vCPU/8 GiB and one at 1 vCPU/1 GiB, never pooled. |
| Search while ingesting | After the block, Win drives UX and API load against Main while it still crawls. Main reports ingest rate and writer lag; Win reports latency. |
| Failure drills | After the block, Main's tunnel drop and container restart are checked for leaks and data loss. |
| Memory versus yield | Worker (1 GiB) against Main (8 GiB) at the same preset and switches shows the memory-versus-yield curve before low-resource defaults are set. |

## Timeline (hours from fleet creation, hour 0)
| Hour | Main | Worker | Win |
|---|---|---|---|
| −49 | Akamai canary created and dry-run checked (0a) | | |
| −1 to 0 | check canary deleted; delete its firewall | | AWS canary (0b) |
| 0 to 2 | provision; tunnel; leak test; pre-stage | same | MSI smoke; toolchain and builds |
| 2 to 3 | fresh start, setup, collector (L1) | same, same minutes | bench 200k |
| 3 to 10 | suppression (6 h), then warm-up (1 h) | same | bench 1M, size-report, relevance, filter budget, reporting-overhead |
| 10 to 16 | segment A (control) | A | repo e2e/Lighthouse on Win (10–13); flake loops |
| ~17 to 23 | segment B (CR-ADM on) | B | flake loops |
| ~24 to 30 | segment B | B | 10M if the volume allows, otherwise flake loops |
| ~31 to 37.5 | segment A | A | as above |
| 37.5 to 39 | restart in control; L3 under Win's load; §17 query | UI check | UX against Main (W3b) |
| 39 to 40 | failure drills | drills (`kill -9`, restart) | delete reports |
| 40 to 41.5 | copy off and scrub evidence; destroy | destroy | destroy |
| 41.5 to 48 | buffer: nothing is meant to run. Backstops: Win's `ExpiresAt` at hour 42, the Akamai watchdog at hour 48 | | |

There's a scripted switch and a 1 h warm-up after each segment. The block has no slack: an invalid segment writes it off (CR-SPEC 6.7), and there's no time to re-run it. Report whatever segments are valid, marked as such. If the watchdog ever deletes a Linode before Terraform does, reconcile the state as the runbook says before any later `apply`.

## The cloud screen (standing stage)
Approved by the owner 2026-11-08Z. Once a crawler improvement is merged, it gets a short cloud screen before it books the experiment host's longer A/B blocks. The screen spends cheap cloud hours so that the scarce host's time isn't spent on a change that's broken.
- **Where:** the two Linodes only (about $4 per 48 h); the Windows VM isn't needed. Phase 0, the leak test and every ground rule above apply.
- **How:**
  - one fresh start, then one ABBA block with resumed restarts on both Linodes in lockstep;
  - 3 h segments, collected with `--rehearsal --baseline-hours 3`, which `run.json` records and `analyze_cr.py` marks "rehearsal: not gate evidence";
  - about 7 h + 4 × 3 h + 3 h ≈ 22 h of crawling, so a verdict in about a day.
  - A session can screen two candidates one after the other on the same data directory, each in its own ABBA block.
- **It can only stop a change for a clear failure,** predeclared here and never argued after the data is in:
  1. **The switch didn't engage:** the mechanism check (CR-SPEC rule 5) fails in any on-segment.
  2. **Instability:** a crash, an OOM kill of Lodestar on Main, a counter reset, or the CR-ADM permit identity stuck non-zero for 60 minutes.
  3. **A clear guardrail regression:** a CR-SPEC 5.2 guardrail worse in both B segments than in both A segments, on both VMs, by more than 25 %.
  4. **A large loss:** the net verified searchable rate lower in both B segments than in both A segments, on both VMs, by more than 25 %.
- **Anything else goes on to the experiment host,** including "no effect" and modest gains or losses. Cloud noise is far above CR-SPEC's ±5 % margin, so a screen never kills an idea for lack of a win.
- **Records:** each screen goes in its slice's decisions file, or in a small runner PR, as "screen: pass" or "screen: stop (rule n)", with the evidence directory. A stop goes back to the slice's owner. The runner decides whether to fix it and screen again (§2.26).
- **The 25 % margins are the runner's measurement parameters** (§2.26), flagged to the owner (§2.19). They change only by a decisions file merged before the screen they apply to.

## Deliverables (when run)
- This plan, and a runbook under `docs/operations/` covering:
  - provisioning, the canaries and the orphan cleanup;
  - the tunnel, the leak test, the switch script and the outage watcher;
  - teardown, and Terraform state reconciliation after an out-of-band deletion;
  - evidence copy-off and scrubbing.
- **Per run:**
  - `docs/soak/evidence/<date>-cloud-<lane>/`: aggregates, `analysis.md` from `analyze_cr.py`, and the bench and Lighthouse numbers;
  - a `docs/decisions/<date>-cloud-pilot.md`, recording the results, anything that touches an owner number (§2.26, flagged per §2.19) and the follow-up slices.
- Anything weakening a security or content-safety rule goes to the owner first. Open PRs follow `.claude/skills/steward/SKILL.md`; one PR per slice, titled `<id>: <title>`.
- Slice entries go through the runner (`runner/slices.yaml` is its to edit), so findings land as slices, not as ad-hoc PRs. This plan's PR is `P-cloud-test-plan`. The infra follow-up (an admin-address list for Main's firewall, a root-volume size parameter for Win) is a slice the runner adds.

## Verification of the plan itself (what "done" looks like)
1. Both watchdogs deleted their canaries and emailed; the orphan cleanup steps are written.
2. The leak test shows zero egress outside PIA with the tunnel down, IPv6 and DNS included.
3. Every lane has evidence that `analyze_cr.py` reads (or a bench/Lighthouse report), with the commit, image digest and binary SHA-256 recorded.
   - Each crawl block's verdict is one of: **lead: win**, **lead: loss**, **lead: unclear**, or **void** (the mechanism didn't engage). Each verdict comes with its reason.
   - A bench or UX result is **within budget** or **regression**, against the labelled machine.
4. All three VMs are confirmed deleted (EC2 console and Linode API), with no orphaned firewalls, volumes or reserved IPs. Win's SSH key is removed from Main, and the PIA configurations are deleted locally.
5. No published file contains a torrent name, infohash, peer address, VM IP, instance ID or PIA exit.

## Decisions (answered by the owner, 2026-11-08Z)
1. **Cloud crawl runs are exploratory only,** not CR-SPEC evidence (the runner's recommendation, taken under §2.26). Recorded in `docs/decisions/2026-11-08-P-cloud-test-plan.md`.
2. **Windows cost:** the owner accepted the estimate of about $15 for 48 h. At $0.337/h, `ExpiresAt` at hour 42 keeps Win at about $14.50 plus the volume.
3. **First treatment on Main and Worker: `dht.coordinated_admission` (CR-ADM).** It's first in the live-run order, and the CR-0 checkpoint showed 98.9% of pool evictions happening before an accepted lookup.
   - **The second is uTP (`dht.utp`, CR-4).** With no inbound port, outbound-only transports matter most.
   - **Revised with the owner's OK the same day:** one CR-ADM ABBA block fills this session, so uTP is the first block of the next one. It's no longer a second round in this session.
   - **Route backoff** waits until `fetch_no_route_total{family="ipv6"}` is above 0, as on the Unraid plan.
   - **One treatment at a time,** the same on every VM in the same hours, with the control inside the block (ABBA).
4. **Port forwarding is not available** (US regions). All crawling is outbound-only through PIA. Testing a non-US region with forwarding would be a separate, later treatment, because it also changes distance and IP.
5. **A standing cloud screen** comes before the experiment host's A/B blocks for each merged crawler improvement (the owner, 2026-11-08Z; see [The cloud screen](#the-cloud-screen-standing-stage)).

## Review changes (2026-11-08Z)
What the review found in the first version, and what changed:
1. **Phase 0 couldn't have proven the Akamai backstop.** The watchdog ignores expiry tags and deletes at 48 h after creation, so a 1 h canary would never go. Also, "a small EC2" is outside the AWS guard's scope, which only acts on c6a.xlarge in one security group. Now: an Akamai canary 49 h ahead, an AWS canary made with the real script, and dry-run invocations to check both scopes at once.
2. **Toggle "rounds" became one CR-SPEC ABBA block.** A second round needs another 6 h suppression and puts each arm in different hours. The block uses resumed restarts, keeps the control in the same hours, fits by hour 37.5 and reuses `analyze_cr.py`. uTP moves to the next session (the owner's OK).
3. **Measurement contamination removed:**
   - UX load from Win during Main's measured window, and the restart drill inside it, would each have invalidated segments (CR-SPEC 6.5);
   - a Windows crawl on the measuring box broke rule 4.
   - Win's jobs now run one at a time.
4. **Things that would have voided the evidence:**
   - `--require-gluetun` is refused for A/B runs;
   - `unattended-upgrades` without `excluded` checklist entries fails every Docker segment;
   - the default free-space guardrails refuse Worker's 25 GB disk;
   - setup in the wrong order;
   - nobody named for the arm switches;
   - tunnel outages nobody would log;
   - on Worker, the OOM killer free to take the tunnel or the collector.
5. **Steps the repo can't run as written:**
   - There's no low-resource mode in the server, so Worker now runs the shipped preset.
   - The Lighthouse configs and the Playwright `core` and `fixture` projects can't target Main.
   - `reporting-overhead` is a bench example and the filter budget is a test, not bench bins.
   - `analyze_cr.py`, not `analyze_cr0.py`, judges A/B runs.
   - The SmartScreen check needs a desktop.
6. **Hardware claims corrected:** c6a.xlarge is 2 physical cores, not §3's 4. Win's 50 GiB volume can't hold the 10M corpus. Worker mode (§19) needs WK-10e and a cluster-port path that the Terraform doesn't provide, so it's deferred.
7. **Blind spots added:**
   - no route from Win to Main's UI (Prerequisite 2);
   - IPv6 and DNS in the leak test, with the capture on gluetun's side so host traffic doesn't confuse it;
   - the collector's public-IP check run inside the tunnel;
   - separate PIA exits per Linode;
   - browser artifacts carrying real names;
   - the Windows licence in the cost;
   - stop conditions;
   - provider complaints;
   - the PIA registration lifetime;
   - Terraform state reconciliation.
8. **Added at the owner's request:** the standing cloud screen.
