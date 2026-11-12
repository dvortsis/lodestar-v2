# X-settings-section-headroom: room under the Settings section's 45 KB cap

**Runner brief, 2026-11-12 (UTC).** Web, small. Run by the UP-1 session as its own PR, before #765 continues.
- **Branch:** `slice/X-settings-section-headroom` from fresh `origin/main`.
- **PR title:** `X-settings-section-headroom: <title>`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Why
"Settings section (own JS + CSS)" in `web/.size-limit.mjs` is held at exactly '45 KB', the §10.1 route cap the owner set. `main` measures about 44.98 KB, so the one `LOADERS` line any new settings section adds (UP-1's `watch`) puts it 2 B over. A lazy `import.meta.glob` for `LOADERS` measured 64 B worse (45066 B), so it's not the route.

## Goal
Make **at least 256 B of headroom** in the Settings section's first view (route, `SectionForm` and the engine), measured by `npx size-limit`, with no change in behaviour. That covers UP-1 and the next few sections.

## Rules
- The '45 KB' line and its entry's file set stay untouched. Don't move code out of the measured set by changing what the gate counts. Moving code to a chunk that genuinely loads later (on demand, after first view) is fine if a later-loaded entry gates it and its tests still pass.
- No behaviour change, no removed tests, no lowered coverage. No new dependency.
- Spend a little time measuring first: find where the first view's bytes go (the build's chunk sizes, the gzip size of each module), then pick the cheapest real savings. Examples, not orders: dead or duplicated helpers, strings built at run time that could be shorter, code only some screens use that loads eagerly.
- Any other `.size-limit.mjs` limit that moves must go down, never up.

## Done
- `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, and `npx playwright test` for the settings specs.
- The PR states the before and after bytes for "Settings section" and for every entry that changed.
- A decisions file, `docs/decisions/2026-11-12-X-settings-section-headroom.md`: what moved and why it's safe.
- Send the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green. A fresh Opus review follows. Then bring `main` into `slice/UP-1` (merge, never rebase) and continue #765.
