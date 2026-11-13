Verdict: CHANGES

Review of #812 at `66acce1b`, one commit on `400cc479`, checked against `main` at `a553fd1b` (#809 merged). Docs only: `docs/build-plan.md` and `docs/decisions/2026-11-13-master-plan-r4-reviews.md`. I didn't fetch the PR body itself (that needs the GitHub API); I checked the summary the coordinator passed on. Files were read only, with no GitHub or cloud calls. #809 touched no file in this diff, so the PR still applies cleanly on `main`.

**B1 holds.** I read every added line and the commit message. Each finding the code review rates as security or content safety is named by id, severity and surface only:
- the security ones: DHT-01, PIPE-01, SEARCH-01, STORE-01, FILTER-01, APIFEAT-01, UPDATE-01, UPDATE-05, CMP-01, SRC-02, INFRA-01, INFRA-02 and INFRA-04;
- FILTER-02 and FILTER-03, which the review rates "gap" but which sit in content safety.

Owner rows 13 to 15 read as choices between fixes, with the detail held. INFRA-01 keeps only what the public workflow shows. The plain-language descriptions left (LOGSET-01, SEARCH-02, the notifications gap, WEBCORE-01, the test-net rows, the UI items) are all findings the review rates correctness, gap, test or docs. The decisions file, the §0.10 history line and the commit message say nothing about how any flaw works. The instruction to bring the code-review folder to `main` is gone.

The one thing left from B1 is a pointer, and it came from my own earlier suggestion (I1 below).

## Blocking
None.

## Important

**I1. `main` would point to where the held detail is public. My B1 fix 4 asked for this, and I was wrong to.**
- **Where:**
  - `docs/build-plan.md:254`, row 20: "delete the code-review branch `claude/happy-hypatia-i314gf`, which already carries that detail publicly";
  - `:92`: "the review folders stay on their branches (`claude/happy-hypatia-i314gf` for the code review, …)", right after the sentence saying held findings are kept out of this repository.
- **Why it matters:** together these tell any reader of `main` which branch holds the unfixed detail. That is the one thing `docs/security/v1/README.md` §5 is there to prevent. The row already says the runner gives the owner the detail directly, so the branch name can go the same way.
- **Fix:**
  1. Row 20: "Open draft security advisories for the held findings, then delete the review branch that holds their detail (the runner gives you both directly)."
  2. Line 92: drop the code-review branch name ("the code-review folder stays off `main`"). The UI review's branch can stay named, since it holds no held detail.
  3. The decisions file names only the UI branch, so it needs no change.

**I2. FILTER-02 no longer has a route.**
- **Where:** `docs/build-plan.md:98`, the content-safety plan cell: "… The rest wait on the owner's choice between fixes (§0.3, 13, 14, 15)".
- **What's wrong:** the row lists FILTER-01, -02 and -03, the three restore findings, APIFEAT-01 and SRC-02. The restore fix is a runner call, and SRC-02 is on the veto list. Rows 13 to 15 are FILTER-01, APIFEAT-01 and FILTER-03. So "the rest" includes FILTER-02, but no row asks about it. In #810, its fix went ahead with no owner answer: it makes an existing locked-floor rule able to fire, so it is a runner call under §2.33.
- **Fix:** "FILTER-02's fix goes ahead as a slice (§2.33). SRC-02 is on the veto list. FILTER-01, APIFEAT-01 and FILTER-03 wait on the owner's choice between fixes (§0.3, 13 to 15)." If you would rather flag FILTER-02 too, add it to the veto list.

## Optional
- **O1. Mark one more id as not yet sliced.** X-consent-gate-lift-external (`:255` and `:257`) is a follow-up named in #807's decisions file and has no `runner/slices.yaml` entry. Mark it like the other three ("not yet in `runner/slices.yaml`").
- **O2. Two severities.**
  - CMP-01 (`:100`) has no severity. Its reviewer rated it blocking, and the second read re-graded it to important. Write "important, companions, security".
  - APIFEAT-01 (`:98`) is shown as "important". Its reviewer rated it blocking, and the second read re-graded it to "needs an owner decision" without giving a new severity. Under §3 of the method, important security findings are treated like blocking ones, so nothing changes in practice. "blocking, re-graded to an owner decision" is the exact wording.
- **O3. Make row 21 a request.** As written it reads as a topic. Use "Approve or change the proposed replacement sentence about the torrent cache in the Legal Notice (§2), which gates X-consent-gate-lift-external".
- **O4. Cosmetic.** There are two blank lines at `:118-119`, before "Each track below…". Make it one.

## Checks of the earlier findings
- **B1:** done, except the pointer (I1).
- **I1 (restore scrub):** done.
  - It is a §2.33 runner call on the veto list (`:257`), with UPDATE-05 and Windows named.
  - The citations are correct: `2026-11-12-X-win-upd-manual-rollback.md` follow-up 1, and SRC-14b's purge rule (#400).
  - The "13 to 17" range is gone. FILTER-02's route was lost on the way (I2).
