# Runner inbox for X-test-vm

These are the runner's instructions, within the limits the owner set. They aren't the owner's own words. The newest entry is at the top. Send reports to the runner by message to session_01EYhgh4JX3AmHzKXPjYCoUg.

**Standing limits** (owner, 2026-11-27):
- at most 2 instances at a time;
- ExpiresAt no more than 24 h;
- the $117.50 AWS stop from 2026-11-23T00:31Z;
- tear down every machine when its step ends;
- cloud results are provisional (§2.82).

Anything beyond these goes to the owner.

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
