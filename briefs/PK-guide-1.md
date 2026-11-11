# PK-guide-1 (Sonnet, docs): port the v0.5.1 guide changes to main, and clean up the hot and cold path leftovers

Assigned by the runner on 2026-11-11 to the session that built PK-unraid (#680, merged as 1fe44a29). The slices.yaml entry is PK-guide-1. Use the user-docs skill, which builds on github-docs.

## 1. Check first: what is still unported
- D-v051-main (#574, merged as 76189f4f) already carried most of release/v0.5.1's docs to main. Diff `origin/release/v0.5.1` against `origin/main` for `docs/user/` and `docs/setup.facts.md`, and list what is truly missing on main.
- Main's guide is newer: 5.4, 5.5 and later revisions stay. **This is a port merged into main's guide, not a copy of the release branch.** Most of that diff is main being ahead (new PNGs, deleted files); don't revert any of it.
- If almost nothing is missing, say so in the PR and keep the PR small. Don't invent work.

## 2. Do
1. Port only what's missing, plus "when full speed starts" and the release status, using plan Q12 wording that matches PK-ui's. Grep docs/ and `runner/` for Q12.
2. **Hot and cold path leftovers** (the runner adds these to your owns):
   - `docs/user/appendix-b-settings-reference.md`: remove the Hot path and Cold path rows at :11-12, so it matches chapter 2.
   - `docs/user/revisions.md`: one row for #680's chapter 2 change and one for this PR.
   - `docs/decisions/2026-11-11-PK-unraid.md:1`: heading `# PK-unraid decisions (#680)`.
3. Don't edit `docs/operations.md` (PK-ops), `docs/architecture.md`, `runner/` or any Rust. Leave the CI job for validate.py too; the runner tracks those.

## Owns
- The slices.yaml PK-guide-1 owns (README, 01, 02, 03, 05, 06, glossary, index, revisions, appendix-a, setup.facts.md);
- plus `docs/user/appendix-b-settings-reference.md` and `docs/decisions/2026-11-11-PK-unraid.md`;
- plus a decisions file `docs/decisions/2026-11-11-PK-guide-1.md`.

## Bar
- `docs_tool.py sync` and `sync --check`, and `lint_docs.py --strict` with 0 failed and 0 warnings.
- No dangling anchors. No 12-digit numbers or personal data.
- Branch `slice/PK-guide-1`, PR `PK-guide-1: <title>`, steward skill.
- Reply "ready" with the head when CI is green.
