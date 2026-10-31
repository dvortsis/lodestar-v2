# X-build-stable-name: deterministic web builds so the size gates stop flapping
**Model:** Sonnet. Build configuration, pattern-following. An Opus reviewer follows. Runner brief, 2026-10-31.

## Why
SvelteKit's `kit.version.name` defaults to the build time. The build also embeds an app id (`__sveltekit_<id>`). Both change on every build, which moves hashed chunk names and shared-chunk bytes, so every size-limit entry wobbles by tens of bytes between identical builds.

Three entries now sit inside that noise on main:
- Alert route: about 45.97–46.01 KB against a 46 KB limit.
- Search results (lazy): about 43.27–43.30 KB against 43.3 KB.
- Results toolbar (lazy): 4,593–4,603 B against 4,600 B. It already fails on some main builds.

The #293 reviewer confirmed that these chunks are byte-identical apart from the version string and hashed names.

## Read first
- `web/svelte.config.js`.
- How the app uses the version: grep `version`, `updated`, `__sveltekit`, `$app/environment` and `version.json` in `web/src`.
- The update-available or reload-on-new-version logic, if any.
- `web/.size-limit.mjs`.
- The release and web workflows, for how a version or commit SHA is available at build time.

## Do
1. Set `kit.version.name` to a stable value.
   - Prefer the package version plus the commit SHA, from an env var that CI and the release workflow set, such as `GITHUB_SHA`.
   - Fall back to the package version for local builds.
   - Two builds of the same commit must produce byte-identical JS. Make the embedded app id deterministic too, if SvelteKit allows it. If it doesn't, say so with the numbers.
2. Keep update detection working.
   - A new release must still be detected as a new version. Check `version.pollInterval` and the `updated` store, if used.
   - Document what changes for dev builds.
3. Measure. Build twice on the same commit, run `npx size-limit` twice, and report the byte spread per entry before and after. The goal is 0 B spread.
   - Never raise a limit.
   - If Results toolbar still fails deterministically after this, report its exact size. The runner decides.
4. Tests:
   - A small vitest or node check that the configured version name is non-empty and stable.
   - `npm run check`, `npx vitest run`, `npm run build` and `npx size-limit` pass.
   - No Playwright baseline changes are expected.
5. Decisions file: `docs/decisions/<date>-X-build-stable-name.md`.

## Own
- `web/svelte.config.js`.
- A tiny version helper, if needed.
- The workflow env lines that pass the SHA in. Name them in the PR.
- Its test and your decisions file.

**Don't touch:**
- `web/src/lib/features/settings/**`, where S-logs-2b and X-form-remount are running.
- `.size-limit.mjs` limits.

## Size
≲ 150 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npx vitest run`
- `npm run build`, then `npx size-limit`, twice, with the spread reported

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, head and spread table. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
