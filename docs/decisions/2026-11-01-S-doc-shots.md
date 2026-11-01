# S-doc-shots decisions

- **Opt-in `docs` Playwright project** (`web/e2e/docs/*.docs.ts`, `npm run docs:shots`): off unless `--project=docs` or `E2E_DOCS=1`, like `fixture` and `all-themes`, so the default project list and CI are unchanged. It writes `docs/user/images/` over the mock fixtures. Reason: the guide must show the real UI (§2.14) and the manual step goes stale.
- **Riverstone light at 1366 wide, plus dark for the guide's existing `<picture>` pairs.** Chapter 5 already references `-dark` images, so dropping them would break the guide; every other shot is light only. A refinement of the one-theme rule, flagged to the owner (§2.19).
- **Deterministic by construction:** frozen clock, fonts and page height settled before the shot, animations off, palette-quantised PNGs at most 1366 wide (sharp, already a dev dependency). Two runs give byte-identical files. Total size about 0.5 MB against the 5 MB budget; plain files, no LFS.
- **Element outline** uses `--focus-ring-width` and `--color-primary` (no hex) on the control a step refers to.
- **Not in scope:** a CI check that the images are current. UI PRs regenerate them by hand (`.claude/skills/user-docs/SKILL.md` §4).
