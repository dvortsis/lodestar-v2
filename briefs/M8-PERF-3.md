# M8-PERF-3 (runner brief, 2026-11-28)

**Slice:** M8-PERF-3. Opus: security-sensitive CI on a public repository. `/security-review`
required.
**Branch:** `slice/M8-PERF-3` from current `main`.
**PR title (exact):** `M8-PERF-3: The 10M benchmark runs on a dedicated runner for main and release branches only`
**Owns:** `.github/workflows/bench-10m.yml`, `.github/workflows/runner-guard.yml`,
`.github/scripts/runner_guard.py`, `.github/scripts/test_runner_guard.py`,
`docs/operations/bench-runner.md`, `docs/decisions/*-M8-PERF-3.md`. **Not** `.github/workflows/ci.yml`:
X-ci-test-shard owns it now.

## Why now
The runner's plan review (2026-11-28) puts M8-PERF-3 on the path to G-v1-10m and M8-SEC-5. Its
setup page is also what the owner needs in order to register the dedicated runner
(`ext:bench-runner`). Starting now keeps that owner action off the end of the release.

## Source
- `runner/slices.yaml` M8-PERF-3.
- `docs/plan/2026-11-06-m8-hardening.md` §5 OQ1, with the owner's chosen mitigations (a)–(d)
  (`docs/owner-decisions.md` §2.46–§2.47):
  - **(a)** an isolated machine registered as an ephemeral runner;
  - **(b)** a ruleset on `release/*`;
  - **(c)** this slice's `runner-guard` check;
  - **(d)** the fork-PR approval setting. **Not (e).**
- M8-PERF-1's bench mode (`bench/README.md`) is what the job runs.

## Do
1. **Step 0.** Propose:
   - the workflow triggers: `push` to `main` and `release/*`, plus `workflow_dispatch` limited to
     those refs, and never `pull_request` or `pull_request_target`;
   - the runner label;
   - permissions (`contents: read` only), and no secrets;
   - how the job handles the dataset cache on an ephemeral runner;
   - the report artifact;
   - the guard's rule: fail any other workflow that names the label, or any trigger that could
     run untrusted code on it;
   - the setup page's outline.

   Send it to the runner and wait for the reply.
2. **The setup page** (`docs/operations/bench-runner.md`) is written for the owner. Follow the
   user-docs skill's tone: plain steps, the safe choice first, and why each mitigation matters.
   Steps 1, 2 and 4 are the owner's:
   1. the isolated machine;
   2. the ephemeral registration with the label;
   3. the `release/*` ruleset;
   4. the fork-PR approval setting;
   5. a first manual run and what a pass looks like.
3. **Tests.** `test_runner_guard.py` covers:
   - a workflow naming the label in any form: string, list, `runs-on` group or expression;
   - `pull_request` / `pull_request_target` on a labelled job;
   - the bench workflow itself allowed.
4. `/security-review` on the workflow diff, a decisions file, `.claude/skills/steward/SKILL.md`, and
   report `ready <PR> <sha>`.
