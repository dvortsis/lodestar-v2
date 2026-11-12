# Master-plan update r4 decisions (2026-11-12)

The owner asked for a review of the completed and running work against the plan, and a plan for everything that remains. The review compared `docs/build-plan.md` §0 (r3) and `docs/handoff.md` with `main` at `f47369b8` (committed 2026-11-12T22:53Z), `runner/slices.yaml`, `runner/dashboard.yaml`, `state.json` on `runner-state`, the open pull requests and the code. Docs only. This file follows `2026-11-02-master-plan-r3.md`; it doesn't supersede r3's refinements, which all stand.

Nothing here reverses or loosens an owner decision, weakens a security or content-safety decision, or changes an owner-set number.

## Owner answer in this session
- **The r4 ladder is approved** (build-plan §0.2), 2026-11-12. The owner approved, in the session that wrote this PR, the plan that sets it. It replaces r3's seven rungs, four of which were finished. `docs/owner-decisions.md` doesn't hold it yet: this file is its record until the runner adds it there, with the decisions made since §2.60 (below).
- **What the approved plan said and r4 corrects.** The plan the owner approved listed the v0.6 tag as needing WK-G, CMP-G and G-m6 as well as the crawler gate. That would have changed §2.25 and P4, which make the tag wait only on the crawler gate, so r4 keeps §2.25: each feature's gate decides whether the feature ships in v0.6. It also put crawler runs ahead of the v0.5.2 soak on the host; r4 puts v0.5.2 first, as rung 2 and §2.58 imply. Both are flagged to the owner in the PR.

## What the review found
- **The build had outrun the plan.** §0.1 still showed v0.5.1 as untagged, workers, companions and M6 as held, Companions as having no code, the store at V13 and search at schema 5. In fact v0.5.1 was published on 2026-11-07, all three epics were unheld on 2026-11-06 and are mostly built, the store is at V14 and search at 7.
- **Counts at the snapshot:** 588 slices. The dashboard counts 436 merged, 19 in progress, 123 not started, 3 closed and 7 epics. Three of the 123 are finished and only need marking (item 2 below), so about 120 are left to start; six of those were ready and not yet in a slot at 22:55Z. 21 slice PRs are open.
- **v0.5.2 is further along than `main` shows.** `release/v0.5.2` holds rc.1 to rc.3 (rc.3 = `833a0d25`) and checked release notes, but there is no tag, `main`'s copy of the notes is a first draft, and `main` records no verdict for the long runs moved to rc.2.
- **The code has no stubs.** No `todo!()`; every `unimplemented!()` is in a test double; 3 TODOs. What's unfinished is deferred on purpose and says so (build-plan §0.1).
- **Building is not what decides the date of 1.0.** The crawler gate (12.3/min against 100/min, no A/B run yet), one Unraid box serving four exclusive queues, and the owner's actions are.

