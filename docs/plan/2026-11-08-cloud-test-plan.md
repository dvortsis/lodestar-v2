# Cloud VM test plan: concurrent lanes on the 48 h pilot fleet

Planning note, 2026-11-08Z, for the build runner. Docs only; it starts no VM and changes no code. **Status: approved; the open questions were answered 2026-11-08Z (see Decisions). Revised the same day after a review against the deployed infrastructure (`dvortsis/lodestar-v2#623`), CR-SPEC and the repo's tools; what changed and why is in [Review changes](#review-changes-2026-11-08z) and `docs/decisions/2026-11-08-P-cloud-test-plan.md`.**

**Authority:** `docs/owner-decisions.md` §2.19, §2.23, §2.25, §2.26, §2.51; `docs/decisions/2026-11-02-CR-SPEC.md`; `docs/build-plan.md` §0.5; `docs/handoff.md` §6; `docs/operations.md` §4.3; architecture §3, §10.1, §17, §19.8.

## Context
We now have a disposable three-VM fleet:

| VM | What it is | Access | Expiry backstop |
|---|---|---|---|
| **Win** | AWS c6a.xlarge, us-east-1: 4 vCPU on **2 physical cores** (SMT), 8 GiB, Windows Server, 50 GiB encrypted gp3 root volume | SSM only; the security group allows no inbound traffic | `lodestar-expiration-guard`, every 15 min: terminates at `min(ExpiresAt tag, launch + 48 h)`. Scope: c6a.xlarge in the Lodestar VPC and Windows security group, tagged `Project=Lodestar`, `Environment=Test`, `ManagedBy=Terraform` |
| **Main** | Linode g6-standard-4, us-east: 4 shared vCPU, 8 GiB, 160 GB, Ubuntu 24.04 | SSH from one admin `/32` only; all other inbound dropped | `lodestar-akamai-watchdog`, every 30 min: deletes at **48 h after creation**, whatever the tags say. Scope: label `lodestar-*`, tags `lodestar` and `test`, region `us-east`, type g6-standard-4 or g6-nanode-1, created after 2026-11-08T00:00Z. At most 4 deletions a run. It leaves firewalls, volumes and reserved IPs behind |
| **Worker** | Linode g6-nanode-1, us-east: 1 vCPU, 1 GiB, 25 GB, Ubuntu 24.04 | as Main | as Main |

Each VM has its own public IP; Main and Worker crawl only through PIA. No VM is running yet. Both watchdogs are deployed, but **neither has deleted a real instance yet**. The provisioning scripts and templates are in `dvortsis/lodestar-v2#623` (`infra/`, `docs/operations/lodestar-cloud-assets.md`).

The goal is the owner's standing one: **better performance and UX** (`docs/owner-decisions.md` §2.26). Until now every live crawl ran on one Windows host with one public IP, one run at a time (`docs/decisions/2026-11-02-CR-SPEC.md`). That is slow. A 24 h window after the 6 h suppression and 1 h warm-up takes a day and a half per data point, and there were no Linux, low-RAM or second-IP numbers. Three machines with three IPs give parallel lanes, a real Linux/Docker target, a 1 GiB target, and a real Windows target.

