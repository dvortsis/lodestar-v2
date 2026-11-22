# CR-SPEC amendment for the path to 100: a faster A/B design (r5.1, #970)

**Decided by:** the owner, 2026-11-20: "Yes, run testing CR-LOOKUP-END and CR-1c together. The plan should be the most efficient. The goal is to get through testing as quickly as possible without sacrificing reliability and/or effectiveness of the testing."

This amends `docs/decisions/2026-11-02-CR-SPEC.md` rule 6.1 (design). It applies to every crawler A/B from the first live block on, and is recorded before any data exists, as §2.25 requires. Everything else in CR-SPEC stands unchanged:
- the measured window, warmups and 6 h segments;
- a fresh data directory per block, with `dht.safe_restart` between arms;
- the cohort, latency coverage and mechanism checks;
- validity;
- the fit (rule 6.2), the +5 % margin and the outcome rules (rule 6.3);
- two blocks (ABBA, then BAAB about 12 h out of phase) before any outcome, and extra blocks in pairs;
- CR-COMB's 24 h gate and the comparator rule.

## A1. The lookup-cost pair is one treatment (owner)
- **The arm:** B has both `dht.lookup_end` (CR-LOOKUP-END) and CR-1c's preset-sized slots on. The pair is decided as one treatment and adopted or left off together.
- **Mechanism checks:** both must act, or the segment is void (rule 5). From CR-LEDGER's series:
  - lookups ending on values are above 0;
  - occupied slots exceed 32 at least once in the window;
  - the query rate stays within the preset.
- **Why:** CR-1c only helps once CR-LOOKUP-END has cut queries per lookup (build-plan §0.5), so testing them apart costs a stage, about 3 host-days, and tells little.
- **If the pair loses or is inconclusive:** the ledger shows which part limited. One more experiment with CR-LOOKUP-END alone is run only if the ledger says it could win, and that choice is recorded.

## A2. Stacked controls from stage 2
- **The design:** from stage 2 on, the control arm has every switch that has already won switched on, at the value it would ship. The treatment adds one more switch, or the A1 pair. A switch that lost, showed no effect or stayed inconclusive stays off.
- **Stage 1 is unchanged:** CR-ADM and uTP each keep the shipped-default control, so Checkpoint 1's `R0` (rule 6.9) stays as defined.
- **Why it is no less reliable:** the block design, its balance and the fit don't change; only the control's switch set does. Each effect is measured where it will ship, which is the quantity the gate depends on. The last stage's treatment arm is then CR-COMB's configuration, so the 24 h run is less likely to fail on an interaction no A/B saw. A false win (true effect about 0) carries forward a switch that does about nothing, and CR-COMB still decides the gate on its own (rule 6.8), as CR-SPEC already relies on.
- **Reporting:** each stage publishes its control's switch set beside its result.

## A3. No idle host between stages
- **Analysis at each segment's end:** the analyzer runs as each segment closes. A stage's outcome is decided the day its second (or fourth) block ends.
- **The next stage is ready in advance:** its image is built, pinned by digest and rehearsed while the current stage runs, so the next block starts within hours. The two treatments of stage 1 run back to back, with no gap.
- **Checkpoint 1 is the runner's (§2.69),** done the day stage 1 is decided. If the projection is short, Checkpoint 1 goes to the owner, but stage 2 doesn't wait: the r5 build is the fix the projection calls for.
- **Rule 1.3's no-peeking holds:** rates are seen only when a window ends.
- **Cloud mechanism checks:** a candidate that fails its cloud-lane mechanism check (build-plan §0.5) is fixed before it takes host time, so host retries go to interruptions, not broken builds.

## Considered and not adopted
- **Shared-control blocks** (ABCCBA, then CBAABC, two treatments against one control). These would save about 1.7 host-days per pair of treatments, but over two blocks the middle arm is never a block's first segment and sits in the middle pair both times. The first-segment effect and a quadratic trend then no longer cancel for it, as ABBA/BAAB guarantees. A third rotated block restores the balance but saves no time. Not adopted, because it trades reliability for speed.
- **Shorter segments, one block before an outcome, reused data directories, or overlapping a block's 6 h suppression with the previous block.** Each removes a protection CR-SPEC relies on: the 6 h window, the degrees of freedom, carry-over control, or one crawler per IP. Not adopted.
- **Folding CR-HINT-FIRST into the A1 pair.** It would save another stage, but a harmful hint path would be hidden inside a winning pair. Not adopted.

## The stages
1. **CR-ADM, then uTP,** each against the shipped defaults. Route backoff runs only if the baseline shows IPv6 no-route events (CR-4).
2. **The lookup-cost pair (A1),** against stage 1's winners. This is CR-AB-LC.
3. **CR-HINT-FIRST, then X-sampler-cooldown-split,** each stacked on the winners so far. The sampler split is also reported on sampler-full time and its segments' worst hours (rule 6.4, reported but not decided on).
4. **CR-1a-S, CR-1a-H, CR-1b,** only those the ledger still justifies, each stacked. A skip is recorded with its reason.
5. **CR-5's profile-led candidate,** if any, as a stacked arm; then **CR-COMB.**

## Calendar
- **Block length:** about 34 h, including its one 6 h suppression and its warmups, so a treatment takes about 3 host-days.
- **Total:** about 16.5–28.5 host-days, against r5's 20–32.
- **With retries:** with the 30 % retry budget, about three to five weeks. An inconclusive treatment costs two more blocks.

## Tooling
**CR-COL-5** adds, in the collector and analyzer:
- per-segment switch sets and the stacked control's identity;
- the A1 pair's joint mechanism check;
- CR-LEDGER's series.

It has passing and failing fixtures and a short rehearsal before first use. Stage 1 needs none of this, so it never waits for CR-COL-5.

## For the owner's veto
A1 is the owner's choice. A2 and A3 are adopted under the owner's instruction above, which asked for the fastest design that keeps reliability. They are listed here so the owner can veto either before stage 2's first block, the first data A2 affects.
