# The build runner

The runner is one long-lived **Opus session**. It works through `docs/build-plan.md` without anyone at the keyboard:
- it starts each slice in a **new cloud session** once that slice's dependencies are merged;
- it has every PR reviewed;
- it merges what passes;
- it moves on.

The owner approved this on 2026-10-22. Merging green, reviewed PRs and creating branches as needed are both allowed. If a direct merge is refused, the runner turns on GitHub auto-merge instead, still only after green CI and a passing review.

**`runner/standards.md` is the single bar** for every slice and every review: the checks, the code and UI rules, and the review checklist with severities. Every brief and every reviewer prompt points to it.

If the runner session is lost, start a new Opus session (Auto mode) with the prompt *"Act as the Lodestar build runner: follow runner/README.md."*. Everything it needs is in this directory and on the `runner-state` branch. The watchdog (below) does this on its own when the runner goes quiet.

## Who merges
**Only the runner merges, enables auto-merge or writes to `main`** (owner, 2026-10-23, `docs/owner-decisions.md` §2.22).
- **The hook enforces this.** `.claude/hooks/merge-guard.sh` refuses those calls in any session without the marker file `~/.lodestar-runner`.
- **Creating the marker is the runner's first step** in every session that acts as the runner: `echo "<this session id>" > ~/.lodestar-runner`. No other session creates it.
- **Every session shares one GitHub account,** so GitHub can't tell the runner from a slice. The hook is a guard against a session merging on its own initiative (as the filter session did with #33, #42, #47 and #52), not against one that sets out to bypass it.
- **Branch protection on `main`** requires a PR and the `ci-ok` check, with no bypass. `ci-ok` covers the correctness jobs in `ci.yml`. The performance-budget jobs, `web.yml`, `e2e.yml` and `release.yml` (separate, path-filtered workflows that can't be required without stalling PRs that don't trigger them) stay outside it and are judged by the green rule below, as before.
- **Only one runner at a time.** On every wake, and again right before every merge and every launch, re-read `runner_session` from `origin/runner-state`. If it names another session, this one has been replaced: delete the marker, unsubscribe from every PR, delete its own scheduled tasks and stop. Never force-push `state.json`; on a non-fast-forward push, refetch and re-check `runner_session` first.

## Files
| File | Holds | Changes how |
|---|---|---|
| `runner/slices.yaml` | the slice graph: id, model, dependencies, owned paths, exit checks, limits | by PR to `main`, when the plan changes or an epic is expanded |
| `runner/briefs/<id>.md` | the brief a slice session starts from (template: `build-plan.md` §7) | written by the runner just before launch, committed with the state |
| `state.json` on branch **`runner-state`** | each slice's status, session id, PR number, review rounds, attempts, and today's launch count | pushed directly by the runner after every change |

Slice status moves through `pending → ready → running → in_review → merged`. The two exits are `parked`, which needs the owner, and `blocked`, which is waiting on something outside the runner.

## Other sessions the runner watches
**Every session that writes code on this repository is either a runner slice or registered with the runner** (§2.22). That includes sessions the owner starts. When the runner finds one, it adds a slice entry to `state.json` for it: its owned paths, its place in `migration_order`, and the tag `lodestar-runner`. It also points the session to `.claude/skills/steward/SKILL.md`. The hook stops such a session merging, so its PRs reach `main` through the runner's review like any slice's.

The owner asked the runner to keep an eye on the M4 sessions ("M4 continuation" and the content-filter session titled "Modern DHT crawler architecture"). On every wake, not only the heartbeat, the runner:
- checks their status;
- tells the owner about any pending permission prompt at once, with a push notification, since it can't answer it (step 3 below);
- nudges an idle session that still has work left;
- flags conflicts with slice ownership or migration order. The current agreed order is in `state.json` under `migration_order`; M5's migration comes after all of M4's.

The list lives in `state.json` under `watched_sessions`.

## When the runner wakes
1. **Messages from slice sessions,** the main signal. Under the `steward` skill each slice messages the runner when its PR is ready for review, when it has pushed review fixes, and when it's blocked.
2. **GitHub events** on every slice PR it subscribes to (CI results, reviews, merges, conflicts). `main`'s own CI sends no events, so the runner checks it on every wake (step 2 below).
3. **An hourly scheduled task**, the safety net for a slice that went quiet without a message.
4. **A one-off check about 30 minutes after each launch.**

## What it does on each wake (reconcile, then act)
1. **Load state.** Fetch `runner-state` and `main`. Check `runner_session` (see "Who merges"), and make sure the marker file exists.
2. **Keep `main` green.** If the latest push run of CI on `main` is red in `ci-ok`, find the merge that broke it. Fix it the fastest way:
   - **Revert:** open a revert PR. Its review checks only that it is a clean revert.
   - **Port a fix:** port the fix if one exists.

   Until `main` is green again, merge nothing else that touches the same crates or `web/`.
