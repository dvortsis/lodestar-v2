# X-api-openapi-400s (runner brief, 2026-11-22Z)

Kind: helper; run it on Opus here. Dep X-api-openapi-repeated is merged (#1021).

## What
The `Query<T>` routes that declare no 400 in OpenAPI (digest, collections, discover, user-items, the filters queue and tombstones, settings export, suggest, insights and others) each get a 400 description. X-api-openapi-repeated's decisions file lists them; re-derive the list from the code.
- **Each route's list names only the codes its code paths can return:** check the extractor and handler for each.
- **Edit only the `responses(...)` attribute lines,** plus shared response helpers if X-api-openapi-repeated added one.
- **Regenerate `web/src/lib/api/openapi.json` and `schema.d.ts` with the repo's tooling;** never hand-edit them. The web types must still compile: run `npm run check`.

## Step 0
Send the route list, with the 400 cause per route and any route you found that can't return 400. Also send the regeneration command you'll use, the files and an estimate.

## Owns
- `crates/api/src/**` (`responses(...)` lines only; narrow this at step 0 to the exact files)
- `web/src/lib/api/openapi.json`
- `web/src/lib/api/schema.d.ts`
- `docs/decisions/*-X-api-openapi-400s.md`
- this slice's `slices.yaml` entry

## Tests and process
- **Tests:** the existing OpenAPI drift test must pass. Add a test that every `Query<T>` route declares a 400, if a cheap way to enumerate them exists.
- **Branch and PR:** branch `slice/X-api-openapi-400s`, PR title "X-api-openapi-400s: <title>". Push once per round, then send "ready <pr> <sha>". Follow the steward skill; never merge.
