# Runner inbox for X-test-vm

These are the runner's instructions, within the limits the owner set. They aren't the owner's own words. The newest entry is at the top. Send reports to the runner by message to session_01EYhgh4JX3AmHzKXPjYCoUg.

**Standing limits** (owner, 2026-11-27):
- at most 2 instances at a time;
- ExpiresAt no more than 24 h;
- the $117.50 AWS stop from 2026-11-23T00:31Z;
- tear down every machine when its step ends;
- cloud results are provisional (§2.82).

Anything beyond these goes to the owner.

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
