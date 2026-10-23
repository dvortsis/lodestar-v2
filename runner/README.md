# The build runner

The runner is one long-lived **Opus session**. It works through `docs/build-plan.md` without anyone at the keyboard:
- it starts each slice in a **new cloud session** once that slice's dependencies are merged;
- it has every PR reviewed;
- it merges what passes;
- it moves on.

The owner approved this on 2026-10-22. Merging green, reviewed PRs and creating branches as needed are both allowed. If a direct merge is refused, the runner turns on GitHub auto-merge instead, still only after green CI and a passing review.

**`runner/standards.md` is the single bar** for every slice and every review: the checks, the code and UI rules, and the review checklist with severities. Every brief and every reviewer prompt points to it.

If the runner session is lost, start a new Opus session (Auto mode) with the prompt *"Act as the Lodestar build runner: follow runner/README.md."*. Everything it needs is in this directory and on the `runner-state` branch.

## Files
| File | Holds | Changes how |
|---|---|---|
| `runner/slices.yaml` | the slice graph: id, model, dependencies, owned paths, exit checks, limits | by PR to `main`, when the plan changes or an epic is expanded |
| `runner/briefs/<id>.md` | the brief a slice session starts from (template: `build-plan.md` §7) | written by the runner just before launch, committed with the state |
| `state.json` on branch **`runner-state`** | each slice's status, session id, PR number, review rounds, attempts, and today's launch count | pushed directly by the runner after every change |

Slice status moves through `pending → ready → running → in_review → merged`. The two exits are `parked`, which needs the owner, and `blocked`, which is waiting on something outside the runner.

## Other sessions the runner watches
The owner asked the runner to keep an eye on the M4 sessions ("M4 continuation" and the content-filter session titled "Modern DHT crawler architecture"). On every heartbeat the runner:
- checks their status;
- tells the owner about any pending permission prompt, which it can't answer;
- nudges an idle session that still has work left;
- flags conflicts with slice ownership or migration order. The current agreed order is in `state.json` under `migration_order`; M5's migration comes after all of M4's.

The list lives in `state.json` under `watched_sessions`.

## When the runner wakes
1. **GitHub events** on every slice PR it subscribes to (CI results, reviews, merges, conflicts).
2. **An hourly scheduled task**, the safety net. A slice session that finishes cleanly doesn't notify anyone, so this is how the runner notices it.
3. **A one-off check about 30 minutes after each launch.**
4. **Messages from slice sessions.**

## What it does on each wake (reconcile, then act)
1. **Load state.** Fetch `runner-state` and `main`.
2. **Reconcile every running slice:**
   - Read its session with `get_session` (`status_bucket`) and find its PR by head branch `slice/<id>`.
   - **Failed session or turn:** send the session one message to resume. If it fails a second time, mark the slice `attempt+1` and relaunch it as below.
   - **PR open and the session idle:** move the slice to `in_review`.
3. **Review every `in_review` slice whose CI is green on its head:**
   - Spawn a **fresh Opus reviewer**: an `Agent` subagent, Opus, in the background. It gets only the diff, the brief, the spec sections and the whole of `runner/standards.md` (§1–§5), never the runner's own opinion.
   - It reports blocking, important and polish findings.
   - **Anything touching auth, credentials, filters, the cluster or companions** also gets `/security-review`.
   - **Blocking findings:** send them to the slice session, with the exact items, to fix and push. Then review again.
   - **No blocking findings:** merge with a merge commit. Then set the slice to `merged`, and pass important and polish findings to the next slice that touches those files.
   - **Merge conflict with `main`:** tell the slice session to merge `origin/main` (never rebase) and push.
   - **CI red:** send the slice session the failing job and the log excerpt, unless the check isn't the PR's (see below).
4. **Retry limits:**
   - A **Sonnet or Haiku** slice that fails review twice is relaunched **on Opus** on the same branch. That's the "weak output is redone on Opus" rule.
   - An **Opus** slice that fails twice is `parked`, and the owner is notified.
