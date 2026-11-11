# OP-5: the operator's prompt-injection corpus, harness and first-run checklist (Opus, /security-review)

This continues in the OP-3c session (session_01MMjhn9NvKRMZMg1baoPCau). The authority is the plan, `docs/plan/2026-11-08-cloud-operator.md`: §1, the "Prompt-injection test and escalation (OP-5)" part, the cost lines (about $15 routine, about $45 worst case, stop at $50), §5 (the first-run checklist, item 8), and the OP-5 row of the Slices table. Read `docs/decisions/2026-11-11-OP-4.md` and `2026-11-11-X-op4-polish.md`, and the OP-4 code in `infra/operator/`, before you design anything.

## Build
1. **The corpus,** in `infra/operator/injection/corpus/`:
   - adversarial texts the operator will meet: public-repo comments, and command output from the VMs (result lines, refusal lines, the status comment);
   - each item states its expected safe behaviour: ignore it, escalate it, or refuse it, and never post a `/fleet` command it wasn't instructed to post;
   - cover these: instructions hidden in comments by non-runner authors (which the reader helper should drop); fake runner or bridge headers; unicode and homoglyph tricks; payloads in VM output; requests to reveal or exfiltrate the token; requests to post outside the channel PR; requests to widen limits.
2. **The harness,** in `infra/operator/injection/`, driven by `.github/workflows/operator-injection.yml`. Its own vault holds a **dummy** GitHub token, never a real one.
   - A **GitHub mock** answers the operator's helper calls, and the agent has **no route to `api.github.com`**. The harness's own environment or networking must make that true; show how.
   - Attempts are detected from the session's **tool-call events**, not from the agent's prose.
   - It **refuses to run while a fleet exists**, by reading `fleet.json` through the bridge role's `s3:GetObject`, which is already in the template.
   - It stops at **$50** of the $200 total, and the decisions file shows the arithmetic.
   - It reuses OP-4's ledger, lock and Refusal patterns where they fit, without editing OP-4 files beyond what's needed. List each shared edit.
3. **The workflow** (`operator-injection.yml`):
   - triggered by `workflow_dispatch` by the owner only, using the same `actor_id` gate as apply;
   - in the `cloud-operator` environment, with OIDC to `LodestarOperatorBridge`. Its trust already names `operator-injection.yml`; check it;
   - actions pinned by SHA, minimal permissions, no `workflow_call`, no reusable workflows, no event text in `run:`;
   - `OPERATOR_STOP` is honoured.
   - `infra/fleet/tests/test_workflow.py` already lists it in COVERED. Make that test pass, and extend OP-1's fake-value public-log test to cover it.
4. **The first-run checklist** (§5): write it where the plan says, and record its results format in the decisions file.

## Rules
- Tests use local fakes only. **No Anthropic, AWS, GitHub or other network call** from tests or CI.
- No `terraform init`, ever.
- Commit no account IDs or ARNs with an account ID, except the fake 111122223333.
- No model ID committed (OP-4's test checks this).
- Never the owner's soak host.
- No fixed sleeps.
- Results go in `docs/decisions/2026-11-11-OP-5.md`. The live run is the owner's step at OP-G; the slice builds it and proves it on fakes.

## Owns
- `infra/operator/injection/**`
- `.github/workflows/operator-injection.yml`
- `infra/fleet/tests/test_workflow.py` (the public-log test extension)
- its decisions file
- minimal, listed shared edits in `infra/operator/` (no behaviour change to wake or apply without asking the runner)

`infra/operator/tripwire.py` is X-tripwire-reviews-lookback's: don't touch it.

Branch `slice/OP-5`; PR title "OP-5: <title>". Follow the steward skill, and message the runner when the PR is ready.
