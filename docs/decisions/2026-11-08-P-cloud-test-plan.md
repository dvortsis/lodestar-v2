# P-cloud-test-plan decisions (#627)

The plan is `docs/plan/2026-11-08-cloud-test-plan.md`: concurrent test lanes on the 48 h pilot fleet (one AWS Windows VM, two Linodes behind PIA), and a standing cloud screen for crawler improvements. Docs only; it starts no VM.

**Two items change the owner's answers, and the owner approved both on 2026-11-08Z:** the uTP timing and the cloud screen.

**Nothing weakens a content-safety decision or changes an owner-set number.**
- The one security-relevant change is Prerequisite 2. It widens Main's SSH ingress by one more `/32` (Win's egress), through the infra follow-up slice's own review, and stays SSH-only.
- Everything else tightens the plan or follows an existing contract (CR-SPEC, `docs/operations.md` §4.3, handoff §6).

The remaining refinements are the runner's to flag (`docs/owner-decisions.md` §2.19).

- **Cloud crawl runs are exploratory leads, never CR-SPEC evidence** (plan Decision 1, the runner's call under §2.26). CR-SPEC fixes the acceptance run to one node on one public IP on the experiment host (owner §2.51). A cloud VM behind a shared PIA exit is not that host. The CR-COMB acceptance run stays on the Unraid box.
- **CR-ADM runs as one CR-SPEC ABBA block, and uTP moves to the next fleet session** (owner, 2026-11-08Z, revising their Decision 3).
  - **Why not rounds:** the first version gave each toggle round its own fresh start. That costs another 6 h suppression per round and puts control and treatment in different hours.
  - **The block:** resumed restarts (`dht.safe_restart`), the control inside the block, and 6 h segments. It fits by hour 37.5 and is judged by `analyze_cr.py`.
  - **What it's worth:** one block is half CR-SPEC's minimum, so it yields a lead, never an outcome. The order (CR-ADM, then uTP) is unchanged.
- **A standing cloud screen** comes before the experiment host's A/B blocks (owner, 2026-11-08Z).
  - **Its purpose:** to catch broken or clearly worse changes before they use the scarce host. It never judges a win.
  - **The format:** one ABBA block of 3 h segments on both Linodes in lockstep, collected with `--rehearsal`.
  - **When it stops a change:** only for four predeclared failures (the mechanism didn't engage, instability, a guardrail worse by more than 25 %, a yield loss of more than 25 %, each consistent across both B segments and both VMs). Everything else goes on.
  - **The 25 % margins are new measurement parameters** (§2.26), flagged here. They change only by a decisions file merged before the screen they apply to.
- **Main and Worker run the same block in the same hours, and are analysed apart.** This keeps Decision 3's "the same toggle on every VM". It never pools a 4 vCPU and a 1 vCPU box, and it gives a 1 GiB replicate.
- **Worker runs the shipped Balanced preset.** The server has no low-resource mode yet (architecture §3 describes one; only M8-PERF-1 plans a bench mode). Worker mode (§19) waits for WK-10e and for a cluster-port path through PIA, which needs an infra change; it gets its own session.
- **Measurements never overlap** (ground rule 4).
  - UX load against Main and the failure drills move after the block. Inside it they would have invalidated segments (CR-SPEC 6.5).
  - Win runs one measuring job at a time, and doesn't crawl. It's the measuring box, and a PIA client's kill switch can cut SSM, Win's only way in.
- **The safety net is proven before the fleet, on each watchdog's real rules.**
  - **Akamai:** the watchdog deletes at 48 h after creation and reads no expiry tag, so its canary starts 49 h ahead. A dry-run invocation checks the token and scope on day one.
  - **AWS:** the guard acts only on c6a.xlarge in the Lodestar security group, so its canary is made with the real provisioning script and a 20 min `ExpiresAt`.
  - A shorter Akamai deletion test needs a change to the deployed watchdog, which is the owner's decision.
- **The evidence is made valid by construction.**
  - The collector runs in gluetun's network namespace, without `--require-gluetun`, which CR-SPEC run kinds refuse. `docker inspect` and `run.json` confirm the network parent.
  - Automatic updates are disabled, and both host-checklist items are passed as `excluded` with next-run times, as the analyzer requires in Docker mode.
  - Worker gets storage guardrails that fit 25 GB.
  - An on-VM script makes the arm switches within CR-SPEC's 10 minutes, and an outage watcher writes `link_outage` notes when an outage happens.
  - gluetun and the collector get a lower OOM score than Lodestar.
- **Content safety and privacy, tightened.**
  - Browser artifacts (Playwright traces and screenshots, Lighthouse reports) from a crawl-grown store carry real torrent names, so they never leave the VM; only numbers do.
  - VM IPs, instance IDs and PIA exits are scrubbed.
  - The UI is reached only over SSH, with a session-only key for Win and no agent forwarding.
- **Each Linode gets its own PIA exit.** Two crawlers on one exit would be "another crawler on the public IP" (CR-SPEC 6.5).

## Flagged for the owner
- The cloud screen's 25 % stop margins (above).
- Infra follow-up for the runner to slice after `#625` (the X-cloud-infra session agreed they're feature changes outside it):
  - an admin-address list for Main's firewall;
  - a root-volume size parameter for Win.
  - Until both land, the plan's cross-VM UX steps and the 10M bench are skipped, not worked around.
