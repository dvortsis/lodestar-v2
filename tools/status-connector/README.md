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

## What it reads, and what you edit
The connector reads three things live from GitHub and caches them for a minute:
1. `runner/slices.yaml`: the slices, their dependencies and the `ext:` conditions.
2. **`runner/dashboard.yaml`**: everything GitHub can't show. Phases and workstreams, which workstream each slice belongs to (`placement`), which outside conditions hold (`conditions`), the owner queue, the gates, the roadmap steps and arrows, the top-of-page notes, the crawler rate and the soak timeline. Its header says which key fits which change.
3. Pull requests, branches and tags.

So a plan change needs **no redeploy**. Edit `runner/dashboard.yaml` in the same pull request as the change, and the dashboard follows within a minute. This is a repository rule: see "Keeping the dashboard current" in `CLAUDE.md`.

What follows by itself, with no edit:
- Progress: a branch, a pull request, a merge.
- A condition with `auto: tag:<name>` or `auto: branch:<name>` (for example `v051-tagged`) flips when that tag or branch appears on GitHub.
- A gate with `slice:` takes its status from that slice.
- A new slice appears. If no `placement` rule covers it, it lands in "Planned follow-ups", and the dashboard shows a warning naming it.

What needs a one-line edit to `runner/dashboard.yaml`: an owner decision made (`met: true`), a new workstream or roadmap step, an epic that has been cut into slices (`expanded:`), a gate measurement, the notes.

## The check that keeps it honest
```bash
npm test
```
`test/consistency.mjs` compares the real `runner/dashboard.yaml` with the real `runner/slices.yaml` and fails on any drift: a slice no rule covers, an `ext:` condition with no entry, a roadmap step or gate naming a slice that no longer exists. CI runs it on any pull request that touches either file, and the dashboard shows the same problems as a warning at the top. `test/run.mjs` runs the handler against mocked GitHub and a fixture.

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
- Tools (none takes arguments): `plan_nodes`, `plan_road`, `plan_edges`, `plan_activity`, `plan_recent`, `plan_meta`, `plan_gates`, `plan_owner`, `plan_ext`, `plan_notes`, `plan_crawler`, `plan_soak`.

## Tests
```bash
npm test
```
Runs the consistency check and the handler against mocked GitHub. It needs no network and no secrets.
