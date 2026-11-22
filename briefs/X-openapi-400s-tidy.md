# X-openapi-400s-tidy (runner brief, 2026-11-22Z)

Kind: helper (tests only). Dep X-api-openapi-400s (#1053) is merged. This comes from #1053's review nits, recorded in `docs/decisions/2026-11-22-runner-records-r11.md`.

## The gaps
1. In `crates/api/tests/openapi.rs`, `every_query_route_declares_a_400` excuses the routes in `QUERY_WITHOUT_400` (today only `GET /feeds/{id}`). An entry that no longer matches any route, after a rename or a removal, is silently ignored. A stale entry should fail the test.
2. The `/feeds/{id}` reason says "a bad id a 404". The route can still answer a 400 `bad_path` for a malformed path segment, so the reason should say it declares no query 400, "apart from `bad_path`". Check what the route really answers from its code before you word it.
3. `docs/decisions/2026-11-22-X-api-openapi-400s.md` says "these three" without naming the third route. Don't edit #1053's file. State the correction in this slice's own decisions file.

## What to do
1. Assert that each `QUERY_WITHOUT_400` entry matches exactly one operation in the document. Report every stale entry by method and path.
2. Correct the `/feeds/{id}` reason, as in gap 2.
3. Mutation check, by hand and not committed: add a fake entry such as `("GET", "/nope", "x")` and see the test fail; rename the real one and see it fail.
4. Write the decisions file, with the "these three" correction.

## Rules
- Tests and docs only: no change to `crates/api/src/**` and no regeneration of the OpenAPI document.
- No fixed sleeps.

## Owns
- `crates/api/tests/openapi.rs`
- `docs/decisions/2026-11-22-X-openapi-400s-tidy.md`

## Branch and PR
- Branch `slice/X-openapi-400s-tidy`, cut from `origin/main`. PR title "X-openapi-400s-tidy: The OpenAPI 400 test checks each exception is used once".
- Gates: `cargo fmt --check`, `cargo clippy -p lodestar-api --all-targets -- -D warnings` and `cargo test -p lodestar-api --test openapi`.
- No step 0 is needed unless something here is wrong. Push once, then send "ready <pr> <full sha>". Follow `.claude/skills/steward/SKILL.md`; never merge.
