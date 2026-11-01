# X-e2e-fixture-ci: run the fixture specs nightly against the real server
**Model:** Sonnet (CI workflow following the existing `all-themes` nightly job). The runner reviews. Runner brief, 2026-11-01. It closes the S-e2e-2 (#320) follow-up.

## Why
#320 added an opt-in Playwright `fixture` project for `web/e2e/*.fixture.test.ts` (detail, search-facets, search-keyboard, search-query, shortcuts). They test the UI against a real `lodestar serve --fixture` server, but no CI job runs them, so they can rot. `docs/decisions/2026-11-01-S-e2e-2.md` ("Follow-up: a fixture CI job") estimates the cost and recommends nightly.

## Read first
- `docs/decisions/2026-11-01-S-e2e-2.md`, `web/e2e/README.md` (the local fixture run: the server command, `E2E_FIXTURE_URL`), `web/playwright.config.ts` (the `fixture` project).
- `.github/workflows/e2e.yml` (the `all-themes` job: nightly `schedule` + `workflow_dispatch`, the Playwright container), `.github/workflows/ci.yml` (how the Rust binary is built and cached; `Swatinem/rust-cache` or equivalent).
- How `lodestar serve --fixture` (or the fixture generator) works: search `--fixture` under `crates/`.

## Do
1. **A `fixture` job in `e2e.yml`** that runs on the nightly `schedule` and on `workflow_dispatch` (not on pull requests), like `all-themes`.
   - Build the binary in a separate job on `ubuntu-latest` (`cargo build -p lodestar --release`, with the repo's existing rust-cache setup), upload it as an artifact, and download it in the Playwright job. Don't add a Rust toolchain to the Playwright container.
   - Start the server with the fixture data on a local port, wait for its health endpoint with a deadline (≥ 30 s, condition wait, no fixed sleep), set `E2E_FIXTURE_URL`, run `npx playwright test --project=fixture`, upload the report on failure, and stop the server.
   - Pin actions the way the other jobs do (full SHAs).
2. **Don't add it to `ci-ok`** or any required check. It's nightly.
3. **Prove it once:** after the PR is open, trigger the workflow with `workflow_dispatch` on your branch if you have the means (the GitHub MCP `actions_run_trigger`), or ask the runner to. Report the run's result and duration in the PR. If a fixture spec fails because it rotted, fix the spec only if the fix is small and clearly a test problem; otherwise list it in the PR for a follow-up and say so to the runner. Never skip or disable a spec.
4. **Docs:** update `web/e2e/README.md` (where the job runs, how to dispatch it). A short decisions file `docs/decisions/2026-11-01-X-e2e-fixture-ci.md`.

## Own
`.github/workflows/e2e.yml` (the new jobs only), `web/e2e/README.md`, the fixture specs (small rot fixes only), your decisions file.
**Don't edit** `ci.yml`, `web/src/**`, `crates/**`, or other e2e specs.

## Size
≲ 200 changed lines.

## Done when
- `actionlint` (if available locally) or a YAML parse of the workflow passes; `npm run e2e:typecheck` passes.
- One dispatched run of the new job, result reported.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the dispatched run's result. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
