Verdict: CHANGES

Review of #810 (P-master-plan-r4, r4.1) at `fb0dca25`, against `main` at `559c1755` and runner PR #809 at `f49b0351`. Docs only: `docs/build-plan.md` and `docs/decisions/2026-11-13-master-plan-r4-reviews.md`.

Verified as stated: §2.62 to §2.65 are now on `main`, so dropping the "not yet recorded" notes is right. §2.66 and `2026-11-13-runner-ui-review-decisions.md` exist only on #809. #779 (`4e19431a`), #781 (`7e826fe4`), #795 (`0db056f9`), #797 (`1256467d`) and #798 (`010a11e3`) are merged. The review counts (20 areas, 203 findings: 16 blocking, 100 important, 87 polish; 153 untracked) match `4f2ccbd9`. Rows 13 to 19 are the code review's own seven "needs an owner decision" items, and row 20 is part 4 §8's eight items. `34cbbafb` is the only commit on either review branch whose trailer names a model. The Phosphor count (124 of 141 unweighted) matches `main`. `LogoutButton.svelte` is imported nowhere. Every slice id is in `runner/slices.yaml` on `main` or #809, or marked as not yet there, with two exceptions (O1). This review restates no finding's mechanism, for the reason in B1.

## Blocking

**B1. The PR publishes details of unfixed blocking and important security and content-safety findings, and tells the runner to commit the full reports to `main`.**
- **Where:**
  - `docs/build-plan.md:101-104`: the Track Q rows for content safety, hostile input, wrong results and supply chain;
  - `:249-252` and `:255`: rows 13 to 16 and row 19;
  - `:445`: §0.9 item 10, "brings both review folders to `main` re-committed";
  - `docs/decisions/2026-11-13-master-plan-r4-reviews.md:20`: runner step 1.
- **What breaks:** §2.47 (OQ6) is owner-approved: "Unfixed blocking or important details are kept in draft security advisories the owner opens". The method M8-SEC-0 merged as #786 says the same in `docs/security/v1/README.md` §5:
  - "Never put the detail in any file on any branch of this repository … a pull request title or body …".
  - "Detail" there means anything that helps someone find or use the flaw: the file, the input that triggers it, the steps.
  - Content-safety findings stay partly held even after the fix.

  Track Q's cells describe how FILTER-01, FILTER-02, FILTER-03, UPDATE-01, STORE-01, PIPE-01, DHT-01, SEARCH-01, STORE-02, CMP-01 and INFRA-01 can be triggered. The first three are blocking findings in the child-safety core, and FILTER-01's cell is close to a recipe for getting past it. The code-review folder adds file and line references and failure scenarios. The repository is public, so merging this, or re-committing the folder, puts that detail on `main`. That weakens a security and content-safety decision, which CLAUDE.md sends to the owner.
- **Fix:**
  1. In Track Q and §0.3, name each blocking or important security or content-safety finding by id, severity and surface only. For example: "FILTER-01, FILTER-02, FILTER-03 (content safety, blocking): fixes held under `docs/security/v1/README.md` §5". Keep the group and plan columns, and drop the mechanisms.
  2. Word the owner questions as choices between fixes, without the flaw. For row 13: "FILTER-01: which of the two fixes in the held detail?"
  3. Replace runner step 1 (decisions file line 20) and the matching words in §0.9 item 10. New text: the runner sends the security and content-safety findings' detail to the owner for draft advisories, per §5 of the method. Only what holds no held detail comes to `main`: the polish findings, the area reports with held rows cut, and the UI review.
  4. Add an owner row. The detail is already public on the branch the code-review branch (named to the owner directly) (`4f2ccbd9`). Should that branch be deleted once the advisories exist? Deleting branches is already an owner action in §0.3 ("delete the branch `wip/CMP-0-catalogue`").
  5. INFRA-01's mechanism can be read from the public workflow, so row 18 can keep its wording.

## Important

