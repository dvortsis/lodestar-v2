# X-workflow-tests-filter decisions

Runner brief, 2026-11-22Z, from #1058's review (recorded in `2026-11-22-runner-records-r11.md`). Two workflow-reading tests could be skipped by a workflow change.

## The concurrency test now runs in the `changes` job (Option A)
- **The gap.** `.github/scripts/test_workflow_concurrency.py` reads `ci.yml`, `web.yml` and `e2e.yml`, but it ran only in the `lint` job, which needs `rust == 'true'`. A PR that changed only `web.yml` or `e2e.yml` leaves `rust` false, so it skipped the test.
- **The fix.** One step in `ci.yml`'s `changes` job, after the smoke-pilots unittest line: `python3 .github/scripts/test_workflow_concurrency.py`. That job runs on every pull request, with no condition, and already holds the other instant checks "here, where every PR runs, rather than behind a filter".
- **The duplicate in `lint` is removed,** so the test runs in exactly one place. `ci.yml` changes still reach it: `changes` runs on every event.
- **Why a `ci_changes.py` output can't do it.** An output only gates a job, and every job that could host the test sits behind `rust` or another filter. A new job would pay a runner start-up for a 0.1 s test.
- **Why not Option C** (the infra job loads and runs the test through `infra/fleet/tests/test_workflow.py`). It needs no `ci.yml` edit, but it couples two test trees, and it costs a web.yml-only PR a 6.5 to 7.5 minute infra run to execute a 0.1 s test. (#1058 already sends such a PR there.)
- **The runner approved the `ci.yml` edit** (its call under §2.26, flagged to the owner under §2.19). No job gained or lost an `if:`, and no permission, secret or trigger changed.
- **Extra CI cost:** about 0.1 s in the `changes` job on every PR and push, and no new job. It still runs in exactly one place, as before for a Rust PR.

## `.yaml` workflows are covered by the fleet policy check
- `infra/fleet/tests/test_workflow.py` globbed `*.yml`, so a `.yaml` workflow with a deployment environment escaped `test_all_deployment_jobs_pin_main_and_can_read_approval_settings`. GitHub runs both extensions.
- **`workflow_paths()` finds both,** and `unlisted_deployment_jobs()` is the check that a deployment job's file is named by its environment's policy. The existing test calls both.
- **A new class, `WorkflowExtensions`,** builds a temp folder with `other.yaml` (a `cloud-fleet` job), `plain.yml`, `notes.md` and `other.yaml.bak`. It expects exactly the two workflow files, and `other.yaml` flagged. A second test checks the real folder.
- The checks after the policy assertion are unchanged. They now run for `.yaml` files too, of which the repo has none.

## Tests in `test_ci_changes.py`
Class `ConcurrencyCheckRunsOnEveryPullRequest`, reading `ci.yml` as text:
- the script appears once in the file, as a plain one-line step in `changes`, and `changes` has no job-level `if:`;
- `lint` no longer carries it;
- a `web.yml`-only or `e2e.yml`-only PR still gets `rust=false` (and `infra=true` from #1058), so it is the always-on job that runs the test.

I checked them against variants of `ci.yml`. Without the new step the first test fails. With the lint line back (main's layout, or both places) both fail. A job-level or step-level `if:` on the step fails the first.

## Not done here
- No permission, secret or trigger change; #632 is untouched.
