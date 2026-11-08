# Cloud VM test plan: concurrent lanes on the 48 h pilot fleet

Planning note, 2026-11-08Z, for the build runner. Docs only; it starts no VM and changes no code. **Status: approved as a plan by the requester; the four owner decisions at the end are open.**

**Authority:** `docs/owner-decisions.md` §2.19, §2.23, §2.25, §2.26, §2.51; `docs/decisions/2026-11-02-CR-SPEC.md`; `docs/build-plan.md` §0.5; `docs/handoff.md` §6; `docs/operations.md` §4.3; architecture §3, §10.1, §17, §19.8.

## Context
We now have a disposable three-VM fleet: **Win** (AWS c6a.xlarge, 4 vCPU/8 GiB, Windows Server 2022+), **Main** (Linode g6-standard-4, 4 vCPU/8 GiB, Ubuntu 24.04) and **Worker** (Linode g6-nanode-1, 1 vCPU/1 GiB, Ubuntu 24.04). Each lives at most 48 h and has its own public IP; Main and Worker would sit behind PIA. No VM is running yet. The AWS and Akamai expiry watchdogs are deployed, but deletion has never been tested end to end.

The goal is the owner's standing one: **better performance and UX** (`docs/owner-decisions.md` §2.26). Until now every live crawl ran on one Windows host with one public IP, one run at a time (`docs/decisions/2026-11-02-CR-SPEC.md`). That is slow: a 24 h window plus a 7 h suppression start is a day and a half per data point, and there were no Linux, low-RAM or second-IP numbers. Three machines with three IPs give parallel lanes, a real Linux/Docker target, a 1 GiB low-resource target, and a real Windows target reached over the internet.

## Ground rules (fit the existing contracts)
1. **Cloud crawl numbers are leads, not gate evidence.** CR-SPEC fixes the acceptance run to one node, one public IP, a fresh data directory, the Balanced preset, and no other Lodestar run within 12 h on that IP. Cloud runs differ (VPN exit IP, shared CPU, different size). They screen candidates; the CR-COMB acceptance run stays on the experiment host unless the owner confirms otherwise (see Decisions).
   The experiment host is the owner's Unraid box with the DHT port not forwarded and UPnP off (owner §2.51; build-plan §0.5 requires an identical network setup every run). PIA with port forwarding on a cloud VM is a different setup, so cloud yields are never compared directly with Unraid arms. The use is to screen candidates (CR-ADM, uTP, route backoff, prefixes) while the Unraid host is booked, and to run the arms the Unraid box can't: Linux, 1 GiB, a second IP.
   Handoff §6 also applies: publish aggregates only, with machine names, IPs and **VPN exit IPs scrubbed** (as the R2 verdict did).
2. **Use the existing tools:** `docs/soak/tools/linux/collect.py` (Linux/Docker evidence), `docs/soak/tools/cr0-soak.ps1` (Windows), `docs/soak/tools/analyze_cr0.py`, `packaging/windows/smoke.ps1`, the `bench/` bins (`search-bench`, `size-report`, `reporting-overhead`, `relevance`), `web/lighthouserc*.json` and the Playwright specs in `web/e2e`. Evidence is aggregates only: no infohashes, names, peer addresses or logs leave a VM.
3. **Only one variable changes per lane,** and the same build (frozen commit hash, SHA-256 recorded) runs on every VM in a round. Two VMs never run different treatments in the same round and are then compared as if the VMs were equal: Main (4 vCPU) and Worker (1 vCPU) are not like-for-like.
4. **Benchmarks never share a VM with a crawl.** Shared-CPU noise would corrupt both.
5. **Content safety:** the child-safety core stays on and unmodified; metadata only, no content download or seeding. Data is destroyed with the VM; nothing is copied off but aggregate evidence.
6. **Cost cap:** Akamai about $3.82 for 48 h (from the brief); AWS Windows c6a.xlarge is probably around $15 for 48 h (my estimate, not from the brief; check the AWS price page). Stop a VM as soon as its lane is done; don't run to the 48 h limit.

## Phase 0: prove the safety net first (before any 48 h run), ~2 h, about $0.10
- Launch one throwaway nanode (and one small EC2) tagged with a **1 h expiry**. Confirm each watchdog deletes it within its interval (30 min Akamai, 15 min AWS) and that the alert email arrives. This closes the "not tested end to end" gap.
- Check what the Akamai guard leaves behind (firewall, volumes), and write down the manual cleanup steps.
- **Tunnel setup uses the documented path:** gluetun with PIA, Lodestar on `network=container:gluetun`, inbound allowed with `FIREWALL_VPN_INPUT_PORTS` and `LODESTAR_DHT_PORT` set to the forwarded port (`docs/operations.md` §4.3; `packaging/unraid/lodestar-gluetun.xml`). Lodestar has **no DHT-level kill switch of its own**, so the kill switch under test is gluetun's firewall. Start the collector with `--leg gluetun --require-gluetun`.
- **PIA leak test** on Main/Worker: with the tunnel up, confirm the public IP seen by an outside service is the PIA exit; kill the tunnel and confirm **all** UDP/TCP egress stops (kill switch); confirm SSH from the allow-listed IP still works; confirm inbound UDP (port forward) works, since DHT yield depends on it.
- Pre-stage images and the frozen build on each VM so billable time goes to testing.

