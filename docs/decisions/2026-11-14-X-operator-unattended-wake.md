# X-operator-unattended-wake decisions

## The owner's decision
- **Operator wakes no longer need the owner's per-wake approval** (the owner, in chat, 2026-11-14). The owner removed the Required reviewers rule from the `cloud-operator` GitHub environment. The next wake then failed closed before the lock: the deployment audit still required the owner as sole reviewer.
- **This loosens the OP-4 rule** that every secret-consuming operator job keeps explicit owner approval (`docs/plan/2026-11-08-cloud-operator.md` owner-setup step 4, `docs/operations/cloud-operator-first-run.md`). The owner made that call; this PR only brings the audit and the docs in line with it.

## The audit change
- **A new policy class, `unattended_environments`,** in `infra/aws/fleet-control/github-deployment-policy.json`, holds `cloud-operator` and nothing else. `infra/fleet/deployment_policy.py` reads it; no environment name is hard-coded in the audit.
- **For an unattended environment the audit accepts either:**
  - no `required_reviewers` rule; or
  - the owner-only rule as before: one rule, the owner's user id as its only reviewer, `prevent_self_review` explicitly `false`.

  So the owner can turn approval back on in GitHub's settings at any time, with no code change.
- **Everything else is still required for `cloud-operator`:** the selected-branches policy, exactly one branch rule (`main`, type `branch`, no tags or wildcards), and `can_admins_bypass` explicitly `false`.
- **Any rule type other than `branch_policy` and `required_reviewers` fails**, a wait timer included. A timer would queue wakes without anyone noticing, so it counts as drift.
- **Any reviewer rule that isn't owner-only fails:** another user, a team, a second reviewer, an empty reviewer list, or two reviewer rules.
- **Reads still fail closed.** A missing environment, a denied read, a `null` rule list or a non-object rule is reported as unreadable, and the bridge stops before it requests OIDC credentials.
- **An environment in both `owner_dispatch_environments` and `unattended_environments` fails** as a conflicting class rather than taking either branch.
- **Unchanged:** `cloud-fleet` and `pia-delivery` still need the owner as sole reviewer, because they gate paid VMs and PIA delivery. `fleet-cleanup` still admits no reviewer and no timer.
- **No stack update is needed.** `build_templates.py` reads only `repository`, `owner_id`, `repository_id` and `environments` from the policy file. `build_templates.py --check` passes, and the three CloudFormation templates are byte-identical to `main`'s (same SHA-256).

## Residual risk
- **Any session that can post as the owner on the channel PR can start a paid wake, with no human click.** Every session posts under the owner's login, so the runner's numeric id in the gate doesn't tell sessions apart. That was already true of who could ask for a wake; the approval click was the only point where the owner looked at each one. The caps below bound the cost.
- **`operator-apply.yml` and `operator-injection.yml` share `cloud-operator`, so they lose the approval click too.** Both keep their own job-level gate: `workflow_dispatch` only, on `main`, first attempt, `github.actor_id == vars.OWNER_USER_ID`, and `OPERATOR_STOP == 'false'`. Neither relied on the reviewer alone. But a session that can dispatch workflows as the owner's login now runs them unapproved:
  - `operator-apply` takes no inputs and applies only `main`'s committed agent, environment and vault definitions;
  - `operator-injection` runs sessions with a dummy token and no route, each with a $1 budget, inside the injection harness's $50 and the workspace's $200 total.

  Reported to the runner; nothing is widened here.

## Compensating controls, checked on `main` at this PR's base
- **The workflow gate runs before the environment.** `operator-wake.yml`'s `gate` job has no environment and no secrets. Its job-level `if:` admits only a `created` `issue_comment` on the channel PR (`vars.CHANNEL_PR`) whose author id equals `vars.RUNNER_USER_ID`. The `wake` job repeats those conditions, adds `main`, `run_attempt == 1` and `OPERATOR_STOP == 'false'`, and only then names `environment: cloud-operator`.
- **Wake caps:** 6 an hour and 60 a day (`infra/operator/wake.py` `PER_HOUR`, `PER_DAY`).
- **Per-session budgets:** $3 routine, $15 triage (`BUDGET_CENTS`), sent as the session's hard budget.
- **The $200 total** (`TOTAL_CAP_CENTS`): a wake that would pass it is refused.
- **The S3 operator lock** (`locks/operator.json`, If-None-Match): one session at a time.
- **`OPERATOR_STOP`:** unset or anything but `false` refuses every wake, in the workflow and again in `wake.py`.
- **The tripwire** (`operator-tripwire.yml`): disables `operator-wake.yml` when the App's bot acts anywhere but a new comment on the channel PR.

## Docs
- `docs/operations/cloud-operator-first-run.md`: wakes run without approval; check 2 reads the gate summary after the fact, and the owner may re-add themselves as reviewer to restore the wait.
- `docs/operations/fleet-control-bootstrap.md` and the plan's owner-setup step 4: `cloud-operator` needs no reviewer; `cloud-fleet` still does.
- **Not changed here (outside this slice's paths):** `infra/aws/fleet-control/README.md` line 15 still says the operator environment needs the owner as sole reviewer. Named in the PR for the runner.
