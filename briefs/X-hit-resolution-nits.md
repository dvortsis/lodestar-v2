# X-hit-resolution-nits (runner brief, 2026-11-23Z)

Core slice, Opus, a fresh Opus review before merge. rv1079's optional findings on X-hit-resolution (#1079), placed by `docs/decisions/2026-11-23-runner-records-r14.md`; slices.yaml entry `X-hit-resolution-nits`.

## Goal
1. **`example_hit()` carries a resolution** (`crates/api/src/dto/user_items.rs`): the OpenAPI example shows a real value (for example `1080p`), not null, so the docs show what the field holds.
2. **The schema gives `resolution` an enum plus null.** `SearchHit.resolution` (`crates/api/src/dto/search.rs:522`) stays `Option<String>` on the wire and `required`, but its schema lists the canonical values, `Resolution::as_str`'s eight (`480p`, `576p`, `720p`, `1080i`, `1080p`, `1440p`, `2160p`, `4320p`), plus null. The generated `schema.d.ts` then reads a string-literal union or null.
   - **One source.** The enum list must come from, or be tested against, `release-parser`'s `Resolution` (a test that fails when a variant is added there and not here). Don't hand-copy the list without such a test.
   - The table's em dash cell for null stays. Web code must still type-check with the narrower type; fix only what the regenerated type requires.

## Step 0 (send me, then wait for "go")
1. How you'll derive or test the enum against `Resolution`, and what the generated `openapi.json` and `schema.d.ts` diff looks like.
2. Owns, to refine: `crates/api/src/dto/user_items.rs`, `crates/api/src/dto/search.rs`, `crates/api/tests/openapi.rs`, `web/src/lib/api/openapi.json`, `web/src/lib/api/schema.d.ts`, `docs/decisions/2026-11-23-X-hit-resolution-nits.md`; plus any web file the narrower type forces (name it).
3. An estimate of the changed lines.

## Rules
- `openapi.json` and `schema.d.ts` are **regenerated, never hand-merged**. #1083 (X-setup-token-boot) also changes both and may merge first: before pushing, if it has merged, bring `origin/main` in and regenerate.
- No web size change is expected; if any size-limit entry moves, tell me before pushing (no raises without the runner).
- Gates per CLAUDE.md for Rust and `web/` (`npm run check`, vitest, build, size:ci).

## Branch and PR
- Branch `slice/X-hit-resolution-nits`. PR title "X-hit-resolution-nits: SearchHit.resolution example and enum schema".
- Push once per round, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
