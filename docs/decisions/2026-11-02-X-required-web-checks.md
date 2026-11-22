# X-required-web-checks decisions

`web.yml` and `e2e.yml` gain always-reporting aggregate jobs so the owner can require them in the ruleset (`docs/owner-decisions.md` §2.28, `docs/build-plan.md` §0.3 item 4). Nothing in `ci.yml`, `ci-ok`, the lineage guard, `release.yml` or `freshness.yml` changes.

## Decisions
- **No workflow-level path filter on `pull_request`.** A `changes` job in each workflow runs `ci_changes.py` (new `web` and `e2e` outputs) and the heavy jobs skip when nothing applies. The `push: main` filter in `web.yml` stays, since nothing requires a push check.
- **Web applies to:** `web/**` (includes `openapi.json`, which `api:types` is checked against); `brand/source/**` (assets built into the UI); the files web tests read outside `web/` (`crates/lodestar/src/settings/golden/`, `settings/preferences.rs`, `crates/search/src/query.rs`, `docs/design-tokens.md`, `docs/themes.md`); `ci_changes.py`; `web.yml`.
- **E2E applies to everything web does, plus `crates/api/**`** (the contract `openapi.json` comes from) and `e2e.yml` instead of `web.yml`. Docs, other crates and other workflows never apply. Changing `ci.yml` alone does not.
- **`web-ok` / `e2e-ok` fail** on any failed or cancelled job, and on a job that applies but was skipped. They pass when the jobs passed or none applies. `e2e-ok` runs on pull requests only; nightly `all-themes` and `fixture` stay out of it.
- **A regeneration never counts.** `e2e-ok` fails when `e2e` output `updated=true` (marker file) and when `commit-baselines` did anything but skip. Comparison and regeneration share one job as before; only the aggregate separates them.
- **Required check names to add:** `web-ok` and `e2e-ok`. Do not require the old job names (they skip on docs-only PRs).
- **A crash in change detection fails the check.** The step uses `shell: bash` (pipefail) and each aggregate fails unless the output is exactly `true` or `false`, so an empty output can't skip the heavy jobs and pass.
- **`web.yml` has no push path filter;** main always runs web.
- `test_ci_changes.py` covers the rules, including a docs-only diff, and runs in each `changes` job.
