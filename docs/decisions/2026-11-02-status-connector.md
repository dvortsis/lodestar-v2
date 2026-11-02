# Roadmap dashboard status connector (owner-requested, 2026-11-02)

The owner asked for the roadmap dashboard to show progress from agents other than Claude, with a live connection to GitHub. `tools/status-connector/` adds a read-only MCP server (a Cloudflare Worker) that reads GitHub and returns the plan's tables. Nothing in the crawler, store, search or UI changes.

## Decisions
- **Progress comes from GitHub, not from the runner.** A branch `slice/<id>` or an open PR titled `<id>: …` means in progress; the PR merged into `main` means done. This is the Path B rule in `docs/handoff.md` §5, so any agent or person is counted the same. `runner-state/state.json` is read only as a fallback for older statuses, because it lags and only the Claude runner writes it (`docs/handoff.md` §2).
- **Attribution is by PR body and branch,** not by account, because every PR here is opened from the owner's account. A Claude Code footer or a `claude/` branch means Claude; anything else shows as "Not Claude".
- **The connector is read-only and authless, with the secret in the URL path.** claude.ai custom connectors take either OAuth or no authentication. The GitHub token stays in the Worker, is read-only and is scoped to this repository. The URL exposes slice names and PR titles to anyone who has it.
- **It deploys from GitHub Actions** (`.github/workflows/status-connector.yml`) only when `tools/status-connector/` changes and the Cloudflare secrets are set. The workflow runs the tests on every PR that touches the folder. It runs no third-party actions beyond `actions/checkout`, pinned like the other workflows.
- **`package-lock.json` is committed,** so the deploy is reproducible. It makes this PR larger than the usual line target without adding code to review (stated reason, `CLAUDE.md` working conventions).

## Not decided here
- Whether the dashboard's plan structure (`src/meta.json`, the rules in `src/plan.ts`) should be generated from `docs/build-plan.md` instead of copied. Today they are hand-written and drift if the plan changes.
