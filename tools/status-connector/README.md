# Lodestar status connector

A small read-only MCP server that runs as a Cloudflare Worker. It reads GitHub and gives the roadmap dashboard (a Claude dashboard artifact) its live data. It writes nothing to GitHub or anywhere else.

## Why it works for any agent
It doesn't ask agents to report progress. It reads the trace every agent leaves:

| What GitHub shows | What the dashboard shows |
|---|---|
| A branch `slice/<id>`, or an open pull request titled `<id>: <title>` | The slice is **in progress** |
| That pull request merged into `main` | The slice is **done** |
| A slice in `runner/slices.yaml` with neither | **Ready**, **waiting** or **waiting on you**, from its `deps` and `ext:` conditions |

`<id>` is the slice id in `runner/slices.yaml`, for example `SRC-4`. This is the rule in `docs/handoff.md` §5 (Path B), so Claude, other vendors' agents and people all show up the same way. A pull request counts as Claude's when its body carries the Claude Code footer or its branch starts with `claude/`; anything else shows as "Not Claude".

`runner-state/state.json` is read when it exists (it carries the runner's older statuses). Nothing needs it.

## Where the plan structure lives
`src/meta.json` holds the phases, workstreams, roadmap steps and outside conditions, and `src/plan.ts` holds the rules that place each slice in a workstream. They are a hand-written copy of `docs/build-plan.md` §0. A slice the rules don't recognise lands in "Planned follow-ups". Update both when the plan changes.

## Deploy from GitHub (recommended)
`.github/workflows/status-connector.yml` tests the connector on every pull request that touches it and deploys it when such a change reaches `main`. Set these once, under Settings, Secrets and variables, Actions:

| Repository secret | Value |
|---|---|
| `CLOUDFLARE_API_TOKEN` | A Cloudflare API token from the "Edit Cloudflare Workers" template |
| `CLOUDFLARE_ACCOUNT_ID` | Your Cloudflare account id (Workers & Pages, right-hand side) |

The deploy runs in a GitHub environment named `status-connector-deploy`. GitHub creates it on the first deploy; you can add required reviewers to it under Settings, Environments. Without the two secrets the workflow still runs the tests and skips the deploy. The Worker's own two secrets (below) are kept in Cloudflare and survive redeploys.

## Deploy from a terminal
You need Node 20 or newer and a free Cloudflare account. From this folder:
```bash
npm ci
```
```bash
npx wrangler login
```
```bash
npx wrangler secret put GITHUB_TOKEN
```
```bash
npx wrangler secret put PATH_SECRET
```
```bash
npx wrangler deploy
```
- `GITHUB_TOKEN`: a fine-grained token for this repository only, with Contents (read-only) and Pull requests (read-only).
- `PATH_SECRET`: a long random string of at least 32 characters, for example the output of `openssl rand -hex 24`. It is the last part of the connector URL and works as its password. The Worker serves nothing while it is unset or shorter than 32 characters.

## Add it to claude.ai
Settings, Connectors, Add custom connector.
- Name: `Lodestar status` (exactly; the dashboard looks for this name)
- URL: `https://lodestar-status.<your-name>.workers.dev/mcp/<PATH_SECRET>`, with no sign-in.

## Limits
- Read-only. The token can't write and the server never calls a write endpoint.
- Answers are cached in Cloudflare for 60 seconds (older pages of closed pull requests for ten minutes), and a GitHub error is remembered for 30 seconds. See `docs/decisions/2026-11-02-status-connector.md` for the quota limits and how to rotate the secret and token.
- Anyone with the URL can read slice names and pull request titles. Keep it private. To stop it, delete the connector in claude.ai and the Worker in Cloudflare, and revoke the token.
- Tools: `plan_nodes`, `plan_road`, `plan_activity`, `plan_recent`, `plan_meta`. None takes arguments.

## Tests
```bash
npm test
```
Runs the real handler with GitHub mocked and this repository's own `runner/slices.yaml`. It needs no network and no secrets.
