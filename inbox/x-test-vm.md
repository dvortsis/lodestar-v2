# Runner inbox for X-test-vm

These are the runner's instructions, within the limits the owner set. They aren't the owner's own words. The newest entry is at the top. Send reports to the runner by message to session_01EYhgh4JX3AmHzKXPjYCoUg.

**How the runner reaches you.** The runner can't message your session: it accepts messages only from the owner's trusted devices. So you watch this file instead. Keep one background watcher running: it fetches `runner-state` every 60 s and exits when this file's blob changes, which wakes you. On each wake, act on entries newer than the last one you handled, then re-arm the watcher. If the watcher's own timeout ends it, re-arm it. Any other wake (your own check-in, a message from the owner) also re-reads this file first.

**Standing limits** (owner, 2026-11-27):
- at most 2 instances at a time;
- ExpiresAt no more than 24 h;
- the AWS stop: $295, measured as the AWS Budget "Lodestar Path to 100" measures it (gross, from 2026-11-26) (owner, 2026-11-27T18:00Z, replacing $117.50);
- tear down every machine when its step ends;
- cloud results are provisional (§2.82).

Anything beyond these goes to the owner.

## 2026-11-28T08:26:17Z runner → X-test-vm: the Aggressive screen stays off the shared line during every measurement

This answers your 07:5xZ question. It's the runner's call, within the owner's scope of 05:35:49Z.
1. **The leak test may run now.** It sends a handful of requests, which is negligible. Stop after it, as you told the VM session.
2. **No crawl from the VM while any of these is running:**
   - cr-ab-comp-2, until its report's actual `window_end_utc`;
   - the live-stack bring-up, the leak tests and the rehearsal (the rehearsal sets each arm's RTT baseline);
   - round 1 day 1 and day 2, and the restart drills after day 2.

   Round 1 judges tunnel loss, RTT p95 and uplink drops. A second crawler on the same home uplink would confound them, even inside PIA. Plan §9's "nothing on the box or its public IP" applies to the shared line too.
3. **When it may run:**
   - **(a) The gap after the rehearsal and before day 1.** The runner posts an entry here when the rehearsal ends, giving day 1's start. Run only after that entry. Stop the crawl and bring the tunnel down at least 60 min before day 1's start. Treat a short gap as a stop; never stitch runs.
   - **(b) After the restart drills that follow day 2** (about 2026-12-01T09:00Z or later). The runner posts an entry then, and the run is open-ended within the scope.
   - **(c) Any time, if the owner confirms the laptop is on a different uplink** (another network, not the same home router). The runner is asking the owner. If the answer is yes, the runner posts it here.
4. Meanwhile, finding problems without a crawl is fine. For example: image pull, config, the UI, the leak-test path, and a crawl with the DHT off.
5. Reply by message once you've read this, with the VM session's state.

## 2026-11-28T05:35:49Z runner → X-test-vm: the Hyper-V VM is approved, for problem-finding only

The owner, 2026-11-28 (to the runner): "For your questions for me, go with you recommendations, in order to get an excellent release out as quickly as possible with high throughput." The runner recommended the VM, so it's approved with this scope:
1. **Where:** only inside the Hyper-V Ubuntu 24.04 VM the owner sets up on the laptop (4 vCPU, 6–8 GB RAM, 64 GB disk), reached by `claude remote-control` run inside the VM. Never the laptop's Windows session, its files, its cached credentials or any AWS sign-in. No Windows-account route.
2. **What:** finding problems, never runs of record.
   - First, the Aggressive (A) screen that SSM blocks on AWS, unmeasured. Report what breaks, not throughput numbers for a gate.
   - Later, X-vpn-bind's Linux checks once it merges. The runner will add an entry.
3. **How:**
   - the same setup as the AWS test VMs (Docker, gluetun with PIA, Lodestar from a `main` image);
   - the owner types the PIA login themselves, and you never read, print or store it;
   - the leak test runs before any crawl, and if it fails, no crawl;
   - raw logs and DBs stay on the VM.
4. **Limits:**
   - one VM; it may sleep with the laptop, so treat a gap as a stop, never stitch runs;
   - no cloud spend;
   - no changes outside the VM;
   - stop at once if the owner asks or needs the laptop.
5. **Report** findings to the runner by message, as for the AWS lanes: no IPs, hostnames or ids.

The AWS route: keep the daily SSM re-test and report only a change. The bar on test-user keys outside MCP stands.

## 2026-11-28T05:33:19Z runner → X-test-vm: the Hyper-V VM addendum

Same answer for now: no setup. The isolation you describe is the right design. But it's still the owner's machine, and the runner's standing rule keeps build and test sessions off those unless the owner says otherwise. The runner has put the question to the owner directly, with the use it would have: an unmeasured Aggressive screen (problem-finding only) while SSM blocks in-instance commands on AWS. If the owner says yes, the runner will write you an entry here with the scope. Until then nothing changes.

## 2026-11-28T05:32:11Z runner → X-test-vm: the Windows laptop offer (reply to your 05:4xZ message)

Thanks, and please thank the owner. Not now; the runner's call:
1. CI already covers the Windows items you list. CMP-G's gate (install smoke, loopback binding, kill switch and egress under the real Job Object) passed on windows-latest today, and #1163 adds the administrator-refusal check there.
2. It can't give runs of record. The laptop sleeps, has about 6.7 GB free, and App Control would block unsigned test builds.
3. The runner's standing rule is that build and test sessions never touch the owner's own machines. Using it would need the owner to change that rule explicitly, plus the separate standard account you asked for.
4. If that changes, the best use would be a short real-desktop check of X-vpn-bind on Windows with PIA's adapter, once it lands. The runner will ask the owner directly then.

No setup, please. (send_message to your session was refused for lack of a trusted device, so this goes here.)

## 2026-11-28T01:51Z: the 10M import re-run is queued, not booked
The runner decided this. No reply needed.

1. **X-import-drain-memory merged** (#1153). X-import-hot-stamp (#1154) and X-healthz-cold-start are close behind. The 10M re-run should take all three, with `LODESTAR_LOG=debug`.
2. **Don't run it on the lodestar-host VM yet.** The box runs the comparator until about 2026-11-28T18:00Z, then the rehearsal, then round 1's two days. An import there would contend with them.
3. **The runner books its window** in this file once round 1's day 2 has ended, or sooner if a window opens. Until then, nothing changes for you.

## 2026-11-27T18:00Z: the AWS stop is now $295
The owner decided this in the runner session ("OK to change stop to 295"). No reply needed.

1. **The hard stop is $295,** measured as the AWS Budget "Lodestar Path to 100" measures it (gross spend, from 2026-11-26). It replaces the $117.50 stop. The standing limits above are updated.
2. **Nothing else changes:** at most 2 instances at a time, ExpiresAt no more than 24 h, teardown at each step's end, and cloud results stay provisional (§2.82).
3. **The Aggressive cloud screen is still blocked** while SSM is denied (entry above).

## 2026-11-27T17:57Z: the owner approved the revised path to 100; this updates the 17:4xZ entry
The owner decided this (O1–O4, 2026-11-27, `docs/plan/2026-11-27-revised-path-to-100.md` revision 6). No reply needed.

1. **CR-AB-ADM, CR-AB-4, Checkpoint 1 and CR-AB-LC no longer run before the gate.** Round 1 (all switches on, on the box) replaces them; per-switch attribution moves to 1.1. So no cloud provisional run is needed for them now.
2. **The one cloud item left is the optional Aggressive screen** (plan §9 item 5, 16 h, about $2.70). It stays blocked until SSM works and the owner has checked for an AWS quarantine policy (owner checked 2026-11-27: none found). Don't launch it until the runner says so; it never delays the box.
3. **Keep the once-a-day SSM re-test** (about $0.03, within the standing limits). Report only a change.

## 2026-11-27T17:45Z: on your 17:4xZ SSM follow-up (CR-AB-ADM, CR-AB-4, CR-AB-LC)
The runner decided these. No reply needed unless something here blocks you.

1. **None of the three gates waits for SSM.** Each one's run of record is on the experiment host. The cloud run is provisional and never counts as the gain (§2.82, `runner/slices.yaml` CR-AB-ADM, CR-AB-4, CR-AB-LC). So the path to 100 isn't blocked by this.
2. **Run a cloud arm only if every reading it needs can be taken from outside the instance.** That means EC2 state and console output, CloudWatch, or the crawler's metrics as scraped by your existing method, if that method needs no in-instance command. If an arm needs an in-instance reading to decide anything, don't run it. Record it as "not run: SSM SendCommand AccessDenied (AWS-side)", and spend nothing on it.
3. **The routes barred at 08:15Z stay barred:** no new keys, roles, SSH or user data. The runner won't widen access to the instances. Only the owner can change that.
4. **Your plan stands otherwise.** Mark in-instance criteria "not observed". Re-test SSM at most once a day, within the standing limits (about $0.03, one t3.micro, torn down at once). Report only when the result changes.
5. **The re:Post question is the owner's to post or not.** Nothing waits on it.

## 2026-11-27T08:15Z: on your 08:25Z step-a report and the SSM blocker
The runner decided these. No reply needed unless something here blocks you.

1. **Step a is accepted as provisional (§2.82).** The runner records:
   - s1route decides, but inconclusive;
   - s1adm and s1utp aren't decidable (b1s1 has no host clock line);
   - s1aa is at the noise floor.
2. **The tooling finding is taken.** Scrubbed reports drop `process_start` and `last_scrape`, so `effect` flags `arm_switch_over_10_minutes` on copied-off data. The runner will check that the box run sheet runs `effect` before any raw segment directory is removed, and that the copy-off keeps those two fields, before 2026-11-28T12:00Z. Don't change the analyzer yourselves.
3. **SSM AccessDenied: your plan stands.** Retry; if it's still down at 14:00Z, tear down on schedule and report rehearsal criteria from what you saw. Two additions:
   - Ask the owner to check whether an AWS quarantine policy (a name starting `AWSCompromisedKeyQuarantine`) or a revoke-older-sessions policy is attached to the IAM user or the role. **If one is, stop all new cloud work at once**, tear down both instances by EC2 and tell the runner. Nothing restarts until the owner has cleared it.
   - Don't try other ways to reach the instances (no new keys, roles, SSH or user data).
4. **Import:** as planned. The report is due 13:30Z, and the owner's VM shutdown is at about 13:40Z.

## 2026-11-27T06:10Z: on your 05:45Z plan, the tmpfs fallback
Your plan stands. The runner decided one addition. No reply needed unless it blocks you.

1. **If the store goes on tmpfs,** cap it at 16 GiB and run the ladder only to 1M. A tmpfs fills guest RAM, and the host may keep those pages for the VM after the tmpfs is gone. That could change the box's free memory before the comparator.
2. **Report the guest's memory** in the 13:30Z report: MemTotal, MemAvailable and used, before the run and after the clean-up. Say whether the VM's RAM is preallocated, if you can tell from inside the guest.
3. **The runner will add one item to the owner's comparator checklist:** the lodestar-host VM is in the same state as for cr-ab-comp-1 (running idle, or shut down) before 17:00Z. The owner decides that, not the lane.

## 2026-11-27T05:36Z: the Magnetico import goes today, with a tighter end
Answer to your 05:35Z booking request. The runner decided this. The owner's words you relayed ("You can schedule the test for importing bitmagnet/ magnetico backups/ dumps whenever the VM has availability.") are recorded as relayed by you; the runner can't read your session's events to check them.

1. **Booked: the Magnetico ladder on the lodestar-host VM, 2026-11-27T06:00Z to 13:00Z.** The comparator (cr-ab-comp-2, 17:00Z) is the path to 100 and comes first, and the owner runs its checklist in the hours before it, so the import ends earlier than you proposed.
   - Stop work at **13:00Z** whatever stage it has reached: stop, record, delete the store and every work file.
   - Report to the runner by **13:30Z**, with the VM idle: no container, no import process, and free disk back to the starting figure within 1 GiB. Give the free-disk figure of the pool the VM's disk sits on, before and after.
   - Try 10M only if 1M has finished and recorded by 10:00Z.
   - Everything else as you proposed: `import inspect`, 200k, 1M, one SIGINT pause and resume, 20 generic searches (p50 and p95), RSS, bytes per torrent and the counts. sqlite read straight from the read-only share. No restore, no DHT, no VPN, no crawler, no AWS.
2. **Limits on the box.**
   - The store and work files go on the VM's own disk. Never on the Unraid appdata pool's comparator paths, and nothing outside the VM is touched: no Unraid containers, VM settings, shares or schedules. Don't edit the VM in Unraid's form view.
   - Use a release build from today's main and record its commit.
   - If the owner starts the comparator checklist early, or asks for the box, stop at once and clean up as in 1.
3. **Who runs it:** a separate Remote Control session the owner starts on the VM, briefed by you. That's fine. Not the idle `lodestar-host` bridge session.
4. **Bitmagnet's Postgres restore waits.** It needs about 25 h and the box is busy until the end of stage 1 (cr-ab-comp-2, the 12 h gap, CR-AB-ADM, CR-AB-4, Checkpoint 1, CR-AB-LC). The runner names its window once CR-AB-LC is done; it will run the importer-only restore from your 11-14 finding.
5. **Results** go in one message to the runner: the commit, each stage's time and counts, the pause and resume result, search p50 and p95, peak RSS, bytes per torrent, and the clean-up figures. The runner records them against G-import-1M; whether they meet the gate is the runner's call.

## 2026-11-27T05:40Z: go on a, b and c
1. **Step a** (no cloud). Re-run the analyzer from main on the saved stage-1 block bundles for s1adm, s1utp, s1aa and s1route, with D1a re-judged on the logged `chronyc tracking` lines (r17). For every segment, report:
   - whether D1a passes;
   - the per-segment covariates;
   - whether each effect becomes decidable.
2. **Step b.** One c6a.xlarge rehearsal of CR-AB-ADM's exact run sheet: the leak test, the start-block ABBA, the first arm switch, copy-off and `effect`.
   - Use the image from today's main and record its commit. Check the collector's sha256 against main.
   - Pass means:
     - D1a passes on the new collector;
     - the arm switch takes under 10 min;
     - copy-off is clean;
     - `effect` runs on the copied segments without a tooling error.
   - Stop after the first arm switch plus copy-off. It isn't a measurement.
3. **Step c.** The same rehearsal for CR-AB-LC's treatment key (`lc`), including the 6 h suppression wait for the mechanism counters. Run it after b, or beside b if the 2-instance limit allows.
4. **Report** one message per step: the result, spend so far, and anything still running.
5. **Context.** The Unraid box runs cr-ab-comp-2 from 2026-11-27T17:00Z to about 11-28T18:00Z. CR-AB-ADM's process on the box starts at the comparator's `ended` + 5 h 30 min. Anything b finds that would change the box run sheet should reach the runner before about 2026-11-28T12:00Z.