## Lanes (all run concurrently, one per VM)

### Lane L: Main, the crawler yield lane (Linux, Docker, PIA)
Runs the Linux packaging as shipped (`docker compose`, the Unraid-equivalent image) on a fresh volume.
- **L1 smoke (first 30 min):** `docker compose up`, check `/healthz`, check the dashboard shows nodes and a growing torrent count, run the architecture §17 query (`file:(ext:mkv size:>2GB) res:2160p seeders:>=1`).
- **L2 yield run:** collector started right after the container (suppression of 6 h, warm-up of 1 h, then a 6 h to 24 h window per CR-SPEC rule 1). Metrics: `searchable_new_total{origin="network"}` per minute, `discovery_to_searchable_seconds` p95, funnel (`fetch_*`), admission rejections, evictions, RSS/handles. Compare against the stored CR-0 baseline (about 717 stored/h on Windows; target 100/min) to see whether the Linux + PIA path is better or worse, and whether the sampling-node provenance, stream-peers and admission changes (`docs/soak/2026-10-24-throughput-strategy.md`) pay off.
- **L3 live-ingest UX/perf (during the crawl, from Win, see lane W):** search p95, typeahead, facets and Insights while the writer is ingesting. This is the real "reporting overhead" case; compare with `reporting-overhead` numbers.
- **L4 stop/restart and `safe_restart`:** one clean stop and resumed start midway, to confirm suppression is skipped and data survives.
- Only one treatment toggle per round (for example `dht.target_prefixes` on or off); a toggle needs a second round, so queue the next candidate for the remaining hours.

### Lane K: Worker, the low-resource lane (1 GiB)
- **Until `WK-10` (worker runtime) ships:** run a standalone Lodestar in **low-resource mode** (architecture §3: trigram field and file index off, smaller caches) as a second independent crawler on a different IP. Checks: RSS well under the 1 GiB machine, no OOM kill, swap use, CPU saturation on one vCPU, yield per vCPU, and that the UI is still usable. This gives the first real "small box" number (N100/ARM-class).
- **After `WK-10`:** Worker joins Main as a coordinator client: worker RSS ≤ 256 MB, ≤ 3 KB per torrent on the wire, grant latency p95 < 50 ms (WAN here, so expect worse than the LAN target; record it as a separate number), kill/restart and partition drills (`kill -9`, `iptables` drop for 60 s) with the exactly-once funnel reconciliation (architecture §19.8). Marginal gain: Main alone vs Main + Worker, with the same 1-hour windows, alternating.
- Since both Linodes are in Newark, WAN latency is optimistic; note this in the report.

### Lane W: Win, the Windows lane plus UX and benchmarks
The Windows machine has a dedicated (non-burstable) c6a vCPU allocation, so it is the best place for stable timings. It isn't crawling, so it can measure.
1. **Install and service (first 1 h):** `packaging/windows/smoke.ps1` already passes in CI (windows-latest) and in the v0.5.1 release verdict, so this is a repeat on a clean Windows Server image (not a hosted runner), elevated: install, service start, `/healthz`, secure data dir (`secure-datadir.ps1`, `dacl-check.ps1`, `service-sid.ps1`), upgrade over a previous MSI, uninstall and `REMOVE_DATA=1`. It checks the unsigned MSI's SmartScreen/Defender experience (owner §2.23: no code signing) and the firewall rules on a Server SKU.
2. **Bench at the spec's scale on a clean 4-core machine (matches the §3 "4 cores" spec):** CI only gates `search-bench` at 200k and `size-report` at 50k, so the 10M-torrent p95 table has never been measured on spec-like hardware. Run `size-report --gate`, `search-bench` at 200k (to compare with CI), 1M and 10M (needs the larger EBS volume, up to 100 GiB), `relevance --gate`, and the 17-minute `reporting-overhead` run. Report p95 against the §3 table. Repeat three times and take the median; flag any run with CPU steal. EBS is not NVMe, so label results "EBS gp3, not NVMe". Also run `filter-budget` (50 µs/torrent), which CI calibrates to an EPYC 7763.
3. **Browser UX against Main over the internet:** Lighthouse CI (`web/lighthouserc.json` mobile: LCP ≤ 2500 ms, TBT ≤ 100 ms, CLS ≤ 0.02, ≤ 100 KB; `lighthouserc.desktop.json`: LCP ≤ 1000 ms) and the Playwright suite (`fixture` project via `E2E_FIXTURE_URL`, plus `core`) from Win to Main's UI (cross-provider, real latency). Playwright is Chromium-only in the repo today, so Edge and Firefox are exploratory checks, not gates; throttled 4G profile for LCP < 2.5 s, INP < 100 ms, CLS < 0.02; 10k-row virtual list scroll at 60 fps; SSE insert batching while Main is ingesting; drawer prefetch on hover; the file tree Web Worker for torrents with more than 2k files. Use a **crawl-grown** database on Main (real names, real file trees, real Unicode and CJK), which the synthetic corpus can't give.
4. **Windows crawler soak (optional, only if time remains):** `cr0-soak.ps1` for 6 h, as a short A/B arm against the same build on Main, to see Windows versus Linux yield in the same hours (different IP, so a lead only).
5. **Windows CI flake reproduction:** run the fixed-sleep and timing-sensitive tests in a loop on Win (the slowest platform the CI uses) to find flakes that don't show on Linux runners.

