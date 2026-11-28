# X-detail-attributes (runner brief, 2026-11-28)

**Slice:** X-detail-attributes (runner/slices.yaml). Opus, core: untrusted-input parsing reaches
an API contract.
**Branch:** `slice/X-detail-attributes` from current `main`.
**PR title (exact):** `X-detail-attributes: GET /torrents/{hash} carries the parsed release attributes`

**Owns (refine at step 0):**
- `crates/api/src/torrents.rs`
- `crates/api/src/dto/torrents.rs`
- `crates/api/tests/golden/**` (re-blessed only)
- `web/src/lib/api/openapi.json` and `web/src/lib/api/schema.d.ts` (regenerated with the repo's tooling, never by hand)
- `docs/decisions/*-X-detail-attributes.md`

## Why
- Per-torrent parsed release attributes (resolution, codec, source, year and the like) exist
  today only in `POST /compare`, so the detail view can't show them.
- Runner records r14 (`docs/decisions/2026-11-23-runner-records-r14.md`) placed this after
  X-detail-depth's step 0.
- Use the same parser and the same attribute shape as `/compare` and X-hit-resolution's SearchHit
  field, so one torrent reads the same everywhere.

## Do
1. **Step 0.** Send the runner:
   - the field shape;
   - where the parse runs (at request time, or a stored column?);
   - its cost against the detail route's latency budget (`docs/metrics.md`; measure it);
   - the refined owns, including any store or parser file.

   Wait for the reply.
2. Fields are optional or null when absent. The detail UI must not break before it uses them.
   Showing them in the UI is not this slice's job, unless step 0 shows it's a few lines.
3. **Tests:**
   - API unit tests;
   - re-blessed goldens;
   - an openapi drift check;
   - a test that a hostile or odd name (very long, odd Unicode) is handled within budget.

   The parser already has property tests. Extend them if you touch it.
4. **Shared files.** X-vpn-bind (#1167) is about to merge and regenerates openapi.json and
   schema.d.ts. If it lands first, bring main in and regenerate. Never hand-merge them.
5. Decisions file: `docs/decisions/<date>-X-detail-attributes.md`.
6. Follow `.claude/skills/steward/SKILL.md` and the Rust pre-push checks in CLAUDE.md. Report
   `ready <PR> <sha>` to the runner session.
