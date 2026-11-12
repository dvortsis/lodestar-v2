# Master-plan update r4 decisions (2026-11-12)

The owner asked for a review of the completed and running work against the plan, and a plan for everything that remains. The review compared `docs/build-plan.md` §0 (r3) and `docs/handoff.md` with `main` at `f47369b8` (2026-11-12T22:58Z), `runner/slices.yaml`, `runner/dashboard.yaml`, `state.json` on `runner-state`, the open pull requests and the code. Docs only. This file follows `2026-11-02-master-plan-r3.md`; it doesn't supersede r3's refinements, which all stand.

Nothing here reverses or loosens an owner decision, weakens a security or content-safety decision, or changes an owner-set number.

## Owner answer in this session
- **The r4 ladder is approved** (build-plan §0.2), 2026-11-12. The owner approved the plan that sets it. It replaces r3's seven rungs, four of which were finished. The runner records it in `docs/owner-decisions.md` with the decisions made since §2.60.

## What the review found
- **The build had outrun the plan.** §0.1 still showed v0.5.1 as untagged, workers, companions and M6 as held, Companions as having no code, the store at V13 and search at schema 5. In fact v0.5.1 was published on 2026-11-07, all three epics were unheld on 2026-11-06 and are mostly built, the store is at V14 and search at 7.
- **Counts at the snapshot:** 588 slices: 438 merged, 19 in progress, 120 not started, 4 closed, 7 epics. Of the 120, 10 are ready, 7 wait only on an outside condition and the others wait on unmerged slices. 21 slice PRs are open.
- **The code has no stubs.** No `todo!()`; every `unimplemented!()` is in a test double; 3 TODOs. What's unfinished is deferred on purpose and says so (build-plan §0.1).
- **Building is not what decides the date of 1.0.** The crawler gate (12.3/min against 100/min, no A/B run yet), one Unraid box serving four exclusive queues, and the owner's actions are.

## Runner-level refinements (flagged under §2.19)
- **§0.1, §0.3 and §0.9 are snapshots that point at live sources.** They state their commit and time, and name the dashboard and `runner-state` as the live view. `docs/handoff.md` §8 no longer keeps a dated list. A dated list in two documents went stale in a day, twice.
- **The track tables keep their r3 text.** They record how each track was planned and what each step's exit was. §0.2 adds one line per track saying where it stands, so the history isn't rewritten.
- **Rungs 3 to 8 run in parallel.** v0.7, v0.8 and M8 slices that wait on no gate start now, because slots are no longer the constraint and the gates at the end of each are. The ladder still decides between ready slices.
- **Migration order: WK-12, then SRC-3b, then CR-3.** WK-12 is on the v0.6 path and four worker slices and WK-G wait behind it; SRC-3b is v0.7; CR-3 already waits for the 50M gate. This is the runner's to assign (`ext:migration-slot`); r4 recommends it.
- **Host order from here:** crawler A/B blocks first, an import pilot in each gap, the v0.5.2 soak when its rc exists, the 1.0 soak and crawler re-measure last. This applies §0.7's exclusive-booking rule; it changes none of the run rules in §0.5.
- **v0.5.2 gets its own rung and its own dashboard step.** The cut is done but the roadmap ends there: no rc is tagged and nothing tracks the gate or the tag.
- **§0's heading loses its date** ("Status and plan"), so the anchor doesn't have to change with each revision. The two links in `docs/handoff.md` are updated.

## For the owner
- The owner queue, in the order it unblocks work, is build-plan §0.3. The first three are: book CR-AB-ADM on the experiment host; do the M6 manual installs; tag v0.5.2 once its gate passes.
- **A choice worth making:** whether import pilots may run on a host other than the Unraid box. It would remove the largest calendar conflict. It changes §2.52, so it isn't decided here.

## For the runner (shared files this PR doesn't edit)
CLAUDE.md gives these to the runner alone. They are sent to the runner by message as well.
1. **`runner/slices.yaml`: add entries** for work that has a slot or an open PR but no entry: M8-PERF-1a (#780), X-ci-main-signal (#784), X-test-blockers-1112 (#778), X-win-soak-attach-proc-hash (#792), X-test-vm, P-v052-cut-rc1-next, and X-un1-nits (merged as #769).
2. **Mark done:** CR-AB-COMP (the comparator was accepted at 317.8 stored/min, but the slice is still not started), X-tokio-1541-v052 (#740, on `release/v0.5.2`), WK-10d-store (#604).
3. **`docs/owner-decisions.md`:** record the decisions the slots cite as §2.61 to §2.64, which the file doesn't have yet, and this ladder. The window's end is given as 23:30Z in one place in `state.json` and 00:00Z in another.
4. **`runner/dashboard.yaml`:**
   - phase badges: `ph-platform` says "Held" and `ph-m7` says "Next", and both are being built;
   - the note labelled "Four slots", whose text says eight; `notes.updated` is 2026-11-08;
   - owner-queue item 1 ("before CR-4 lands"; CR-4 merged on 2026-11-02) and the note that the comparator is booked from 2026-11-07;
   - the roadmap text for `tag` (D-v051-main merged as #574) and `unhold`;
   - a roadmap step and a gate for the v0.5.2 rc and tag;
   - the owner queue in the order of build-plan §0.3, if the runner agrees.
5. **`state.json`:** about 190 slices that `main` shows merged have an old or empty status; `open_prs`, `review_queue`, `queue_note`, `release` and `plan_ref` date from early November; the epics still read held or pending; a note says CMP-12 needs an owner answer that has been given.
6. **Deferred items with no slice confirmed** (build-plan §0.1): check each against the graph and add a slice where none covers it. Also: the header comment in `crates/lodestar/src/update/windows/mod.rs` still says install and rollback aren't built; `docs/releases/v0.5.2.md` is a first draft; #632 has had no activity since 2026-11-09 and no slice owns it.