3. **Reconcile every running slice:**
   - Read its session with `get_session` (`status_bucket`) and find its PR by head branch `slice/<id>`.
   - **Failed session or turn:** send the session one message to resume. If it fails a second time, mark the slice `attempt+1` and relaunch it as below.
   - **PR open and the session idle:** move the slice to `in_review`.
   - **Red CI or a merge conflict:** the slice handles these itself (`steward` skill §3). Step in only when the session is idle and the PR has sat red or conflicted for more than 30 minutes; then send it one message with the failing job.
   - **Waiting on a permission prompt** (`status_bucket` blocked, with a pending action in its summary), whether a slice or a watched session: send the owner a push notification at once, naming the session and what it's waiting on. The runner can't answer the prompt, and the owner may not see it otherwise. Notify once per prompt.
4. **Review every `in_review` slice whose CI is green on its head:**
   - Spawn a **fresh Opus reviewer**: an `Agent` subagent, Opus, in the background. It gets only the diff, the brief, the spec sections and the whole of `runner/standards.md` (§1–§5), never the runner's own opinion.
   - It reports blocking, important and polish findings.
   - **Anything touching auth, credentials, filters, the cluster or companions** also gets `/security-review`.
   - **Blocking findings:** send them to the slice session, with the exact items, to fix and push. Then review again.
   - **No blocking findings:** check freshness, then merge with a merge commit.
     - **Fresh enough:** the PR's head already contains `main`, or nothing merged since its merge base touches the same area. The areas are: Rust, as `.github/scripts/ci_changes.py` defines it (`crates/**`, `Cargo.*`, `bench/**`, `fuzz/**`, `rust-toolchain*`, `.cargo/**`, `web/src/lib/api/openapi.json`); the rest of `web/**`; and `.github/**`. Docs-only changes never overlap.
     - **Not fresh enough:** bring `main` in first, with `update_pull_request_branch` (a merge commit), and merge once CI is green on the new head. This is the substitute for a merge queue, which GitHub offers only on repositories owned by an organization.
     - **The landing lane** (owner, 2026-10-23, `docs/owner-decisions.md` §2.24). It stops a reviewed PR from chasing a fast `main`, as #48 did for 36 hours. A PR enters the lane when it has passed review but still needs `main`, and it has already needed two catch-up merges, or has more than ~1,500 changed lines.
       - While it's in the lane, merge nothing else that overlaps its areas. Docs-only PRs can still merge. Record the lane in `state.json` (`landing_lane`: PR, areas, entered at).
       - Ask the running slices whose areas overlap to hold their pushes until it lands, so the Actions queue is free for it.
       - Only one PR is in the lane at a time; pick the critical-path PR first.
       - The PR leaves the lane when it merges, or when a failure turns out to be its own. Then the others resume while it's fixed, and it re-enters once it's green.
       - If the lane has been held for 2 hours, tell the owner what's holding it.
       - After the PR leaves, tell the slices that held their pushes to resume.
   - **After merging:** set the slice to `merged`, unsubscribe from its PR, and archive its session (`archive_session`). Pass important and polish findings to the next slice that touches those files.
5. **Retry limits:**
   - A **Sonnet or Haiku** slice that fails review twice is relaunched **on Opus** on the same branch. That's the "weak output is redone on Opus" rule.
   - An **Opus** slice that fails twice is `parked`, and the owner is notified.
6. **Compute the ready set.** A slice is ready when every one of its `deps` is `merged` and every `ext:` dependency is satisfied (see the `external` section of `slices.yaml`).
7. **Launch, within the limits in `slices.yaml`:**
   - at most `max_core` Opus code slices, and only if their `owns` paths are disjoint;
   - at most `max_helpers` Sonnet and Haiku slices;
   - docs-only slices don't count towards either limit;
   - at most `max_launches_per_day` new sessions;
   - **never two running slices whose `owns` overlap.** Every slice records its decisions in its own new file under `docs/decisions/`, so no two slices share a decisions file (§16 is closed to new entries);
   - **order:** critical-path core slices first, then the slices that unblock the most others;
   - **size:** a slice estimated at more than ~3,000 changed lines is split before launch: into sequential slices, or into parallel ones against interfaces fixed first. Each PR then lands between other merges instead of chasing `main` (§2.24).
8. **Expand epics.**
   - When an `epic` entry's deps are merged, the runner (Opus) reads the relevant architecture sections and the code as it now stands.
   - It writes that epic's slices into `slices.yaml`, opens a PR, reviews it and merges it.
   - Later phases are planned this way, with current knowledge, not guessed now.
9. **Clear the polish backlog.** When `carry_forward` holds more than 15 items, launch one polish slice for the items whose files no running slice owns.
10. **Save state.** Push `state.json` on every wake, even when nothing changed: the watchdog reads the age of the newest commit. Then schedule the next one-off check.
    - Keep `state.json` small: status fields plus a one-line latest note per slice.
    - The history goes in `log/<id>.md` on the same branch, which the runner reads only when it needs the detail.

