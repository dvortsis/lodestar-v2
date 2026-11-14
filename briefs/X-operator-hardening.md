# X-operator-hardening

**Runner brief, 2026-11-14. Opus, security-sensitive (operator channel, untrusted comment input).** From the owner-started security review of the cloud operator before first-run check 8. Findings are named by id, severity and surface only; the runner sends the detail by message (docs/security/v1/README.md §5).

Branch `slice/X-operator-hardening` from current main. PR title `X-operator-hardening: operator channel and tripwire hardening before check 8`. Target ≲ 1,000 changed lines.

## Findings in scope
| Id | Severity | Surface |
|---|---|---|
| OPERATOR-01 | important | tripwire sweep |
| OPERATOR-02 | important | channel thread reader |
| OPERATOR-03 | important | channel trust of bot-authored comments |
| OPERATOR-04 | important | injection test corpus (check 8) |
| OPERATOR-06 | to verify | App reach, tripwire trigger types |
| OPERATOR-07 | polish | wake approval gate |
| OPERATOR-08 | polish | injection harness realism (results note only) |

OPERATOR-05 is the runner's own handling of the channel; the runner has fixed it outside this slice.

## Owns
- `infra/operator/**` (code, tests, injection corpus and harness, `agent/system-prompt.md` only if a finding needs it)
- `.github/workflows/operator-tripwire.yml`, `.github/workflows/operator-wake.yml`: trigger types and a job summary only. No permission, secret, environment or actor-gate changes; those are the owner's.
- `docs/operations/cloud-operator-first-run.md` (check 7's list, check 8's corpus and reading the approval summary)
- `docs/decisions/2026-11-14-X-operator-hardening.md` (new), its `runner/dashboard.yaml` placement line

## Rules
- Every change fails closed or escalates; nothing may widen what a comment can trigger.
- Python tests under `infra/operator/tests` for each finding, with a fake transport; no network, no cloud calls, no AWS.
- A change to the agent's prompt or config needs the owner to dispatch operator-apply again after merge: say so in the PR.
- Never post to #821. Follow `.claude/skills/steward/SKILL.md`; `/security-review` before ready; message the runner "ready <PR> <sha>".