## Prerequisites (all before hour 0)
1. **Infrastructure:** `#623` is merged (or its scripts are reviewed as the source used), and the Akamai Terraform state is reconciled. `#623` itself says not to run `terraform apply` before that, because a fresh state can recreate machines.
2. **Win can reach Main.** Main's firewall allows TCP/22 only from one admin `/32` (`ssh_admin_ipv4_cidr` takes exactly one). Lanes L3 and W3 need Win's egress `/32` allowed on TCP/22 too, so `#623` or a follow-up needs a list variable. Without it, the cross-VM UX steps are skipped, not opened up some other way.
3. **Disk on Win:** the 50 GiB root holds Windows, the Rust toolchain and one build (about 15 GiB with the target directory). The 10M corpus alone was about 19 GB on disk (`docs/decisions/2026-11-05-X-search-files-4.md`). The 10M step runs only if the provisioning script gets a larger root volume (it hardcodes 50 GiB). Otherwise the step is skipped and the report says so.
4. **Two PIA WireGuard configurations,** for two different exit servers in one US region: one for Main, one for Worker. Two Lodestar crawlers behind one exit IP are "another crawler on the public IP" (CR-SPEC rule 6.5). Neither exit may be one the Unraid experiment host uses during the session. Configurations are copied host to host over SSH only, never through Git, Terraform, cloud-init or chat.
5. **A frozen build:** one source commit, the Docker image pinned by digest, and the MSI from the same commit's CI. Record the commit, the digest and the binaries' SHA-256. The build must have `LODESTAR_DHT_COORDINATED_ADMISSION` (it's on `main`).
6. **The owner books the 48 h** and is reachable for the stop conditions below.

## Ground rules (fit the existing contracts)
1. **Cloud crawl numbers are leads, not gate evidence.** CR-SPEC fixes the acceptance run to one node, one public IP, a fresh data directory, the Balanced preset, and no other Lodestar run within 12 h on that IP. Cloud runs differ (VPN exit IP, shared CPU, different size). They screen candidates. The CR-COMB acceptance run stays on the experiment host (Decision 1).
   - **The experiment host** is the owner's Unraid box, with the DHT port not forwarded and UPnP off (owner §2.51). Build-plan §0.5 requires an identical network setup every run.
   - **Every crawling session goes through PIA, in a US region, which has no port forwarding.** The DHT port is therefore not forwarded, the same as on the Unraid host. The cloud VMs still differ in IP, CPU and location, so cloud yields are never compared directly with Unraid arms.
   - **The use is** to screen candidates while the Unraid host is booked, and to run the arms the Unraid box can't: Linux, 1 GiB, a second IP.
2. **Use the existing tools and run them as their READMEs say:**
   - Crawls: `docs/soak/tools/linux/collect.py` with `--run-kind ab`, judged by `docs/soak/tools/analyze_cr.py`. `analyze_cr0.py` is the older baseline analyzer and is used only for the CR-0 comparison.
   - Windows: `packaging/windows/smoke.ps1`.
   - Benchmarks: the `bench/` bins `search-bench`, `size-report` and `relevance`, the `reporting-overhead` example (`cargo run --release -p lodestar-bench --example reporting-overhead -- --gate`), and the filter budget test (`cargo test --release -p lodestar-filter --test props -- --ignored --nocapture budget`).
   - Browser: `web/lighthouserc*.json` and `web/e2e`.
3. **One variable changes at a time,** and the same build runs on every VM. Main (4 vCPU) and Worker (1 vCPU) are not like-for-like, so each VM's results are analysed on their own and never pooled.
4. **Nothing that measures shares a VM or a time slot with a crawl's measured window.** Benchmarks never run on a crawling VM. Client-side timings (Lighthouse, Playwright) never run on a box that is crawling or benchmarking. Nothing sends load to Main or Worker during their blocks (see the timeline).
5. **Content safety:** the child-safety core stays on and unmodified. Metadata only: nothing is downloaded or seeded.
   - **Nothing identifying leaves a VM:** no infohashes, names, peer addresses or logs. Main's and Worker's disks are encrypted (Linode disk encryption) and Win's volume is encrypted EBS; all of it is destroyed with the VM.
   - **This includes browser artifacts.** Against a crawl-grown store, Playwright traces, screenshots and HTML reports, and Lighthouse reports (they embed screenshots and a filmstrip), show real torrent names. Run with `--trace off` and no screenshots or video. Copy off numbers only. Delete the reports before teardown.
6. **Scrub before publishing** (handoff §6): machine names, Linode and AWS IPs and instance IDs, PIA exit IPs, server names and region. The collector already keeps only an IP verdict (`reference`/`unchanged`/`changed`), never the address.
7. **No UI port on the internet.** Lodestar's `8090` is published on the gluetun container as `127.0.0.1:8090:8090` on Main's host and reached only over SSH (`ssh -L`). Win uses its own key pair, created for this session and deleted at teardown.
8. **Cost cap:**
   - **Akamai:** about $3.82 for 48 h (one main plus one worker) and $0.36 for the canary.
   - **AWS:** c6a.xlarge on Windows costs about $0.34/h with the licence (check the AWS price page). The provisioning script prints $0.153/h, which is the Linux price. Torn down at hour 41 that's about $14, plus the volume, which is within the owner's accepted ~$15.
   - **Teardown:** stop each VM as soon as its lane is done; don't run to the backstop.
9. **Provider terms:** `#623` lists Akamai's AUP as not checked for a DHT workload. Through PIA, Akamai sees one WireGuard flow per VM, and abuse reports go to PIA. If Akamai, AWS or PIA raises any complaint, stop crawling at once and tell the owner.

## Stop conditions
- **The leak test fails:** no crawl starts on that VM.
- **A watchdog canary isn't deleted:**
  - Delete the canary by hand.
  - The fleet runs only with the owner's explicit OK and a manual-teardown reminder, because the 48 h backstop is unproven.
- **The tunnel drops in a block:** the segment is invalid (CR-SPEC 6.5); log it with `collect.py note --category link_outage`. A gluetun restart also needs a Lodestar restart (`docs/operations.md` §4.3), which ends the block.
- **Worker is OOM-killed:** that's a finding, not a failure. Record it, don't restart inside the block, and write the block off.
- **A complaint from any provider:** stop, as in rule 9.

## Phase 0: prove the safety net and the tunnel
**0a. Akamai canary (start at least 49 h before hour 0; about $0.36).**
- **Why the plan changed:** the watchdog has no expiry tag, so a short canary would never be deleted. It deletes in-scope Linodes only at 48 h after creation.
- **Create it in Cloud Manager or with the API, not with the fleet's Terraform.** An out-of-band deletion would leave that state stale.
  - Label `lodestar-canary-<yyyymmdd>`, tags `lodestar` and `test`.
  - Region us-east, type g6-nanode-1.
  - A drop-all-inbound firewall attached.
- **Check between 48 h and 49 h after creation:**
  - the Linode API returns 404 for it;
  - the CloudWatch log has `deleted_verified` for its id;
  - the SNS email arrived.
- **Then delete the orphaned firewall by hand.** Write those steps down (the runbook in Deliverables).
- **A faster test** needs a test-mode change to the deployed watchdog (a shorter age for one label). That's a change to a deployed safety service: the owner decides it, through a reviewed change set, not this plan.

**0b. AWS canary (hour −1; about $0.20).**
- **The canary must be in the guard's scope:** a c6a.xlarge in the Lodestar VPC and security group, with the three tags. A smaller instance is skipped as `out_of_scope`. So use the real script: `Deploy-AwsWindows.ps1 -ExpiresAt <now + 20 min>` (its minimum lease is 15 min).
- **Within about 35 min, confirm:**
  - the instance is `terminated`;
  - its root volume is gone;
  - the email arrived. It says "termination initiated", so confirm the final state in the console.
- **The script refuses** while any Lodestar instance is pending, running, stopping or stopped. The canary must be fully terminated before Win is created.
- **Create Win with `-ExpiresAt` at hour 42,** so the backstop sits just past the planned teardown, not at 48 h.

**0c. Tunnel setup (hours 0–2, Main and Worker).**
- **Use the documented path** (`docs/operations.md` §4.3; `packaging/unraid/lodestar-gluetun.xml`):
  - gluetun with PIA through gluetun's custom WireGuard provider, with Lodestar on `network_mode: service:gluetun`;
  - the gluetun image pinned by digest, not `:latest`;
  - the WireGuard endpoint fixed to one server, so a reconnect keeps the same exit IP.
- **Firewall settings:** `FIREWALL_VPN_INPUT_PORTS` stays empty, because there's no forwarding. `FIREWALL_INPUT_PORTS=8090`, so the port published on gluetun answers on the host's loopback.
- **Lodestar has no DHT-level kill switch of its own;** the kill switch under test is gluetun's firewall.
- **Lodestar's first run:**
  - set the admin password and accept the Legal Notice; the collector stops with `notice_required` otherwise;
  - set crawler switches by environment only; the collector refuses a `config.toml`.

**0d. Leak test (Main and Worker).**
- **Where to test from:** inside the shared network namespace, with a throwaway container on `--network container:gluetun`.
- **With the tunnel up:**
  - the IPv4 echo returns the PIA exit;
  - an IPv6 request fails. Every Linode has a public IPv6 address, so this matters;
  - DNS answers only through gluetun.
- **With the tunnel down:**
  - How: block the WireGuard endpoint in the host's `DOCKER-USER` chain. gluetun keeps running, so its firewall is what's tested.
  - IPv4, IPv6, DNS and an outbound UDP send from the namespace all fail.
  - `tcpdump -ni eth0 'not port 22'` on the host shows no packets except the blocked ones to the endpoint.
  - Then remove the block and confirm the tunnel recovers.
- **SSH:** confirm SSH from the allow-listed IP still works throughout.
- **Stopping the gluetun container is a different case.** The namespace disappears and Lodestar must be restarted (§4.3). Test it once, outside any block.

**0e. Pre-stage.**
- On Main and Worker: pull the pinned images, and create the witness directory outside the data directory as a bind mount (CR-SPEC 2.3).
- On Win: install the Rust toolchain and build the bench and test binaries. Record `rustc -V` and the binaries' SHA-256.

## Lanes (all run concurrently, one per VM)

### Lane L: Main, the crawler yield lane (Linux, Docker, PIA)
Runs the Linux packaging as shipped (`docker compose`, the Unraid-equivalent image) on a fresh data directory.
- **L1 smoke (during suppression):** `/healthz`, the dashboard shows DHT nodes and the routing table growing. This touches no measured window.
- **L2: one CR-ADM block (Decision 3).**
  - **Why a block:** a CR-SPEC A/B block instead of separate "rounds". Separate rounds would each need their own fresh 6 h suppression and would compare different hours.
  - **The design:** one block, ABBA (control, CR-ADM on, CR-ADM on, control), 6 h segments. Every other switch stays at its shipped default; `LODESTAR_DHT_SAFE_RESTART=true` for the whole block. Each arm switch is a clean stop and a resumed start within 10 minutes, with a 60 min warm-up (CR-SPEC 2.3, 2.4).
  - **Timing:** from container start, 6 h suppression + 1 h warm-up + 4 × 6 h + 3 × 1 h ≈ 34 h. It starts by hour 3 and ends by hour 37.
  - **The collector:** `--run-kind ab --treatment adm --stop --require-gluetun --password-file …`. It runs in **gluetun's network namespace** (`--network container:gluetun`, with the Docker socket and cgroup mounts from `docs/soak/tools/linux/README.md`) so that its public-IP check sees the PIA exit, not the Linode address.
  - **Judged by** `analyze_cr.py`, mechanism checks included (CR-SPEC 5.1: a segment with no admission grants is void, not negative).
  - **One block is half of CR-SPEC's minimum of two.** So the verdict is "lead: win/loss/unclear", never an outcome under rule 6.3.
- **Metrics:**
  - `searchable_new_total{origin="network"}` per minute;
  - `discovery_to_searchable_seconds` p95 and coverage;
  - the funnel (`fetch_*`), admission grants and refusals, evictions;
  - RSS and cgroup `anon`;
  - the routing-table size.
  - Compare against the stored CR-0 baseline (about 717 stored/h on Windows; target 100/min) as context only, since the host, IP and network differ.
- **L3 live-ingest UX/perf (hours 37–39, after the block; from Win, see lane W):**
  - Main keeps crawling in the control arm, so the writer is ingesting into the largest store.
  - Measure search p95, typeahead, facets and Insights, and run the architecture §17 query (`file:(ext:mkv size:>2GB) res:2160p seeders:>=1`). This is the real "reporting overhead" case; compare with the `reporting-overhead` numbers.
- **L4 stop/restart:** covered by the block's three arm switches. Each must show reason `resumed`, with suppression skipped and data intact. A switch that isn't `resumed` is published (CR-SPEC 2.3).
- **Failure drills (hours 38.5–39.5, after L3):** drop the tunnel, restart the container, stop gluetun. Confirm nothing leaves outside the tunnel and that the funnel reconciles after the restart.

### Lane K: Worker, the 1 GiB lane
- **No low-resource mode exists in the server yet.**
  - Architecture §3 describes it, but nothing implements it. M8-PERF-1 only adds a bench `--low-resource` run.
  - So Worker runs the **same image at the shipped Balanced preset**, in lockstep with Main's block: the same arm at the same hours (Decision 3: the same toggle on every VM in a round). It's analysed as its own block.
  - This answers the question that matters before any low-resource default is chosen: does a 1 GiB box survive the defaults, and at what yield?
- **Checks:**
  - cgroup `anon` and RSS against the 1 GiB machine (256 MB swap);
  - OOM kills (`dmesg`, container exit 137);
  - swap use and CPU saturation on one vCPU, with gluetun's WireGuard encryption counted separately;
  - yield per vCPU;
  - whether the UI stays usable (checked once, after the block).
  - The collector's own stop rule (process memory over 1 GiB for five samples) may end the run first. That's a finding.
- **Worker mode (§19) is not in this session.**
  - It needs the WK-10 epic finished (WK-10e is still open) and a released image with the join flow.
  - It also needs the cluster port `8091/tcp` (mTLS) reachable from Worker to Main. Both crawl behind PIA without forwarding, so that needs one of two infra changes to `#623`'s Terraform:
    - a firewall rule plus a gluetun outbound-subnet exception on Worker and an input port on Main's gluetun;
    - or a WireGuard overlay between the two, which architecture §19.2 prefers.
  - Book it as its own session. Its checks: worker RSS ≤ 256 MB, ≤ 3 KB per torrent on the wire, grant latency p95 (WAN, recorded apart from the LAN target), and kill and partition drills with the funnel reconciliation (§19.8).
- **Both Linodes are in Newark,** so any WAN latency is optimistic; note it in the report.

### Lane W: Win, the Windows lane plus UX and benchmarks
Win is the measuring box: it never crawls in this session (rule 4).
1. **Install and service (hours 0–2):**
   - **What it is:** `packaging/windows/smoke.ps1` already passes in CI (windows-latest) and in the v0.5.1 release verdict, so this repeats it on a clean Windows Server image, elevated, instead of a hosted runner.
   - **The steps:**
     - install, service start, `/healthz`;
     - the secure data directory (`secure-datadir.ps1`, `dacl-check.ps1`, `service-sid.ps1`);
     - upgrade over the previous MSI;
     - uninstall with `REMOVE_DATA=1`.
   - **Also checks:** the unsigned MSI's SmartScreen/Defender experience (owner §2.23: no code signing) and the firewall rules on a Server SKU.
2. **Benchmarks (hours 2–10, three repeats each, median, CPU steal flagged):**
   - **What runs:** `size-report --gate`, `search-bench` at 200k (to compare with CI) and 1M, `relevance --gate`, the `reporting-overhead` example (17 min), and the filter budget test. CI calibrates the filter budget to an EPYC 7763.
   - **Label every result** "c6a.xlarge, 2 cores / 4 threads, 8 GiB, EBS gp3, Windows". §3's reference is 4 cores and NVMe, so these are leads.
   - **10M** runs only if Prerequisite 3 gave a larger volume. Earlier 10M runs used 15 GiB RAM, so 8 GiB results aren't comparable to them.
   - **The 1.0 gate (G-v1-10m) stays on a dedicated runner** (`docs/plan/2026-11-06-m8-hardening.md` OQ1).
3. **Browser UX:**
   - **The repo's configs can't target Main as they are.** `lighthouserc*.json` starts the local preview server (the mock backend), the Playwright `core` projects run against that mock, and `fixture` asserts on the fixture dataset.
   - **(a) Hours 10–37, on Win alone:** run the repo suites as shipped. They are the first Windows Server/Edge numbers. Playwright is Chromium-only in the repo, so Edge and Firefox are exploratory.
   - **(b) Hours 37–39, against Main** over the SSH tunnel (Prerequisite 2):
     - Lighthouse with a run-specific config: `collect.url` set to the tunnelled Main, no `startServerCommand`, signed in. The repo thresholds are the yardstick: mobile LCP ≤ 2500 ms, TBT ≤ 100 ms, CLS ≤ 0.02, ≤ 100 KB; desktop LCP ≤ 1000 ms.
     - A small run-specific Playwright script for the interactions only a real store shows: a throttled 4G profile, INP < 100 ms, 10k-row virtual list scroll at 60 fps, SSE insert batching while Main ingests, drawer prefetch on hover, and the file-tree Web Worker on a torrent with more than 2k files.
     - Rule 5 applies: no traces, no screenshots, numbers only. The script itself holds no data and goes in the evidence directory.
4. **Windows CI flake reproduction (hours 10–37):** loop the timing-sensitive tests on Win, the slowest platform CI uses, to find flakes that don't show on Linux runners. Report a flake to the runner before writing a fix (`CLAUDE.md`, multi-session rules).
5. **No Windows crawl this session.**
   - It would break rule 4 on the measuring box.
   - A PIA client's kill switch can also cut the SSM agent, which is the only way into Win, so a dropped tunnel locks the box until a reboot.
   - Book a Windows arm separately, with split tunnelling for the SSM endpoints and the same leak test.

## Cross-lane experiments that need concurrency
| Experiment | How the three VMs help |
|---|---|
| CR-ADM on two machine sizes | Main and Worker run the same ABBA block in the same hours: one lead at 4 vCPU/8 GiB and one at 1 vCPU/1 GiB, never pooled. |
| Search while ingesting | After the block, Win drives UX and API load against Main while it still crawls. Main reports ingest rate and writer lag; Win reports latency. |
| Failure drills | After the block, while Worker and Win keep working: drop Main's tunnel and restart its container. The funnel reconciles and nothing leaks outside the tunnel. |
| Memory versus yield | Worker (1 GiB) against Main (8 GiB) at the same preset and switches shows the memory-versus-yield curve before low-resource defaults are set. |

## Timeline (hours from fleet creation, hour 0)
| Hour | Main | Worker | Win |
|---|---|---|---|
| −49 | Akamai canary created (0a) | | |
| −1 to 0 | check canary deleted; delete its firewall | | AWS canary (0b) |
| 0 to 2 | provision; tunnel; leak test | same | MSI smoke; toolchain and builds |
| 2 to 3 | start container and collector (fresh data dir); L1 during suppression | same, same minute | bench 200k |
| 3 to 10 | suppression (6 h), then warm-up (1 h) | same | bench 1M, size-report, relevance, filter budget, reporting-overhead |
| 10 to 16 | segment A (control) | A | repo e2e/Lighthouse on Win; flake loops |
| 17 to 23 | segment B (CR-ADM on) | B | flake loops; 10M if the volume allows |
| 24 to 30 | segment B | B | as above |
| 31 to 37 | segment A | A | as above |
| 37 to 39.5 | L3 under Win's load, §17 query, then drills | UI check; drills (`kill -9`, restart) | UX against Main (W3b) |
| 39.5 to 41 | copy off and scrub evidence; destroy | destroy | delete reports; destroy |
| 41 to 48 | buffer: nothing is meant to run. Backstops: Win's `ExpiresAt` at hour 42, the Akamai watchdog at hour 48 | | |

There's a 1 h warm-up after each arm switch (hours 16, 23 and 30). The block has no slack: an invalid segment writes it off (CR-SPEC 6.7), and there's no time to re-run it. Report whatever segments are valid, marked as such. The uTP block (Decision 3's second treatment) is the first thing booked for the next fleet session.

## Deliverables (when run)
- This plan, and a runbook under `docs/operations/`: provisioning, the canaries and the orphan cleanup, the tunnel and leak test, teardown, and evidence copy-off and scrubbing.
- Per run: `docs/soak/evidence/<date>-cloud-<lane>/` (aggregates, `analysis.md` from `analyze_cr.py`, bench and Lighthouse numbers) and a `docs/decisions/<date>-cloud-pilot.md`. It records the results, anything that touches an owner number (§2.26, flagged per §2.19) and the follow-up slices.
- Anything weakening a security or content-safety rule goes to the owner first. Open PRs follow `.claude/skills/steward/SKILL.md`; one PR per slice, titled `<id>: <title>`.
- Slice entries go through the runner (`runner/slices.yaml` is its to edit), so findings land as slices, not as ad-hoc PRs. This plan's PR is `P-cloud-test-plan`.

## Verification of the plan itself (what "done" looks like)
1. Both watchdogs deleted their canaries and emailed; the orphan cleanup steps are written.
2. The leak test shows zero egress outside PIA with the tunnel down, IPv6 and DNS included.
3. Every lane has evidence that `analyze_cr.py` reads (or a bench/Lighthouse report), with the commit, image digest and binary SHA-256 recorded. Each has a verdict of "lead" or "regression", with the reason.
4. All three VMs are confirmed deleted (EC2 console and Linode API), with no orphaned firewalls, volumes or reserved IPs, and Win's SSH key removed from Main.
5. No published file contains a torrent name, infohash, peer address, VM IP, instance ID or PIA exit.

## Decisions (answered 2026-11-08Z)
1. **Cloud crawl runs are exploratory only,** not CR-SPEC evidence (the runner's recommendation, taken under §2.26). Recorded in `docs/decisions/2026-11-08-P-cloud-test-plan.md`.
2. **Windows cost:** the owner accepted the estimate of about $15 for 48 h. With the Windows licence the rate is about $0.34/h, so teardown by hour 41 keeps it there; running to 48 h would be about $17.
3. **First treatment on Main and Worker: `dht.coordinated_admission` (CR-ADM).** It's first in the live-run order, and the CR-0 checkpoint showed 98.9% of pool evictions happening before an accepted lookup.
   - **The second is uTP (`dht.utp`, CR-4).** With no inbound port, outbound-only transports matter most. One CR-ADM block fills this session (review change 2), so uTP is the first block of the next one.
   - **Route backoff** waits until `fetch_no_route_total{family="ipv6"}` is above 0, as on the Unraid plan.
   - **One treatment at a time,** the same on every VM in the same hours, with the control inside the block (ABBA).
4. **Port forwarding is not available** (US regions). All crawling is outbound-only through PIA. Testing a non-US region with forwarding would be a separate, later treatment, because it also changes distance and IP.

## Review changes (2026-11-08Z)
What the review found in the first version, and what changed:
1. **Phase 0 couldn't have proven the Akamai backstop.** The watchdog ignores expiry tags and deletes at 48 h after creation, so a 1 h canary would never go. Also, "a small EC2" is outside the AWS guard's scope, which only acts on c6a.xlarge in one security group. Now: an Akamai canary 49 h ahead, and an AWS canary made with the real script.
2. **Toggle "rounds" became one CR-SPEC ABBA block.** A second round needs another 6 h suppression and puts each arm in different hours. The block uses resumed restarts, keeps the control in the same hours, fits by hour 37 and reuses `analyze_cr.py`. uTP moves to the next session.
3. **Measurement contamination removed:**
   - UX load from Win during Main's measured window, and the restart drill inside it, would each have invalidated segments (CR-SPEC 6.5);
   - a Windows crawl on the measuring box broke rule 4.
   - All three now run after the block, or not this session.
4. **Steps the repo can't run as written:**
   - There's no low-resource mode in the server. Worker now runs the shipped preset.
   - The Lighthouse configs and the Playwright `core` and `fixture` projects can't target Main. Win's runs are split into local runs and run-specific configs.
   - `reporting-overhead` is a bench example and the filter budget is a test, not bench bins.
   - `analyze_cr.py`, not `analyze_cr0.py`, judges A/B runs.
5. **Hardware claims corrected:** c6a.xlarge is 2 physical cores, not §3's 4. Win's 50 GiB volume can't hold the 10M corpus. Worker mode (§19) needs WK-10e and a cluster-port path that the Terraform doesn't provide, so it's deferred.
6. **Blind spots added:**
   - no route from Win to Main's UI (Prerequisite 2);
   - IPv6 and DNS in the leak test;
   - the collector's public-IP check had to run inside the tunnel;
   - separate PIA exits per Linode;
   - browser artifacts carrying real names;
   - Legal Notice and admin password before the collector starts;
   - the Windows licence in the cost;
   - stop conditions;
   - provider complaints;
   - Terraform state reconciliation before `apply`.
