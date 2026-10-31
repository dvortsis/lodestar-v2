# X-alert-search-headroom: real headroom for the Alert, Search results and Alerts routes
**Model:** Opus (web performance and the chunk graph). Runner brief, 2026-10-31. High priority: these gates are blocking feature work.

## Why
These entries sit within bytes of their limits on main (after #296, builds are deterministic apart from the 7-char SHA):

| Entry | Size | Limit |
| --- | --- | --- |
| Alert route | ~45,976–45,998 B | 46,000 B |
| Search results (lazy) | ~43,274–43,290 B | 43,300 B |
| Alerts route | ~29,164–29,168 B | 29,200 B |
| Results toolbar (lazy) | ~4,590–4,600 B | 4,650 B (raised, SHA noise) |

Any change to a shared chunk pushes them over. Recent examples:
- a new export in SvelteKit's runtime chunk renamed its minified exports (#299);
- registry `about`/`iconLoad` loaders (#300);
- shared date/format code splitting into these chunks (#300).

As a result, features are dropping UX. S-filters 2a shipped without a purpose line and without the queue's expiry time.

## Read first
- CLAUDE.md UI rules and `runner/standards.md`.
- `docs/architecture.md` §3 and §10.1 (budgets).
- `web/.size-limit.mjs`: how each of these entries is measured, i.e. which files count and the stoppers.
- `docs/decisions/2026-10-31-X-settings-registry-slim.md`, `-X-build-stable-name.md` and `-S-logs-2.md` (chunk re-split notes).
- The runner notes on `$app/environment`. For those, read `state.json` on the `runner-state` branch, key `runner_notes`.

## Do
1. **Measure what these entries actually include.** Use `npx size-limit --json`, the Vite manifest, and a bundle visualizer or source-map-explorer if available. List the biggest shared contributors to each, and which of them the route doesn't need on first view.
2. **Find real bytes, in this order:**
   - (a) Code the route doesn't need until interaction: move it to an on-demand import, as the repo does elsewhere.
   - (b) Shared chunks that bundle unrelated modules: use `manualChunks` or an import restructure so these routes stop sharing a chunk with the Settings registry, the format/date code and the runtime's churny exports. The goal is that Settings and registry changes can't move these routes at all.
   - (c) Plain dead code, or duplicate helpers.
3. **Target ≥ 0.5 KB headroom** on Alert, Search results and Alerts, and ≥ 0.1 KB on Results toolbar. If you can, **lower** the limits so the gate stays meaningful: new limit = measured + 10% of the cut, or keep the old limit if the headroom comes from the cut. Never raise a limit. If ≥ 0.5 KB isn't reachable, report what you got and why.
4. **Don't change behaviour or visuals.**
   - Run Lighthouse locally if you can. Load timing must not regress: no extra round-trip on first paint.
   - Baselines must not change. If any do, stop and tell the runner.
5. **Make it stick.** Add a short comment by these entries in `.size-limit.mjs` naming the isolation and what not to import into them. If a cheap test can catch a regression (for example, that the Alert route's chunk list doesn't include the Settings registry chunk), add it.
6. **Write a decisions file:** `docs/decisions/<date>-X-alert-search-headroom.md`. Include before/after bytes for every entry that changed.

## Own
- `web/vite.config.*` (chunking only) and `web/.size-limit.mjs` (these entries and comments).
- The specific imports in the Alert, Alerts and Search routes and components you restructure. Name them in the PR.
- A test, if added, and your decisions file.

**Don't touch** `features/settings/about/**` (S-about), `features/filters/**` (S-filters) or `features/settings/security/**` (#301). If a fix needs one of those, tell the runner.

## Size
≲ 500 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build`, then `npx size-limit` (twice; the result must be identical)
- `npx playwright test` core

Chromium is at /opt/pw-browsers; never run `playwright install`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the before/after table. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
