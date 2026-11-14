# X-operator-check-notes decisions (#PR)

These change the owner's first-run checklist (`docs/operations/cloud-operator-first-run.md`) after the first good round trip. The runner decided each one on 2026-11-14. Each tightens a check or says how to run it; none loosens one. No code, limit, gate or permission changes.

## Check 2: how the token is checked
- **The operator never prints the token.** Check 2's pass needs the operator to confirm that `GITHUB_TOKEN` in its sandbox is a placeholder. The checklist now says how: the operator prints only whether the value starts with `ghs_`, the prefix of a GitHub App installation token. The token part passes on `False`.
- **Presence and length are supporting evidence, not a pass.** A real installation token is 40 characters today, but GitHub doesn't promise a fixed length, so length alone can't rule one out.
- **Why a placeholder is expected:** the bridge stores the token as an `environment_variable` vault credential (`infra/operator/wake.py`). The Managed Agents vaults guide says the sandbox holds such a credential as an opaque placeholder, swapped for the real secret at egress. The operator's prompt says the same (`infra/operator/agent/system-prompt.md`).
- **Check 5 closes an open token item.** Its `/fleet status` instruction also asks the prefix question. `False` closes it; `True` stops the checks and goes to the runner.
- **Where check 2 stands:** passed except the token item, which is pending check 5. The 2026-11-14 round trip got a report from the operator's bot, unedited, that parsed. It answered with presence and length, which didn't match a GitHub token's length.

## Check 3c: `/fleet status`, not `/fleet up screen`
- **This supersedes one item of the plan's first-run test** (`docs/plan/2026-11-08-cloud-operator.md`, "Negatives": "the owner's login posting `/fleet up screen` is refused by `fleet.yml`").
- **Why `status`:** `fleet.yml`'s gate checks who posted before it looks at the command, so any command makes the same negative test. With `status`, a gate that failed open would cost nothing. With `up`, it could start paid infrastructure.
- **What passes is now stated in full:**
  - `fleet.yml`'s jobs skip.
  - In `operator-wake`, the gate and refuse jobs run and post nothing. The gate refuses the line as "not an operator instruction", the one reason the refuse job never answers (`REFUSE_REASONS` in `infra/operator/wake.py`). Wake and sweep skip.
  - Any comment from either workflow fails the check.

## Check 7: the App's approvals
- **If a ruleset here ever requires a review, the App's approvals must never count toward it.** The owner also checks that the App is on no ruleset's bypass list, and records what they saw.
- **Why:** the App can submit reviews (check 7 tests that the sweep sees them), so its approval would be the operator acting, not an independent review. The runner asked for this line after the bash decision (`2026-11-14-X-operator-bash-allow.md`), since `bash` on `always_allow` lets the operator write to any PR the token reaches.
