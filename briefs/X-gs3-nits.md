# X-gs3-nits (Opus; runner brief 2026-11-20)

These are the optional findings from #973 (GS-3, collapse-by-group, merged as 5b4c6baa). They're small and done in one PR. Keep behaviour the same, except where item 2 makes a malformed request fail clearly.

1. **Test.** In `crates/search/src/collapse/tests.rs` (~line 393, the cursor-page check inside the bounded-scroll test), `members(…, first.next.as_ref(), …)` runs without checking that there is a next page. Assert `first.next.is_some()` first. Otherwise the test can pass vacuously on a single-page result.
2. **Parse `collapse` through `CollapseMode`.** `crates/api/src/search.rs` (~283, ~330) handles the `collapse` query value next to the typed `CollapseMode` enum (`crates/api/src/dto/search.rs` ~239). Parse it once through `CollapseMode`, with serde or `FromStr`, so the accepted values, the 400 error for an unknown value and the OpenAPI schema all come from one place.
   - A bad value gets the same error shape and wording style as the other invalid query parameters. Check the existing ones and match them. Wording is plain and UX-first.
   - Regenerate the OpenAPI document if the schema changes, with the repo's tool, never by hand. CI's "openapi.json drift" job checks it.
   - Add an API test: `collapse=group`, `collapse=none` and no value all work, and `collapse=bogus` gives the clear 400.
3. **Decisions wording.** `docs/decisions/2026-11-16-GS-3.md` states the bound on the work an exact count does. Make it say `min(Σ group sizes, k × matches)`, if that's what the code does. Check `count.rs` first and write what's true. Put the correction in a new decisions file, `docs/decisions/2026-11-20-X-gs3-nits.md`, and leave the old file unedited unless that's the repo's convention for typo fixes.

## Owns
- `crates/search/src/collapse/tests.rs`
- `crates/api/src/search.rs`
- `crates/api/src/dto/search.rs`
- `crates/api/tests/search_collapse.rs`
- the generated OpenAPI file, if the schema changes
- `docs/decisions/2026-11-20-X-gs3-nits.md`
- this slice's own `slices.yaml` and `dashboard.yaml` entries

## Not in scope
Any change to ranking, counting or paging behaviour.

## Done
- CI green, including openapi drift and the web checks if the UI's generated client changes.
- Self-review per the steward skill.
- Send the runner "ready <pr> <full sha>".
