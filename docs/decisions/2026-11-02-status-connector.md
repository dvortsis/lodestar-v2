# Roadmap dashboard status connector (owner-requested, 2026-11-02)

The owner asked for the roadmap dashboard to show progress from agents other than Claude, with a live connection to GitHub. `tools/status-connector/` adds a read-only MCP server (a Cloudflare Worker) that reads GitHub and returns the plan's tables. Nothing in the crawler, store, search or UI changes.

## Decisions
- **Progress comes from GitHub, not from the runner.** A branch `slice/<id>` or an open PR titled `<id>: …` means in progress; the PR merged into `main` means done. This is the Path B rule in `docs/handoff.md` §5, so any agent or person is counted the same. `runner-state/state.json` is read only as a fallback for older statuses, because it lags and only the Claude runner writes it (`docs/handoff.md` §2).
- **Attribution is by PR body and branch,** not by account, because every PR here is opened from the owner's account. A Claude Code footer or a `claude/` branch means Claude; anything else shows as "Not Claude".
- **The connector is read-only and authless, with the secret in the URL path.** claude.ai custom connectors take either OAuth or no authentication (runner §2.19 call, accepted). The GitHub token stays in the Worker, is read-only and is scoped to this repository. The URL exposes slice names and PR titles to anyone who has it.
  - **The path check fails closed.** If `PATH_SECRET` is unset, empty or shorter than 32 characters the Worker serves nothing, so `/mcp/undefined` and `/mcp/` are not open doors while the secret is being rotated. The comparison hashes both sides with SHA-256 and compares the digests.
  - **Progress is cached to protect the owner's GitHub quota.** Answers are cached in Cloudflare's Cache API, shared by every isolate in a data centre: 60 s for the first page of closed PRs and the other calls, ten times longer for older pages of closed PRs. Concurrent misses share one fetch, and a failed GitHub call is remembered for 30 s so a rate limit isn't hammered. Worst case is about 15 GitHub calls a minute per data centre against the token's 5,000 an hour. A Workers rate-limit binding was not added: it needs a Cloudflare plan setting and the URL secret is the access control. Revisit if the URL leaks.
- **It deploys from GitHub Actions** (`.github/workflows/status-connector.yml`) only when `tools/status-connector/` changes and the Cloudflare secrets are set. The workflow runs the tests on every PR that touches the folder. It uses no action other than `actions/checkout`, pinned to a commit as in `ci.yml`'s newer jobs (some older jobs there use `@v4` tags). Dependencies install in a step that has no Cloudflare credential in its environment; only the deploy step receives them, and it runs in a named environment (`status-connector-deploy`) so the owner can require approval.
- **Merged PRs are read across all pages of closed PRs** (up to 20 pages of 100), not the newest 400, so a merged slice doesn't drop out as the repository grows. A branch `slice/<id>` whose only PRs were closed without merging is not shown as in progress. A PR title may name several slices (`SRC-4/SRC-5: …`).
- **`package-lock.json` is committed,** so the deploy is reproducible. It makes this PR larger than the usual line target without adding code to review (stated reason, `CLAUDE.md` working conventions).

## Operating it
- **Rotate the URL secret:** `npx wrangler secret put PATH_SECRET` with a new random value of at least 32 characters (`openssl rand -hex 24`), then change the URL of the `Lodestar status` connector in claude.ai. Until both are done the dashboard falls back to its snapshot.
- **Replace the GitHub token:** create a new read-only fine-grained token for this repository, `npx wrangler secret put GITHUB_TOKEN`, then revoke the old one on GitHub.
- **If the URL leaks:** rotate the URL secret first. The worst a reader can do is read slice names and PR titles and spend GitHub API quota (see the caching limits above).
- **Stop it:** delete the connector in claude.ai and the Worker in Cloudflare, and revoke the token.

## Not decided here
- Whether the dashboard's plan structure (`src/meta.json`, the rules in `src/plan.ts`) should be generated from `docs/build-plan.md` instead of copied. Today they are hand-written and drift if the plan changes.
