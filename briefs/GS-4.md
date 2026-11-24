# GS-4: Detail drawer: Related (other versions, identical clones, same file, similar payloads)

**Runner brief, 2026-11-24 (UTC).** Helper slice, Sonnet. Svelte only. Target < 1,200 changed lines.
- **Branch:** `slice/GS-4` from fresh `origin/main` (or the branch the harness fixes).
- **PR title:** `GS-4: Detail drawer: Related (other versions, identical clones, same file, similar payloads)`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Why
GS-2 built the siblings producer and `GET /torrents/{hash}/siblings` (`crates/api/src/compare.rs`, `crates/api/src/dto/compare.rs`); GS-3 and GS-5 shipped collapse-by-group in search. The drawer still has no Related section, so GS-5's "Show all N" opens the drawer as a stand-in until this lands (GS-5 owner flag).

## Read first
- `docs/architecture.md` §10.5 and §10.14; `docs/design-tokens.md`, `docs/ui-glossary.md`, `docs/metrics.md`.
- The slice's notes in `runner/slices.yaml`, and the UI plan for unbuilt modules (build-plan §0 item 7, part 2 section 5, "UI review 02 §5.1"); `docs/decisions/2026-11-20-X-ui-plan-r5.md`.
- `docs/decisions/2026-11-12-GS-2.md`, `2026-11-16-GS-3.md`, `2026-11-24-GS-5.md` (how Show all reaches the drawer today).
- The siblings DTO and `web/src/lib/api/openapi.json`; the drawer's section list in `web/src/lib/features/detail/` and how other sections (e.g. the ENR-8 adult-enrichment section) mount; `/compare` and how it takes hashes.
- `runner/standards.md`.

## What to build
1. A Related section in the detail drawer, mounted with one line in the drawer's section list (shared file: that one line only).
2. Four labelled lists with counts: other versions, identical clones, same file, similar payloads. Empty lists say so briefly or hide, per the UI plan.
3. Each row shows the attributes that differ from the open torrent, not every attribute.
4. "Compare these" pre-fills `/compare` with 2 to 5 picked torrents; disabled outside that range, with the reason in words.
5. Honour Show adult off and the API's 404 for hidden items: never show what the API withheld. Loading, error and retry states in our own words.
6. If GS-5's Show all should now open Related directly, wire that through the existing drawer API; if it needs a change in `web/src/lib/features/search/collapse/**`, ask the runner first.
7. i18n keys in en and zh; tokens only (no hex, no arbitrary values); keyboard reachable; size budget per `size-limit` (add an entry for the section if it's lazily loaded; size calls under §2.26 go in the decisions file).
8. Unit tests (vitest) and an e2e spec under `web/e2e/` for the four lists and Compare these. No fixed sleeps.
9. Add this slice's placement rule in `runner/slices.yaml` only if none covers it; run `cd tools/status-connector && npm test` if you touch it.

## Owns
`web/src/lib/features/detail/related/**`, one mount line in the drawer's section list, its e2e spec, its size-limit entry, and `docs/decisions/<date>-GS-4.md`. Anything else: ask the runner.

## Done
In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npm run size:ci`, `npx playwright test` for your spec with `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium` (never `playwright install`). Report `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green.

No AWS or cloud calls, no new accounts, no spending, no force-push.