**I1. The restore scrub is both "no owner answer needed" and owner question 16, and the reason given cites the wrong decision.**
- **Where:** `docs/build-plan.md:101` and `:252`.
- **The clash:** The content-safety row says the scrub on every restore path (STORE-01, UPDATE-01) "need[s] no owner answer and go[es] ahead as slices". It then says FILTER-01, FILTER-03, APIFEAT-01 and SRC-02 wait for "§0.3, 13 to 17". That range includes row 16, which asks the owner to confirm the same scrub (UPDATE-01, UPDATE-05).
- **The citation:** The row says "SRC-14b already requires every restore entry point to carry holds and deletions". It doesn't. SRC-14b (`docs/decisions/2026-11-02-SRC-14b-2.md`, "From the runner's review (#400)") requires every restore entry point to apply recorded source purges. The rule that a restore must not bring back hashes deleted since, because that "would loosen a content-safety decision", is in `docs/decisions/2026-11-12-X-win-upd-manual-rollback.md` (follow-up 1).
- **Fix:** Cite those two files correctly. The scrub only enforces existing decisions, so it is a runner call (§2.33). Drop row 16, or turn it into a §2.19 flag under "Any time … the runner calls open to a veto", with UPDATE-05 (Windows) named in Track Q's "go ahead" list. Then the range becomes "13, 14, 15 and 17".

**I2. Row 19 (TEST-05, the drawer's size budget) is the runner's call under §2.26, not an owner question.**
- **Where:** `docs/build-plan.md:255`; decisions file line 15.
- **Why:** §2.26 makes performance and UX changes to owner-set numbers, size budgets named, the runner's decision, flagged under §2.19. The row even cites §2.26. The review's own text says "one of them should change, under §2.26".
- **Fix:** The runner decides which number changes, records it and flags it. Move the row to the "Any time" veto list and renumber 20 to 22.

**I3. Row 17's premise is out of date: X-consent-gate-lift has merged.**
- **Where:** `docs/build-plan.md:253`.
- **What's stale:** "settle it before X-consent-gate-lift widens what sources can run". X-consent-gate-lift r1 merged as #807 (`559c1755`, this PR's base). Per `docs/decisions/2026-11-12-X-consent-gate-lift.md`, it opens Restricted and Overlay, and no plug-in of either class is registered yet. External waits for X-consent-gate-lift-external and Account for the -r2 PR.
- **Fix:** "settle it before X-consent-gate-lift-external, the Account r2 or the first Restricted or Overlay plug-in lets more sources run". Also, SRC-02 is a recorded runner decision flagged under §2.19, so word the row as "keep or veto".

**I4. The INFRA-03 sentence about v0.5.1.1 is wrong.**
- **Where:** `docs/build-plan.md:104` ("INFRA-03 … applies to a v0.5.1.1 if §2.58's fallback is used"); decisions file line 16.
- **Why it's wrong:**
  - INFRA-03 is about a backport tagged after a newer release has shipped. §2.58's fallback is used only while v0.5.2 is unpublished, so `:latest` would move forward, not back.
  - A tag named `v0.5.1.1` isn't a valid version for the workflow. `release.yml` on `release/v0.5.1` rejects it twice: `package-gate`'s Version step requires strict MAJOR.MINOR.PATCH[-pre], and so does `publish`'s "Tag is a semantic version" step. As named, it would publish nothing.
- **Fix:** Drop the sentence. If anything is said: INFRA-03 matters for any patch on an older line once a newer version is out (for example a v0.5.x after v0.6.0). A §2.58 fallback needs a valid semver name, and choosing it is the owner's call (releases).

**I5. Row 22 and the hostile-input row have no rule for what happens before the owner answers.**
- **Where:** `docs/build-plan.md:102` and `:258`.
- **Why it matters:** Row 21 says "the plan follows P4 until you answer". Row 22 has no such line, so slices that fix blocking availability findings could wait on an owner reply. Placing a new fix slice on a rung is something the runner already does (X-watch-stall-retry went into rung 5 in r4). Only reordering the owner-approved ladder is the owner's.
- **Fix:** Add to row 22 and the hostile-input row: "Until you answer, the runner places them as it places any new fix slice (§2.33), so they don't wait; the question is only whether they go ahead of rungs 2 and 3."

**I6. Track U re-words an owner decision and repeats §2.66, so the two will drift.**
- **Where:** `docs/build-plan.md:117` ("Decided: 'Group versions' is on by default … (back to the owner if G-v07-scale fails with it on)"), and the restated drawer rule at `:113` and `:245`.
- **What's lost:** §2.66 item 2 says that if G-v07-scale fails the keyword p95 with grouping on, "the default falls back to off and the question comes back to the owner". The summary drops the fall-back to off, which is the rule that applies in the meantime.
- **Fix:** Replace the restatements with pointers ("per §2.66 items 1 to 3"), or quote item 2 exactly. The decisions file already says it doesn't restate #809's file (line 9). Hold §0 to the same rule.

## Optional

- **O1. Two named ids are in neither `slices.yaml`.**
  - **Where:** `docs/build-plan.md:95`.
  - **Detail:** X-watch-stall-retry and X-utp-spurious-rto are in `runner/slices.yaml` neither on `main` nor on #809. r4 listed them as entries to add (`2026-11-12-master-plan-r4.md`, list item 1), and §0.9 item 4 still says "once the runner adds it".
  - **Fix:** Add "(neither is in `runner/slices.yaml` yet)".
- **O2. "M8-SEC-0 may cover it" (INFRA-09) is unlikely.**
  - **Where:** `docs/build-plan.md:104`.
  - **Detail:** M8-SEC-0 merged as #786: `SECURITY.md` and the review method, with no CI check. M8-SEC-5 reviews "pinning, dependencies", but only by reading.
  - **Fix:** "No slice adds an advisory check; M8-SEC-5 reviews dependencies by reading only."
- **O3. INFRA-04 is named but not described.**
  - **Where:** `docs/build-plan.md:104`.
  - **Detail:** The cell lists INFRA-01 to INFRA-04 and INFRA-08 but describes only four of the five. INFRA-04, the image built from the shared Actions cache, is missing.
  - **Fix:** Add a description by id and surface only (see B1).
- **O4. Cite the holding method's own file.**
  - **Where:** `docs/build-plan.md:108`, "held under `SECURITY.md`'s method (M8-SEC-0)".
  - **Detail:** The holding method is `docs/security/v1/README.md` §5. `SECURITY.md` is the public reporting policy.
  - **Fix:** Cite `docs/security/v1/README.md` §5.
- **O5. The history line understates the change.**
  - **Where:** `docs/build-plan.md:451` ("nothing else in §0 changes") and decisions file line 3 ("changes nothing else in it").
  - **Detail:** The PR also changes:
    - the header blockquote;
    - §0.2's concurrency and cloud-testing paragraphs (the "not yet recorded" notes);
    - §0.3's closing sentence;
    - §0.9 item 3;
    - §1.2.
  - **Fix:** List them, or say "and the 'not yet recorded' notes for §2.62 to §2.65 are corrected".
- **O6. Two small wording points.**
  - **Where:** `docs/build-plan.md:47`; decisions file line 8.
  - **Line 47:** It cites "(runner PR #809)" twice, and a slice is launched, not launched by a PR. Use "launched 2026-11-13T03:15Z; recorded in #809".
  - **Decisions file line 8:** It says the B1/B2 priority was "relayed by the review session". #809's file says it was "relayed by the 'Remaining work plan' session from the codebase-review session". Use #809's wording.
- **O7. One open owner question is left out, so the closing sentence is wrong.**
  - **Where:** `docs/build-plan.md:262`, "Every question … was answered, except …".
  - **Detail:** #807's decisions file has an open owner question: the Legal Notice §2 sentence about the torrent cache, which gates X-consent-gate-lift-external.
  - **Fix:** Add a row, or say the live list is `owner_queue` and the runner adds it there.
- **O8. Make the INFRA-01 check concrete.**
  - **Where:** `docs/build-plan.md:438` and `:104`.
  - **Suggested text:** "the `v0.5.2` tag publishes `:0.5.2`, `:0.5` and `:latest` as soon as smoke and the arm64 build pass, before the `release` approval. Gating it for this tag needs a release PR to `release/v0.5.2` before the tag."
  - **Precision:** Write "the workflow at the tagged commit" rather than "the one on `release/v0.5.2`".
- **O9. Merge order.** §2.66 and `2026-11-13-runner-ui-review-decisions.md` exist only on #809. Merge #809 first, or in the same landing, or the citations point at nothing for a while.

## Hygiene
- No 12-digit numbers (`git diff origin/main...HEAD | grep -nE '[0-9]{12}'` finds nothing).
- Every new time is UTC with Z, and no time zone or place is named. ("EST" matches are inside "TEST-0x".)
- No model identifiers. The text says only that a trailer "carries a model name". "Opus slices" is the repository's policy tier, as elsewhere in the plan.
- No personal data.
- Paths: everything cited exists on `main`, except the review folders (on the named branches, as the text says) and the #809 file (O9).

## For the runner (dashboard.yaml)
Not a finding against #810. These are the edits your own PR would make so the dashboard matches r4.1, after #809 and after B1's wording is settled. Under the method, `dashboard.yaml` and `slices.yaml` may carry only ids, severities and surfaces for held findings.
1. **`owner_queue`:** one `type: decision` entry per open row (13 to 15, 17, 18, 20 to 22 after I1 and I2), each with `unblocks` and `when`. Row 18's `when` is "before the v0.5.2 tag". Also:
   - the review-branch question (B1 fix 4);
   - #807's Legal Notice sentence, if it isn't there yet;
   - a §2.19 `review` entry for what moves to the veto list (the restore scrub, TEST-05).
2. **`conditions`:** an `ext:` entry (`who: you`, `met: false`) for each owner answer that a fix slice's `deps` will name. For example, the FILTER-01 and FILTER-03 fix slices, the INFRA-01 image gate, the part-4 proposals behind Track U step 6, and the v0.6 tag question (row 21), unless an existing condition covers it. `npm test` fails on an `ext:` that has no entry.
3. **`roadmap.steps`:** two new steps.
   - **Track U:** "UI review fixes", `members` X-wizard-labels-signout, X-sources-restructure, X-ui-guards and X-insights-funnel-fixture (all on #809), then the I1+I3 slice, the B3 slice and the L1/L2 slices once they're in `slices.yaml`.
   - **Track Q:** "Code-review fixes", with `members: []` until sliced. X-lease-reconcile-every-session, X-watch-stall-retry and X-utp-spurious-rto need `slices.yaml` entries before they can be members.
   - Free `col`/`row` cells are needed. Rows 3, 4 and 7 have the fewest steps now. Optional edges: Track Q → the `m8`/`v1` steps (G-v1-security), Track U → `m8` (M8-DR).
4. **`placement`:** #809 already places its four ids. For Track Q slices, either add rules into the existing area workstreams (ws-filters, ws-api, ws-workers, ws-companions, ws-m6, ws-m8-release, ws-web) or add one workstream, such as `ws-review-fixes` under `ph-polish` or `ph-v10`. Keep `owns` at component level per the method.
5. **`gates`:** no new gate. Update G-v1-security's `plain` and `target` to say Track Q's blocking and important findings are in scope, matching the `slices.yaml` change (decisions-file runner step 4). The INFRA-01 check has no gate entry to attach to, since there is no v0.5.2 gate in `gates`. Put it in the `v052` roadmap step's `plain`, or in a `notes.now` item.
6. **`notes`:** `updated` is still `'2026-11-08'`. Add `now` items for "B1 and B2 first (§2.66)" and "Review fixes: Tracks Q and U", and a `you` item for the new owner decisions.
7. Run `cd tools/status-connector && npm test`. The CI warning that `build-plan.md` §0 changed without `dashboard.yaml` is expected on #810 and is cleared by this PR.

## INFRA-01
**On `release/v0.5.2` (head `114bb625`), `.github/workflows/release.yml` (752 lines, no reusable workflow called):**
- **Trigger:** `on.push.tags: ["v*.*.*"]`, plus `schedule`, `workflow_dispatch` and path-filtered `pull_request`.
- **Job that pushes the image:** `publish` ("publish (amd64 + arm64)", lines 248-311).
  - `needs: [smoke, build-arm64]`.
  - `if:` smoke and build-arm64 both succeeded, the event isn't a pull request, and the ref is `refs/tags/v*` or `refs/heads/main`.
  - **No `environment:`.**
  - `permissions: contents: read, packages: write`.
  - It logs in to `ghcr.io` with `GITHUB_TOKEN`. `docker/metadata-action` sets `flavor: latest=auto` and tags `{{version}}` and `{{major}}.{{minor}}`. `docker/build-push-action` runs with `push: true`, `platforms: linux/amd64,linux/arm64` and `cache-from: type=gha`.
  - Its only tag check is a semver regex, which also accepts pre-releases.
- **Jobs behind the `release` environment:** `package-windows` (`environment:` is `release` on tags, for its signing secrets) and `package-release` (`environment: release`, which creates the draft release). Neither is in `publish`'s `needs`. So the image `:X.Y.Z`, `:X.Y` and `:latest` is pushed as soon as smoke and the arm64 build pass, whether or not the owner approves the release. There is no `update-manifest` job on this branch.

**On `main`:** the same `publish` job (lines 302-370), with `needs: [smoke, build-arm64, package-gate]` (package-gate only supplies the version) and still no `environment:`. `package-windows`, `package-release` and `update-manifest` use `release`.

**Which file runs:** a tag push runs the workflow at the tagged commit, so a `v0.5.2` tag on `release/v0.5.2` runs that branch's file, and a fix on `main` alone changes nothing for it. No `v0.5.2*` tag exists yet (the remote tags are `v0.5.1` and `v0.5.1-rc.1` to `-rc.4`).

**Verdict on the PR's description:** correct. "The image is pushed on any version tag without the release environment gate", and "the workflow that runs is the one on `release/v0.5.2`", are accurate; strictly, it is the file at the tagged commit. The v0.5.1.1 and INFRA-03 sentence is not (I4). Files were read only; no cloud or GitHub API call was made.