## Cross-lane experiments that need concurrency
| Experiment | How the three VMs help |
|---|---|
| Time-of-day bias | Run the same build on Main and on the Worker (low-resource mode) in the same hours; the ratio of yields shows how much of a change is the hour, not the build. |
| Linux vs Windows, same build, same hours | Main (Linux) and Win (6 h `cr0-soak.ps1` arm) simultaneously. Different IPs, so treat as a lead. |
| Search while ingesting | Win drives UX and API load against Main during its crawl; Main reports ingest rate and writer lag while Win reports latency. |
| Failure drills | Kill Worker mid-lease, drop Main's PIA tunnel, restart Main's container: all while the other lanes run, verifying the funnel reconciles and nothing leaks outside the tunnel. |
| Low-resource ceiling | Worker (1 GiB) versus Main (8 GiB) with the same config shows the memory-versus-yield curve before we set low-resource defaults. |

## Timeline (hours from start)
| Hour | Main | Worker | Win |
|---|---|---|---|
| 0 to 2 | Phase 0 canary, leak test | Phase 0 canary, leak test | Phase 0 canary; MSI smoke |
| 2 to 3 | L1 smoke; start crawl; start collector | Start low-resource crawl; collector | Bench gates (3 repeats) |
| 3 to 10 | Suppression (6 h) then warm-up | same | Bench 10M run; reporting-overhead |
| 10 to 20 | Measured window | Measured window | Lighthouse/e2e against Main (growing store); flake loops |
| 20 to 32 | Window continues; L4 restart drill | Drills; optional worker lane | Optional 6 h Windows arm |
| 32 to 40 | Collect and analyze; destroy | Destroy | Final UX run on the largest store, then destroy |
| 40 to 48 | Spare buffer; nothing is meant to run | | |

Main must start the crawl by hour 3 for the first window to fit well inside 48 h (6 h suppression + 1 h warm-up + at least 6 h window = 13 h). A full 24 h window (about 31 h in total) just fits but leaves no second round; prefer one 12 h window plus one toggle round if a treatment is ready.

## Deliverables (when approved and run)
- `docs/plan/2026-11-08-cloud-test-plan.md` (this plan, in repo form) and a runbook under `docs/operations/` or `docs/soak/` (provision, leak test, teardown, evidence copy-off).
- Per run: `docs/soak/evidence/<date>-cloud-<lane>/` (aggregates, `analysis.md` from `analyze_cr0.py`) and a `docs/decisions/<date>-cloud-pilot.md` that records results, anything that touches an owner number (§2.26, flagged per §2.19) and the follow-up slices.
- Anything that changes an owner-set number is recorded in the decisions file and flagged (§2.19, §2.26). Anything weakening a security or content-safety rule goes to the owner first. Open PRs follow `.claude/skills/steward/SKILL.md`; one PR per slice, titled `<id>: <title>`.
- Slice entries go through the runner (`runner/slices.yaml` is its to edit), so findings land as slices, not as ad-hoc PRs. Branch `claude/bold-davinci-9yq7s5` for the docs PR.

## Verification of the plan itself (what "done" looks like)
1. Both watchdogs deleted their canaries and emailed; the manual cleanup steps are written.
2. The leak test shows zero egress outside PIA with the tunnel down.
3. Every lane has evidence that `analyze_cr0.py` reads (or a bench/Lighthouse report) with commit and binary SHA recorded, and a verdict of "lead" or "regression" with the reason.
4. All three VMs are confirmed deleted (EC2 console and Linode API) with no orphaned firewalls or volumes.

## Decisions needed from the owner
1. **May cloud crawl runs count as CR-SPEC evidence?** Recommended: no, exploratory only; add a decisions file so everyone reads cloud numbers as leads.
2. **Windows c6a.xlarge cost** (my ~$15 estimate for 48 h): confirm the AWS budget.
3. **Which candidate treatment should the first Main toggle round test?** For example sampling-node provenance, `dht.target_prefixes`, or concurrency tuning.
4. **Is PIA port forwarding available on the chosen exit region?** Inbound UDP depends on it.