- **I2 (TEST-05):** moved to the veto list as a §2.26 call. Done.
- **I3 (SRC-02):** the premise is now "before X-consent-gate-lift-external, the Account r2 or the first Restricted or Overlay plug-in", and the item is on the veto list (keep or veto). Done.
- **I4 (v0.5.1.1):** the sentence is dropped. INFRA-03 now reads "matters for any patch on an older line once a newer version is out". Done.
- **I5 (no interim rule):** row 19 and the hostile-input row now say the runner places these slices like any new fix slice (§2.33) until the owner answers. Done.
- **I6 (restating §2.66):** Track U step 3 points to "§2.66 items 1 to 3", and the §0.3 lead-in points to §2.66. Neither restates the "Group versions" fall-back. Done.
- **O1:** §0.9 item 10 and the decisions file say X-watch-stall-retry and X-utp-spurious-rto aren't in `slices.yaml`. Done.
- **O2:** INFRA-09: "no slice adds one; M8-SEC-5 reviews dependencies by reading only". Done.
- **O3:** INFRA-04 named by id. Done.
- **O4:** `docs/security/v1/README.md` §5 cited throughout. Done.
- **O5:** the §0.10 line lists every changed part, §1.2 included. The decisions file points to it. Done.
- **O6:** line 47 says "launched 2026-11-13T03:15Z". The relay attribution matches #809's file. Done.
- **O7:** row 21 added, and the closing sentence says "13 to 21". Done (see O3 for wording).
- **O8:** INFRA-01 is concrete in Track Q, row 16 and §0.9 item 3, and says "the workflow at the tagged commit". Done.
- **O9:** #809 has merged, so §2.66 and `2026-11-13-runner-ui-review-decisions.md` now resolve on `main`. Done.
- **New row 20:** right idea, wrong place for the branch name (I1).
- **New row 21:** right (see O3).
- **Ids:** every slice id is in `runner/slices.yaml` on `main`, or marked as not yet sliced (X-lease-reconcile-every-session, X-watch-stall-retry, X-utp-spurious-rto, ENR-13b), except X-consent-gate-lift-external (O1).

## Hygiene
- No 12-digit numbers: `git diff origin/main...HEAD | grep -nE '[0-9]{12}'` finds nothing.
- Times: 00:00Z, 03:15Z, 18:56Z and 22:30Z, all UTC with Z. No time zone or place is named.
- No model identifiers in the diff or the commit. The two "Opus" hits are the repository's policy tier on existing lines. The trailer is plain `Claude`.
- No personal data.
- Every `docs/`, `runner/` and `web/` path cited exists on `main`.

## For the runner (dashboard.yaml)
Not a finding against #812. This is rebased on `main` at `a553fd1b`. #809 changed only `placement`: the four UI slices are in ws-settings, ws-insights, ws-web and ws-src-ui (lines 277, 294, 320 and 512). `notes.updated` is still '2026-11-08'. `owner_queue` has 12 entries. Nothing in either file may carry held detail, only ids, severities and surfaces.
1. **`owner_queue`:** add, renumbering from 13:
   - one `type: decision` entry per row 13 to 21, each with `unblocks` and `when`:
     - rows 13 to 15: "a choice between held fixes; the runner gives you the detail";
     - row 16: `when` is "before the v0.5.2 tag", `unblocks` the v0.5.2 tag;
     - row 17: unblocks Track U step 6;
     - row 18: unblocks the v0.6 tag;
     - row 19: the ladder order for the hostile-input fixes;
     - row 20: as an `action`, with no branch name (I1);
     - row 21: unblocks X-consent-gate-lift-external;
   - one `type: review` entry (§2.19) for the three runner calls on the new veto list: the restore paths, SRC-02 and TEST-05. Alternatively, append them to the existing entry 11.
2. **`conditions`:** an `ext:` entry (`who: you`, `met: false`) for each answer a fix slice's `deps` will name. Candidates:
   - FILTER-01, FILTER-03 and APIFEAT-01 (rows 13 to 15);
   - the INFRA-01 image gate (row 16);
   - the part-4 proposals (row 17, for Track U step 6);
   - the torrent-cache sentence (row 21, for X-consent-gate-lift-external).

   Check whether the v0.6 tag question (row 18) already has a condition before adding one. `npm test` fails on an `ext:` with no entry.
3. **`roadmap.steps`:** two new steps.
   - **Track U, "UI review fixes":** `members` X-wizard-labels-signout, X-sources-restructure, X-ui-guards and X-insights-funnel-fixture (all on `main` now), plus the I1+I3, B3 and L1/L2 slices once they're in `slices.yaml`.
   - **Track Q, "Code-review fixes":** `members: []` until sliced. X-lease-reconcile-every-session, X-watch-stall-retry and X-utp-spurious-rto need `slices.yaml` entries first. A step naming a missing slice fails `npm test`.
   - **Placing them:** every row has free cells. Row 1 is empty, and row 4 has free columns from 4 to 8. For example, Track U at (4, 4) and Track Q at (4, 1).
   - **`roadmap.arrows`:** for example `[<track-q>, m8]`, `[<track-q>, v1]` (G-v1-security) and `[<track-u>, m8]` (M8-DR).
4. **`placement`:** for the Track Q slices, either rules in the existing area workstreams (ws-filters, ws-api, ws-workers, ws-companions, ws-m6, ws-m8-release, ws-web) or one new workstream, such as `ws-review-fixes` under `ph-polish` or `ph-v10`. Keep ids and `owns` at component level per §5 of the method.
5. **`gates`:** no new gate. Widen G-v1-security's `plain` and `target` to cover Track Q's blocking and important findings, matching the `slices.yaml` scope change (decisions-file runner step 4). There is no v0.5.2 entry in `gates`, so put the INFRA-01 check in the `v052` roadmap step's `plain`, or in a `notes.now` item.
6. **`notes`:**
   - Set `updated` to the date of your PR.
   - Add `now` items for "B1 and B2 first (§2.66)" and "Review fixes: Tracks Q and U, held findings named by id only".
   - Add a `you` item for the new decisions and the advisories.
7. Run `cd tools/status-connector && npm test`. The CI warning that `build-plan.md` §0 changed without `dashboard.yaml` is expected on #812, and your PR clears it.
