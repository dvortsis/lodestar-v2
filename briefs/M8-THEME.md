# Brief: M8-THEME (Sonnet, helper), runner 2026-11-11

**Slice:** "Nightly all-themes failure opens or updates one tracking issue; run summary kept" (`runner/slices.yaml`, spec §10.17, §17).
**Branch:** `slice/M8-THEME`. **PR title:** `M8-THEME: <title>`. Follow `.claude/skills/steward/SKILL.md`; the runner merges.
**Owns:** `.github/workflows/e2e.yml` (the all-themes job, plus one new job next to it), and `docs/decisions/2026-11-11-M8-THEME.md`.

## What to build
1. **Keep the test job read-only.** `all-themes` (e2e.yml, `playwright (all-themes)`) keeps its current permissions; it never gets `issues: write`. It runs npm code, so it must not hold a write token.
2. **Run summary, every run:** the all-themes job writes a short summary to `$GITHUB_STEP_SUMMARY` (passed / failed / flaky counts and the failing spec titles, from Playwright's JSON reporter or the existing report), and uploads that summary as a small artifact on every run (`if: always()`, e.g. 30 days). The full report upload stays failure-only, as today.
3. **A new job `all-themes-issue`**, `needs: all-themes`, `if: always() && (github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')`, on `ubuntu-latest` with no container, no checkout of PR code and no npm. Job-level `permissions: { issues: write, contents: read }`; nothing else in the workflow gets `issues: write`.
   - On `needs.all-themes.result == 'failure'`: find the one open issue with a fixed label (e.g. `nightly-all-themes`) using `gh` and `GITHUB_TOKEN`; if it exists, add a comment (run URL, date in UTC, the counts and failing titles from the summary artifact); otherwise create it with that label. Never more than one open issue.
   - On `success`: if the tracking issue is open, comment "passed on <run URL>" and close it.
   - On `cancelled` or `skipped`: do nothing.
   - Treat the summary text as untrusted when building the comment: pass it through a file (`gh issue comment --body-file`), never through shell interpolation or `${{ }}` expression injection.
   - Create the label if it doesn't exist (`gh label create ... || true`).
4. Pin any action by full SHA with a version comment, as the rest of the file does. No new third-party actions are needed; prefer `gh`.
5. **The decisions file** records: the separate job and why (no write token near npm code), the label name, the close-on-pass behaviour, and that it isn't part of `ci-ok`.

## Checks
- `actionlint` if available (or `python3 -c "import yaml"` parse) on e2e.yml; the repo's own workflow lint if any.
- Dispatch is not possible from the session; say in the PR how it was checked (for example, a dry run of the issue script against a fake summary with `gh` stubbed).
- `/security-review` on the diff (token scope, expression injection) before reporting ready.
- Reply to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with "ready", the PR number and the full head SHA when CI is green.
