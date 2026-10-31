# S-about: Settings → About
**Model:** Sonnet (a Svelte screen, pattern-following). An Opus reviewer follows. Runner brief, 2026-10-31. It's the M5 backlog item from `runner/slices.yaml` (`S-about`).

## Read first
- CLAUDE.md UI rules and `runner/standards.md`.
- `docs/ui-glossary.md`.
- **`docs/architecture.md` §10.10.13 "About"** (the spec) and §10.19 (brand assets, credits).
- `runner/slices.yaml`, the `S-about` entry, and its notes on the registry and messages.
- How recent hand-built screens are built and split into lazy parts, so you follow the same pattern:
  - `docs/decisions/2026-10-31-S-logs-2.md`
  - `-S-logs-2b.md`
  - `-X-settings-registry-slim.md` (what a registry entry costs, and where purpose lines live now)
  - `-M-security-settings.md`
- The engine README.
- The APIs, in `web/src/lib/api/schema.d.ts`:
  - `/system` (version, build, `ui_build_id`, deployment)
  - `/updates` (read-only state for the About summary; the full Updates screen is S-updates)
  - diagnostics or support-bundle endpoints, if any
  - licences, if served
  - Use only what exists. For anything missing, leave a slot and say so.

## Do
1. **Build the About screen per §10.10.13**, using only what the API serves:
   - version and build, including the UI build id;
   - the stale-tab check: UI build ≠ server build means offering a reload;
   - a release-notes link;
   - the update status summary, linking to Updates;
   - project links;
   - a connections summary, if the spec lists it and an API serves it;
   - diagnostics: copy a diagnostics text, and the support bundle if the API has one. Follow §12.1 privacy: nothing identifying is copied without the user seeing it first;
   - licences and credits per §10.19.
2. **Registry entry:** one line, following the cost note in the registry file. Put the purpose line in the lazy `about.ts`, as #293 established.
3. **Sizes:**
   - The About first view must be ≤ 45 KB with ≥ 0.3 KB headroom. Licences, credits and diagnostics load on demand.
   - Other Settings first views may grow only by the measured registry-entry cost. State it in bytes.
   - Alert, Search results and Results toolbar must not grow; they're within a few bytes of their limits.
   - New entries go in at measured + 10%. Never raise an existing limit.
4. **Messages:** add `settings_about_*` keys in en and zh, and regenerate with the repo's generator.
5. **Tests:**
   - vitest for the stale-tab logic, diagnostics copy (it uses `copyText` from `$lib/util/clipboard`, so it works on plain-HTTP LAN), and the missing-API slots.
   - Playwright: the screen renders against the mock, keyboard-only, with axe.
   - Condition waits only.
   - Settings baselines will change because the nav gains About. Use the `web/e2e/.update-snapshots` marker, check that only Settings images change, then remove the marker.
6. **Decisions file:** `docs/decisions/<date>-S-about.md`.

## Own
- `web/src/lib/features/settings/about/**`
- its registry entry (one line)
- its message keys and regenerated maps
- its mock fixtures
- `web/e2e/settings-about.*`
- its size-limit entries
- your decisions file

**Don't edit:** the engine (`form/**`), other screens, `lib/state/**`, `lib/ui/**` or `lib/api/**`.

## Size
≲ 1,000 changed lines.

## Done when
These pass in `web/`:
- `npm run check`
- `npm run e2e:typecheck`
- `npx vitest run`
- `npm run build` then `npx size-limit`
- `npx playwright test` for the Settings specs (3× for new ones)

Chromium is at /opt/pw-browsers; never run `playwright install`. If the pinned headless shell is missing, a symlink to the installed one is fine.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number, the head and the size deltas. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
