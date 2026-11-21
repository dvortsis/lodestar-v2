# X-param-echo-cap

Follow-up to X-search-param-errors (#1000), from its review's optional findings. Runner's call; small, Opus.

## Owns
- crates/api/src/search.rs (only the echo/repeat check added in #1000)
- crates/api/src/facets.rs
- crates/api/src/discover.rs
- crates/api/src/export.rs
- crates/api/tests/ files that test these four routes
- runner/slices.yaml: this slice's own entry only
- docs/decisions/<date>-X-param-echo-cap.md

## Do
1. Make the search echo helper `pub(crate)` and reuse it wherever an error echoes a caller-supplied value: the facet name in `facets.rs` (message and `field`), `type` in `discover.rs`, and the cap in `export.rs`, which currently drops the "…". The same 64-character cap, Debug escaping and "…" apply everywhere.
2. The /facets repeated-parameter case: give a specific `bad_parameter` message naming the repeated parameter, matching /search. Use the same parse the extractor uses (`form_urlencoded::parse`).
3. The /search repeat check skips HEAD, because axum `get()` also serves HEAD. Check every method except POST.
4. Tests:
   - 64 and 65 characters, with "…" only on the 65-character value;
   - a multi-byte character at positions 64 and 65;
   - repeated-parameter parsing for `sort&sort` (no `=`), `sort=&sort=x` and `so+rt`;
   - HEAD with a repeated parameter;
   - the facets and discover echoes, including a control or bidi character coming back escaped.
   The mutation "…" only past 65 characters must fail a test.
5. No API contract changes: error codes stay the same and OpenAPI is unchanged.

## Gates
cargo fmt --check, clippy -D warnings, cargo test --workspace, and the status-connector npm test, because slices.yaml changes.
