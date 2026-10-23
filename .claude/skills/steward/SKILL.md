---
name: steward
description: How a Lodestar slice session drives its own pull request from first push to merge-ready, for the build runner (runner/README.md). Use in every slice session the runner launches, and in any other session that opens a PR on this repository: self-review before the PR, watching your own PR, fixing CI and merge conflicts, answering the runner's review, and telling the runner when the PR is ready or blocked. The runner merges; you never do.
---

# Steward: drive your own PR to merge-ready

You own your slice's PR until it merges. The runner launches slices, runs the final review and merges. Everything between your first push and "ready to merge" is yours, so the runner doesn't have to relay it. `runner/standards.md` is the bar throughout.

## 1. Before you open the PR
1. Run every check in your brief's "Done when" line and CLAUDE.md's pre-push checks. Don't push red.
2. **Review yourself with a fresh reviewer.** Spawn one `Agent` subagent with `model: "opus"`, in the foreground. Give it only:
   - the diff against `origin/main`;
   - your brief;
   - the spec sections the brief cites;
   - the whole of `runner/standards.md`.

   Don't give it your own opinion of the code. Ask it for blocking, important and polish findings, as `runner/standards.md` §5 defines them.
3. Fix every blocking and important finding, then re-run the checks. Do the same for polish findings that are plainly right and in your owned paths.
4. Open the PR to `main`, titled `<id>: <title>`. Write the description the standing instructions ask for, and add one line on the self-review: how many findings it raised, what you fixed, and what you left and why.

The runner's own fresh review still runs. The self-review is there so that review finds nothing to send back.

## 2. Watch your PR
- Right after opening the PR, call `subscribe_pr_activity` for it. CI results, reviews and conflicts then wake you; don't poll.
- **Tell the runner.** Send `send_message` to `@parent` with `<id>: PR #<n> ready for review, head <sha>`. If you weren't launched by the runner, use the runner session id from `state.json` (`runner_session`) on the `runner-state` branch.

## 3. On each event
- **CI red:** reproduce the failure locally, fix it and push. Before blaming the PR, check whether the same check is red on `main`, or whether a fix PR for a known flake is already open. If either is true, it isn't your PR's failure (`docs/owner-decisions.md` §2.18): say so once on the PR, naming the check, the evidence and the fix PR, and tell the runner. "Flake" is never a root cause on its own.
- **Merge conflict, or the runner asks you to bring `main` in:** `git fetch origin main && git merge origin/main`. Never rebase, amend or force-push. Resolve the conflict, regenerate generated files with the repo's tooling, re-run the checks and push.
- **Review findings from the runner:** fix every blocking item. Reply on each thread, saying what changed or why not. Push once everything is fixed and the checks pass, then message `@parent` with the new head.
- **Your PR merged or closed:** unsubscribe and stop.

## 4. Never
- **Never merge, enable auto-merge or push to `main`.** `.claude/hooks/merge-guard.sh` refuses these calls in every session except the runner's. Don't work around it: the runner merges.
- Never edit paths outside your brief's `owns`. A shared file that needs a change goes in the PR description.
- Never skip, disable or quarantine a test to get green, and never push an empty commit to re-run CI.

## 5. When you're blocked
Open the PR as a draft if you haven't opened one, or comment on it, saying exactly what's missing. Then message `@parent` with `<id>: blocked on <what>` and stop. A question for the owner goes to the runner, never to the owner directly.