## Findings from the owner's codebase review
A separate session reviewed `main` at `f47369b8` for bugs at the owner's request and found no high-severity defect in what it read. The runner placed its two findings on rung 5:
- **X-watch-stall-retry** (new, Opus): the watch folder holds a Malformed verdict for one more poll, so a copy that stalls for more than 10 s isn't moved to `failed/` for good (`crates/lodestar/src/watch.rs`). It runs before UP-2 and X-watch-env.
- **X-utp-spurious-rto** (already planned after #788): it also resets `timeouts` only on ACK progress, so a peer whose packets acknowledge nothing can't hold a connection open (`crates/utp/src/conn.rs`).

## Runner-level refinements (flagged under §2.19)
- **§0.1, §0.3 and §0.9 are snapshots that point at live sources.** They state their commit and time, and name the dashboard and `runner-state` as the live view. `docs/handoff.md` §8 no longer keeps a dated list. A dated list in two documents went stale in a day, twice.
- **The track tables keep their r3 text.** They record how each track was planned and what each step's exit was. §0.2 adds one line per track saying where it stands, so the history isn't rewritten.
- **Rungs 2 to 8 run in parallel.** v0.7, v0.8 and M8 slices that wait on no gate start now, because slots are no longer the constraint and the gates at the end of each are. The ladder still decides between ready slices.
- **Migration order recommended: WK-12, SRC-3b, PL-8, CR-3.** WK-12 is on the v0.6 path, and WK-13, WK-16, WK-19 and WK-G wait behind it; SRC-3b is v0.7; PL-8 is v0.8; CR-3 waits for the 50M gate. `migration_order`, SRC-3b's note in `runner/slices.yaml` and three plans reserve V15 for CR-3, so this order means releasing that reservation. The numbers are the runner's to assign (`ext:migration-slot`); until it does, V15 stays CR-3's.
- **Host order from here:** the v0.5.2 soak first if it's still owed, then crawler A/B blocks with an import pilot in each gap, the 1.0 soak and crawler re-measure last. This applies §0.7's exclusive-booking rule; it changes none of the run rules in §0.5.
- **v0.5.2 gets its own rung and its own dashboard step.** The dashboard's roadmap ends at the cut, so nothing there tracks the candidates, the gate or the tag.
- **CR-1c is built alongside CR-AB-4 and Checkpoint 1.** It depends only on CR-AB-ADM, so it needn't wait on the path that decides the date.
- **§0's heading loses its date** ("Status and plan"), so the anchor doesn't have to change with each revision. The two links in `docs/handoff.md` are updated.

## For the owner
- The owner queue, in the order it unblocks work, is build-plan §0.3. The first three are: book CR-AB-ADM on the experiment host; do the M6 manual installs; tag v0.5.2 once its gate passes.
- **Two corrections to the plan you approved** are listed under "Owner answer in this session": the v0.6 tag still waits only on the crawler gate, and the v0.5.2 soak goes ahead of the crawler runs.
- **A choice worth making:** whether import pilots may run on a host other than the Unraid box. It would remove the largest calendar conflict. It changes §2.52, so it isn't decided here.

## For the runner (shared files this PR doesn't edit)
CLAUDE.md gives these to the runner alone. They are sent to the runner by message as well.
1. **`runner/slices.yaml`: add entries** for work that has a slot or an open PR but no entry: M8-PERF-1a (#780), X-ci-main-signal (#784), X-test-blockers-1112 (#778), X-win-soak-attach-proc-hash (#792), X-test-vm, X-gluetun-leak, WK-18w, P-v052-cut-rc1-next, and X-un1-nits (merged as #769).
2. **Mark done:** CR-AB-COMP (the comparator was accepted at 317.8 stored/min, but the slice is still not started), X-tokio-1541-v052 (#740, on `release/v0.5.2`), WK-10d-store (#604).
3. **`docs/owner-decisions.md`:** record the decisions the slots cite as §2.61 to §2.64, which the file doesn't have yet, and this ladder. Build-plan §0.2 cites both as not yet recorded. The window's end is given as 23:30Z in one place in `state.json` and 00:00Z in another.
4. **`runner/dashboard.yaml`:**
   - phase badges: `ph-platform` says "Held" and `ph-m7` says "Next", and both are being built;
   - the note labelled "Four slots", whose text says eight; `notes.updated` is 2026-11-08;
   - owner-queue item 1 ("before CR-4 lands"; CR-4 merged on 2026-11-02) and the note that the comparator is booked from 2026-11-07;
   - the roadmap text for `tag` (D-v051-main merged as #574) and `unhold`;
   - a roadmap step and a gate for the v0.5.2 candidates, long runs and tag, and a note of which candidate the gate runs on;
   - the owner queue in the order of build-plan §0.3, if the runner agrees.
5. **Migrations:** decide the order above; if CR-3's V15 reservation is released, update `migration_order` and SRC-3b's note in `runner/slices.yaml`.
6. **`state.json`:** about 190 slices that `main` shows merged have an old or empty status; `open_prs`, `review_queue`, `queue_note`, `release` and `plan_ref` date from early November; the epics still read held or pending; a note says CMP-12 needs an owner answer that has been given.
7. **Deferred items with no slice confirmed** (build-plan §0.1): check each against the graph and add a slice where none covers it. Also: the header comment in `crates/lodestar/src/update/windows/mod.rs` still says install and rollback aren't built; `docs/releases/v0.5.2.md` is a first draft; #632 is the owner's branch and stays with the owner (the runner's standing instruction); the 11 unverified findings from the owner's cloud review of it go to the owner in the runner's summary, the reaper-approval question as a decision.
