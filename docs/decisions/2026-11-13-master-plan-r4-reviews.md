# Master-plan update r4.1 decisions (2026-11-13)

After #798 merged, three reviews of `main` reached the plan session, and the owner asked for the plan to be updated with them. This adds two fix tracks and nine questions for the owner to build-plan §0 (r4). Docs only. It follows `2026-11-12-master-plan-r4.md`; the exact list of what changed in §0 is in its §0.10 line.

Nothing here reverses or loosens an owner decision, weakens a security or content-safety decision, or changes an owner-set number.

## What the owner decided, as the plan carries it
- **B1 and B2 first** (`docs/owner-decisions.md` §2.66, runner PR #809; the owner, 2026-11-13, relayed to the runner by the codebase-review session and by the "Remaining work plan" session): the first-run wizard's raw labels and the missing Sign out go ahead of the larger queue.
- **The §2.66 answers to the UI review** are not restated here or in §0. The plan points to them. Their decisions file is `2026-11-13-runner-ui-review-decisions.md` (#809).
- **The owner asked for the plan to be updated** with the review results. The review findings reached this session second-hand, from the review sessions, so the plan treats them as unverified review output, not as decisions.

## What the plan does with the reviews (runner-level, §2.19)
- **Held detail stays out.** Blocking and important findings about security or content safety are named by id, severity and surface only, as `docs/security/v1/README.md` §5 and owner §2.47 require. Owner questions about them are worded as choices between fixes. No review folder is committed by this PR, and the plan doesn't tell the runner to bring the code-review folder to `main`.
- **Two tracks, not a re-ranked ladder.** Track Q (code review) and Track U (UI/UX) sit beside the ladder. The ladder is the owner's to reorder, and the owner reordered it for B1 and B2 only. For Track Q's hostile-input group the plan asks whether it goes ahead of rungs 2 and 3, and says the runner places those slices as any new fix slice (§2.33) until the owner answers.
- **No slice ids are invented.** The runner owns them. The plan names the ids the runner has already chosen (X-wizard-labels-signout, X-ui-guards, X-sources-restructure and X-insights-funnel-fixture in #809; X-lease-reconcile-every-session, X-watch-stall-retry and X-utp-spurious-rto from its messages, none of the last three in `runner/slices.yaml` yet) and describes the rest by finding id.
- **Questions 13 to 21 in §0.3** are the code review's questions, the UI review's eight design proposals as one row, the v0.6 tag question still open from r4, the order of the hostile-input fixes, the draft advisories and the review branch, and the Legal Notice sentence open on #807. The restore-path rule, SRC-02 and TEST-05 are runner calls listed for veto, not questions.
- **INFRA-01 is checked before the v0.5.2 tag.** A version tag publishes the image before the `release` approval, and the workflow that runs is the one at the tagged commit, so gating it for this tag needs a release PR to `release/v0.5.2`. The runner's review confirmed this from the workflow files.
- **Track Q's blocking and important findings feed G-v1-security.** None may be open at the release candidate unless held under the method in `docs/security/v1/README.md` §5.

## For the runner (shared files this PR doesn't edit)
1. Decide what, if anything, of the two review folders holds no held detail and comes to `main`. Don't merge the commit on `claude/codebase-review-bugs-x9ff7p` whose trailer names a model.
2. Triage Track Q and the UI review into slices; ask the owner the questions in §0.3 (13 to 21).
3. `runner/dashboard.yaml`: the owner queue, roadmap steps for the two tracks, conditions for the owner answers the fix slices will depend on, if you agree.
4. Extend G-v1-security's scope in `runner/slices.yaml` to Track Q's findings.
5. Run the checks in the UI review's `03-not-done.md` that need no owner, and send the owner the ones that do.
