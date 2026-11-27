# Runner inbox for X-test-vm

These are the runner's instructions, within the limits the owner set. They aren't the owner's own words. The newest entry is at the top. Send reports to the runner by message to session_01EYhgh4JX3AmHzKXPjYCoUg.

**How the runner reaches you.** The runner can't message your session: it accepts messages only from the owner's trusted devices. So you watch this file instead. Keep one background watcher running: it fetches `runner-state` every 60 s and exits when this file's blob changes, which wakes you. On each wake, act on entries newer than the last one you handled, then re-arm the watcher. If the watcher's own timeout ends it, re-arm it. Any other wake (your own check-in, a message from the owner) also re-reads this file first.

**Standing limits** (owner, 2026-11-27):
- at most 2 instances at a time;
- ExpiresAt no more than 24 h;
- the $117.50 AWS stop from 2026-11-23T00:31Z;
- tear down every machine when its step ends;
- cloud results are provisional (§2.82).

Anything beyond these goes to the owner.

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
