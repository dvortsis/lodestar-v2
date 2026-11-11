# Review: PR #655 OP-4 @ ca0fbf6d (fresh Opus + security review)
VERDICT: CHANGES (1 blocking)

## Tests
- infra/operator: 82 pass, 1 skipped (hypothesis not installed).
- test_workflow: 20 pass. fleet-control: 23 pass. test_ci_changes: 10 pass.
- 16 guards mutated; the tests caught all 16.

## Blocking
**B1.** wake.py:331-343 catches only (Exception, Refusal). An OIDC/STS failure raises a bare SystemExit (infra/fleet/apply.py:545-546), so it posts no answer, contradicting the decisions file.
- Fix: catch SystemExit there too, and add a test that raises SystemExit from store setup.

## Non-blocking
1. wake.py:381-385: the exception path archives without user.interrupt, so the session keeps spending its budget. Interrupt first when sid is set.
2. tripwire.py:283-287: a sweep API error exits red with no escalation, and an Incomplete list escalates but never disables.
3. The decisions line "the sweep is what catches it" overstates it for body edits on PRs whose base isn't main.
4. The agent's helper fetch is unauthenticated (system-prompt.md:11-14), so GitHub's rate limit can bite. Check in OP-5's first run.
5. The ledger's 50x100 page cap: over 5,000 sessions, every wake refuses for good. Needs a line in the decisions file.
6. Apply re-runs don't reconcile the credential's networking or injection_location, and a failed first run leaves orphans.
7. "Only the owner" applies really means any session under the owner's login. Harmless: apply takes no inputs.
8. .github/scripts/test_ci_changes.py isn't on the approved list.

## Security
- Workflows: actions pinned by SHA, minimal permissions, no event text in run:, sender.id gating, no workflow_call.
- Condition 4 met on every point.
- App token: scoped, revoked on a wider scope, JWT correct.
- Lock: a correct compare-and-swap.
- Agent: web tools off, networking limited to api.github.com, one header credential.
- IAM narrowed, exact keys, nothing widened.
- Only the fake account ID 111122223333 is committed.
