# P-cloud-test-plan decisions

The plan is `docs/plan/2026-11-08-cloud-test-plan.md`: concurrent test lanes on the 48 h pilot fleet (one AWS Windows VM, two Linodes behind PIA). Docs only; it starts no VM.

**Nothing here reverses or loosens an owner decision, weakens a security or content-safety decision, or changes an owner-set number.** Every item tightens the plan or follows an existing contract (CR-SPEC, `docs/operations.md` §4.3, handoff §6). The refinements below are the runner's to flag (`docs/owner-decisions.md` §2.19).

- **Cloud crawl runs are exploratory leads, never CR-SPEC evidence** (plan Decision 1, the runner's call under §2.26). Leads are not admitted as evidence because CR-SPEC fixes the acceptance run to one node on one public IP on the experiment host (owner §2.51), and a cloud VM behind a PIA exit is not that host. The CR-COMB acceptance run stays on the Unraid box.
- **CR-ADM runs as one CR-SPEC ABBA block, not as separate toggle "rounds"** (refines plan Decision 3, flagged). The first version gave each round its own fresh start. That costs another 6 h suppression per round and puts control and treatment in different hours.
  - **The block:** resumed restarts (`dht.safe_restart`), the control inside the block, and 6 h segments. It fits by hour 37 and is judged by `analyze_cr.py`.
  - **The cost:** one block fills the session, so **uTP moves to the first block of the next fleet session** instead of a second round in this one. The order (CR-ADM, then uTP) is unchanged.
  - **What it's worth:** one block is half CR-SPEC's minimum, so it yields a lead, never an outcome.
- **Main and Worker run the same block in the same hours, and are analysed apart.** This keeps Decision 3's "the same toggle on every VM in a round". It never pools a 4 vCPU and a 1 vCPU box, and it gives a 1 GiB replicate.
- **Worker runs the shipped Balanced preset.** The server has no low-resource mode yet (architecture §3 describes one; only M8-PERF-1 plans a bench run of it). Worker mode (§19) waits for WK-10e and for a cluster-port path through PIA, which needs an infra change; it gets its own session.
- **Nothing that measures shares a VM or a measured window with a crawl** (tightens ground rule 4).
  - UX load against Main and the failure drills move after the block. Inside it they would have invalidated segments (CR-SPEC 6.5).
  - Win doesn't crawl in this session. It's the measuring box, and a PIA client's kill switch can cut SSM, Win's only way in.
- **The safety net is proven before the fleet, on each watchdog's real rules.**
  - **Akamai:** the watchdog deletes at 48 h after creation and reads no expiry tag, so its canary starts 49 h ahead.
  - **AWS:** the guard acts only on c6a.xlarge in the Lodestar security group, so its canary is made with the real provisioning script and a 20 min `ExpiresAt`.
  - A shorter Akamai test needs a change to the deployed watchdog, which is the owner's decision.
- **Content safety and privacy, tightened.**
  - Browser artifacts (Playwright traces and screenshots, Lighthouse reports) from a crawl-grown store carry real torrent names, so they never leave the VM; only numbers do.
  - VM IPs, instance IDs and PIA exits are scrubbed.
  - The UI is reached only over SSH, with a session-only key for Win.
- **Each Linode gets its own PIA exit.** Two crawlers on one exit would be "another crawler on the public IP" (CR-SPEC 6.5).
- **The collector runs in gluetun's network namespace,** so its public-IP check sees the PIA exit, not the Linode address.

## Flagged for the owner
- uTP moves from "second round" to the next fleet session (above).
- The Windows rate is about $0.34/h with the licence; the provisioning script prints the Linux price ($0.153/h). Teardown by hour 41 keeps the session at about the accepted ~$15; a full 48 h would be about $17.
- Prerequisites in `#623`'s infrastructure:
  - Main's firewall takes one admin `/32`, so Win can't reach Main without a list variable;
  - Win's root volume is fixed at 50 GiB, too small for the 10M corpus.
  - Each is skipped, not worked around, until the infrastructure changes.
