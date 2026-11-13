# Master-plan update r4.1 decisions (2026-11-13)

After #798 merged, three reviews of `main` reached the plan session, one by one, and the owner asked for the plan to be updated with them. This adds two fix tracks and ten questions for the owner to build-plan §0 (r4) and changes nothing else in it. Docs only. It follows `2026-11-12-master-plan-r4.md`.

Nothing here reverses or loosens an owner decision, weakens a security or content-safety decision, or changes an owner-set number.

## What the owner decided, as the plan carries it
- **B1 and B2 first** (`docs/owner-decisions.md` §2.66, runner PR #809; the owner, 2026-11-13, relayed by the review session): the first-run wizard's raw labels and the missing Sign out go ahead of the larger queue.
- **The §2.66 answers to the UI review** (the drawer beside the results, "Group versions" on by default, no posters in "Identified as", Sources restructured before the next plug-in row) are summarised in Track U. The decisions file for them is `2026-11-13-runner-ui-review-decisions.md` (#809); this file doesn't restate it.
- **The owner asked for the review documents to come to the plan session** and for the new items to be added. The code review, UI/UX review parts 3 and 4 and the later checks (L1 to L5) reached this session second-hand, from the review sessions, so the plan treats their findings as unverified review output, not as decisions.

## What the plan does with the reviews (runner-level, §2.19)
- **Two tracks, not a re-ranked ladder.** Track Q (code review) and Track U (UI/UX) sit beside the ladder. The ladder is the owner's to reorder, and the owner reordered it for B1 and B2 only. For the rest, the plan recommends that Track Q's content-safety and hostile-input fixes run beside rungs 2 and 3 and ahead of v0.7 features, and puts that as an owner decision (§0.3, 22) instead of inserting it.
- **No slice ids are invented.** The runner owns them. The plan names the ids the runner has already chosen (X-wizard-labels-signout, X-ui-guards, X-sources-restructure, X-insights-funnel-fixture in #809; X-lease-reconcile-every-session, X-watch-stall-retry, X-utp-spurious-rto from its messages) and describes the rest by finding id. Until #809 merges, the first four aren't in `runner/slices.yaml` on `main`.
- **Questions 13 to 19 for the owner are the review's seven**, in its order, plus the eight design proposals as one row (20), the v0.6 tag question still open from r4 (21) and the order of Track Q against the ladder (22).
- **INFRA-01 is flagged against the v0.5.2 tag.** The review says the image is pushed on any version tag without the release-environment approval. The tag is rung 2, so the plan asks for it to be checked first; the workflow that runs is the one on `release/v0.5.2`. INFRA-03 (a backport tag moves `:latest` back) applies to a v0.5.1.1 under §2.58. This is the plan's reading of the review's finding, not something the review says about v0.5.2.
- **Track Q's blocking and important findings feed G-v1-security.** None may be open at the release candidate unless held under the method M8-SEC-0 writes.

## For the runner (shared files this PR doesn't edit)
1. Bring both review folders to `main`, re-committed: `docs/code-review/2026-11-13-full-code-review*` from `claude/happy-hypatia-i314gf` (`4f2ccbd9`) and `docs/review/2026-11-13-ui-ux/` from `claude/codebase-review-bugs-x9ff7p` (`5c3e447a`). Don't merge commit `34cbbafb` on the second: its trailer names a model.
2. Triage Track Q and the rest of the UI review into slices; ask owner decisions 13 to 22.
3. `runner/dashboard.yaml`: the owner queue, a roadmap step or note for the two tracks, if you agree.
4. Extend G-v1-security's scope in `runner/slices.yaml` to Track Q's findings.
5. Run the checks in `03-not-done.md` that need no owner, and send the owner the ones that do.
