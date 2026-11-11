# X-natural-wording (Sonnet): the natural-search refusal and the /parse docs count tokens, not words

Assigned by the runner on 2026-11-11. Its dependency, X-natural-strict-parity (#609), is merged, and so is X-updates-api-text (#657). That merge frees `openapi.json` and `schema.d.ts`.

## Read first
- The `X-natural-wording` entry in `runner/slices.yaml`. Its notes are binding.
- `crates/api/src/search.rs`, around lines 205–240: the `/parse` doc comment at :210 and the refusal text at :233.
- `crates/search/src/natural.rs`: `NaturalError` (around line 41) and the token count (around 601–665). **Read it only; don't edit it.**
- `docs/ui-glossary.md` and the plain-language rules in `CLAUDE.md` (User documentation).

## The problem
B7's cap counts tokens, and brackets and a lone `-` are tokens too. So a phrase with as few as 8 visible words can be refused. The doc comment says "32 words", and the refusal interpolates `NaturalError`'s Display ("more than 32 words and operators"). Both mislead users.

## Owns
- `crates/api/src/search.rs`
- `crates/api/tests/golden/parse_natural_too_long.txt` (re-bless it with the repo's own bless mechanism)
- `web/src/lib/api/openapi.json` and `web/src/lib/api/schema.d.ts`, regenerated only by the generator (`cargo run -p lodestar --bin openapi`, then `npm run api:types`). Never hand-edit or hand-merge them.
- `docs/decisions/2026-11-11-X-natural-wording.md`
- Not `crates/search/src/natural.rs`. The cap stays unchanged.

## Build
- **The refusal text:** for `TooManyWords`, the API builds its own text instead of interpolating `{e}`. Say plainly that a natural search can hold at most 32 words and symbols, and that brackets and a lone minus sign count as one each. Suggest shortening the search, or switching to the query language. This is the runner's UX call under §2.26. Keep the 512-byte case accurate too.
- **The `/parse` doc comment:** describe the cap as tokens, in the same terms.
- **Tests:** a golden for the new text, and a test with 8 visible words and enough brackets to pass 32 tokens, refused with the new wording. Mutation-check it: the test must fail if the text goes back to interpolating `{e}`.

## Checks
- `cargo fmt --check`; clippy with `-D warnings`; `cargo test -p lodestar-api`; the openapi drift check.
- In `web/`: `npm run check` and `npx vitest run`. If the web UI shows this text anywhere, check that its test still passes.

## Process
- Follow `.claude/skills/steward/SKILL.md` from first push to merge-ready.
- Branch `slice/X-natural-wording` from the latest `origin/main`. PR title: `X-natural-wording: <title>`.
- No fixed sleeps. Push once per round of fixes. Keep it small.
- Anything beyond this brief goes to the runner first.
- When ready, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg): `X-natural-wording: PR #<n> ready for review, head <sha>`, with CI green. If blocked: `X-natural-wording: blocked on <what>`. The runner reviews and merges.