## Launching a slice session
Use `create_session` with these settings:
- **source:** `https://github.com/dvortsis/lodestar-v2` at `main`
- **outcome branch:** `slice/<id>`
- **model:** the slice's `model`
- **tags:** `["lodestar-runner", "slice:<id>"]`
- **title:** `<id>: <title>`
- **permission mode:** inherited, so the runner must itself run in **Auto**. Never launch in Plan, because a Plan-mode session stops for approvals nobody is watching.
- **prompt:** the brief, followed by the standing instructions below

**Standing instructions for every slice session:**
> You are building one slice of Lodestar for an automated runner.
> - Read CLAUDE.md, `runner/standards.md`, `.claude/skills/steward/SKILL.md` and the brief's spec sections first. **Follow the steward skill from your first push until the PR merges.**
> - Work only on branch `slice/<id>`, and edit only the paths the brief lets you own. If a shared file needs a change, say so in the PR description instead of making it.
> - Before pushing, run every check in the brief's "Done when" line, plus CLAUDE.md's checks. Don't push red.
> - Before opening the PR, run the fresh-reviewer self-review (steward §1) and fix what it finds.
> - When done, open a PR to `main` titled `<id>: <title>`. Its description should cover what changed, the checks you ran with their results, the self-review, the shared-file edits needed, any missing contract fields, and any deviations from the spec and why.
> - Watch your own PR, and fix red CI and merge conflicts yourself (steward §2–§3). Message the runner (`send_message` to the session in `runner_session` on the `runner-state` branch, or `@parent` if you can't read it) when the PR is ready, after each round of fixes, and when you're blocked.
> - **Do not merge.** The runner reviews and merges, and the merge-guard hook refuses merges from your session.
> - If you're blocked, comment on the PR (or open a draft PR) saying exactly what's missing, message the runner, then stop.
> - No model identifiers in commits, PRs or code.

## Conflicts in `docs/architecture.md` §16
When a PR conflicts with `main` only in `docs/architecture.md` §16, the runner (or the slice) resolves it by merging `main` and keeping both blocks, main's first. A semantic conflict, such as a new required struct field, is fixed in the same merge commit and the checks re-run.

## Notifying the owner
Send a push notification only for:
- a parked slice;
- a session waiting on a permission prompt (step 3);
- a landing lane held for more than 2 hours (step 4);
- an external blocker that needs the owner (the Unraid submission, a policy question);
- a milestone exit gate (v0.5, M5, …) being met.

There's a **daily summary** on the first wake after 07:00 in the owner's time zone. It covers what merged, what's running, what's parked and why, the next ready slices, and every refinement of an owner decision decided since the last summary (`docs/owner-decisions.md` §2.19), which the owner can veto.

## Watchdog
A scheduled task, **"Lodestar runner watchdog"** (`trig_0172BngAUoA4NRaRWBYiRWVf`), starts a fresh session every 4 hours. It only reads, and it decides from the age of the newest commit on `runner-state`:
- **Quiet:** that commit is 2 to 4 hours old and the runner session isn't `working`. The watchdog nudges the runner once with `send_message`.
- **Gone:** that commit is more than 4 hours old, so the last nudge didn't help, or the runner session is archived, failed or missing. The watchdog starts a new runner session with the prompt above, tells the old session to stop, and sends the owner one line.
- **The new runner takes over** by creating the marker and setting `runner_session` to its own id. It then recreates the hourly scheduled task for itself, re-subscribes to every open slice PR, and messages running slices with its session id. An old runner sees the change before its next merge or launch and stops ("Who merges").
- **Otherwise the watchdog does nothing and ends.** It never pushes, merges, launches slices or edits `state.json`.

## Stopping
- **Disable the hourly scheduled task and the watchdog, or tell the runner to stop.** Running slices finish their current turn, and nothing new launches.
- **`max_launches_per_day`** is a backstop against runaway loops. Hitting it stops launches and notifies the owner.

**Green means every check run on the head finished `success`, `skipped` or `neutral`.** Since #90, CI skips the jobs a PR's files don't need, so a docs-only PR shows skipped Rust jobs; those count as passing. So does a red check that isn't the PR's (next paragraph), outside `ci-ok`.

**A red check that isn't the PR's doesn't block** (owner, 2026-10-22, `docs/owner-decisions.md` §2.18). That means a check that's also red on `main`, or a known flake with a fix PR open, failing in code the PR doesn't touch. The runner counts it as passing and says so on the PR, naming the check, the evidence (the same check red on `main`, or the fix PR) and the fix PR if there is one. Anything else red is the PR's to fix.

**`ci-ok` is required, so §2.18 can't wave its jobs through** (§2.22). For a job inside `ci-ok` that is red for a reason outside the PR: if it's red on `main` too, fix `main` first (step 2 above), then bring `main` into the PR; if it's a flake, re-run the failed jobs once (`actions_run_trigger`, `rerun_failed_jobs`), and treat a second failure as real. §2.18 applies unchanged to checks outside `ci-ok`.