5. **Compute the ready set.** A slice is ready when every one of its `deps` is `merged` and every `ext:` dependency is satisfied (see the `external` section of `slices.yaml`).
6. **Launch, within the limits in `slices.yaml`:**
   - at most `max_core` Opus code slices, and only if their `owns` paths are disjoint;
   - at most `max_helpers` Sonnet and Haiku slices;
   - docs-only slices don't count towards either limit;
   - at most `max_launches_per_day` new sessions;
   - **never two running slices whose `owns` overlap.** Every slice records its decisions in its own new file under `docs/decisions/`, so no two slices share a decisions file (§16 is closed to new entries);
   - **order:** critical-path core slices first, then the slices that unblock the most others.
7. **Expand epics.**
   - When an `epic` entry's deps are merged, the runner (Opus) reads the relevant architecture sections and the code as it now stands.
   - It writes that epic's slices into `slices.yaml`, opens a PR, reviews it and merges it.
   - Later phases are planned this way, with current knowledge, not guessed now.
8. **Save state.** Push `state.json`, then schedule the next one-off check.

## Launching a slice session
Use `create_session` with these settings:
- **source:** `https://github.com/dvortsis/lodestar-v2` at `main`
- **outcome branch:** `slice/<id>`
- **model:** the slice's `model`
- **tags:** `["lodestar-runner", "slice:<id>"]`
- **title:** `<id>: <title>`
- **permission mode:** inherited, so the runner must itself run in **Auto**
- **prompt:** the brief, followed by the standing instructions below

**Standing instructions for every slice session:**
> You are building one slice of Lodestar for an automated runner.
> - Read CLAUDE.md, `runner/standards.md` and the brief's spec sections first.
> - Work only on branch `slice/<id>`, and edit only the paths the brief lets you own. If a shared file needs a change, say so in the PR description instead of making it.
> - Before pushing, run every check in the brief's "Done when" line, plus CLAUDE.md's checks. Don't push red.
> - When done, open a PR to `main` titled `<id>: <title>`. Its description should cover what changed, the checks you ran with their results, the shared-file edits needed, any missing contract fields, and any deviations from the spec and why.
> - **Do not merge.** The runner reviews and merges.
> - If the runner sends review findings, fix them, push, and reply on the PR.
> - If you're blocked, comment on the PR (or open a draft PR) saying exactly what's missing, then stop.
> - No model identifiers in commits, PRs or code.

## Conflicts in `docs/architecture.md` §16
When a PR conflicts with `main` only in `docs/architecture.md` §16, the runner (or the slice) resolves it by merging `main` and keeping both blocks, main's first. A semantic conflict, such as a new required struct field, is fixed in the same merge commit and the checks re-run.

## Notifying the owner
Send a push notification only for:
- a parked slice;
- an external blocker that needs the owner (signing certificate, Unraid submission, a policy question);
- a milestone exit gate (v0.5, M5, …) being met.

There's a **daily summary** on the first wake after 07:00 in the owner's time zone. It covers what merged, what's running, what's parked and why, the next ready slices, and every refinement of an owner decision decided since the last summary (`docs/owner-decisions.md` §2.19), which the owner can veto.

## Stopping
- **Disable the hourly scheduled task, or tell the runner to stop.** Running slices finish their current turn, and nothing new launches.
- **`max_launches_per_day`** is a backstop against runaway loops. Hitting it stops launches and notifies the owner.

**Green means every check run on the head finished `success`, `skipped` or `neutral`.** Since #90, CI skips the jobs a PR's files don't need, so a docs-only PR shows skipped Rust jobs; those count as passing. So does a red check that isn't the PR's (next paragraph).

**A red check that isn't the PR's doesn't block** (owner, 2026-10-22, `docs/owner-decisions.md` §2.18). That means a check that's also red on `main`, or a known flake with a fix PR open, failing in code the PR doesn't touch. The runner counts it as passing and says so on the PR, naming the check, the evidence (the same check red on `main`, or the fix PR) and the fix PR if there is one. Anything else red is the PR's to fix.
