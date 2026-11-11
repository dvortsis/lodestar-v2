# OP-4 (Opus, security-sensitive, /security-review): the operator agent and its bridge

Assigned by the runner on 2026-11-11.

## Read first
- The `OP-4` entry in `runner/slices.yaml`; its notes are binding (the job-level `if:` gate before `environment:` on every job that assumes the bridge role, because the OIDC trust accepts any event running a workflow from main; narrow the bridge's `locks/operator*` to exact keys; tripwire build requirements; extend OP-1's fake-value public-log test to `operator-wake.yml`).
- `docs/plan/2026-11-08-cloud-operator.md` §1-§4 and the OP-4 row (~389), the setup steps that depend on OP-4 (~335-360), and the containment rows (~294).
- The "act only on unedited GitHub text" rule in `.claude/skills/steward/SKILL.md` and `runner/README.md` (already on main).
- `docs/owner-decisions.md` §2.53, and the OP-1, OP-2a, OP-2 decisions files.

## Owns
`infra/operator/**` except `infra/operator/injection/**` (OP-5); `.github/workflows/operator-apply.yml`, `operator-wake.yml`, `operator-tripwire*.yml`; `docs/decisions/2026-11-11-OP-4.md`.

## Rules
- The runner never holds the Anthropic workspace key; nothing here needs it in tests or CI. No live call to any API (Anthropic, GitHub writes, AWS, Linode) in tests or CI, even with dummy credentials. No `terraform init` against S3.
- Every action pinned by full SHA; minimal `permissions:`; no PR-controlled string in `run:` or expressions; `sender.id` gate in the job-level `if:`.
- No `workflow_call` trigger, no reusable workflow call (the plan's ~137 rule); add the test.
- Nothing private in any log; fake account IDs only `111122223333`; no ARNs with real account IDs.

## Process
- Follow `.claude/skills/steward/SKILL.md` from first push to merge-ready. Branch `slice/OP-4` from the latest `origin/main`; PR title `OP-4: <title>`.
- Before review: mutation-check every guard you add (the last reviews were mostly about guards with no test). No fixed sleeps in tests; condition waits with deadlines of at least 10 s.
- Push once per round of fixes. Keep the PR under ~1,500 changed lines (generated files aside); if it grows past that, tell the runner before splitting.
- Anything beyond this brief (a new crate, a `Cargo.toml` change, a loosening of security or content safety, an owner-set number) goes to the runner first.
- When ready: message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `OP-4: PR #<n> ready for review, head <sha>` with CI green. Blocked: `OP-4: blocked on <what>`. The runner reviews and merges; you never merge.
