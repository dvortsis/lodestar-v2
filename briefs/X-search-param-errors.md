# X-search-param-errors (runner brief 2026-11-20)

These are pre-existing optional findings from #992's review (X-gs3-nits). They tighten the errors `/api/v1/search` returns for bad parameters, so the UI and API users get a clear, bounded message.

## Scope
1. **O1. Cap the echoed value.** When `collapse` or `sort` is unknown, the 400 error echoes the value back. Cap the echo at about 64 characters (truncate on a char boundary and mark it with "…"), and make sure the echo can't carry control characters. Do the same anywhere else this handler echoes a query parameter. Test it with a 10 kB value and with a value containing control characters.
2. **O2. A repeated parameter.** `?sort=a&sort=b`, or the same for `collapse`, `cursor` and the other single-valued parameters, gets a field-specific 400 naming the parameter, not a generic parse error. Test each single-valued parameter.
3. **O3. A collapsed cursor without its collapse value.** `crates/api/src/search.rs` around :468 handles this. When a cursor made under a collapse mode is sent without `collapse` (or with a different mode), the 400 says so in plain words: the cursor belongs to a collapsed search, so send the same collapse value or start again. Test both cases.
4. Keep every error shape in the existing problem format, and update the golden files under `crates/api/tests/golden/` for any changed body. If the OpenAPI description changes, keep `web/src/lib/api/openapi.json` and `schema.d.ts` in sync with the repo's generator, never by hand. Say so in the PR.

## Owns
- crates/api/src/search.rs
- crates/api/src/dto/search.rs
- crates/api/tests/search*.rs
- crates/api/tests/golden/search_*.txt
- web/src/lib/api/openapi.json and web/src/lib/api/schema.d.ts, only if the description changes
- docs/decisions/2026-11-20-X-search-param-errors.md
- this slice's own yaml entries

No running slice owns these.

## Checks
Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test -p` on the api crate, plus the workspace tests before you push. If you touch openapi.json, also run `npm run check` in web/.

## Done
CI green. Send the runner "ready <pr> <full sha>". A fresh Opus review follows.
