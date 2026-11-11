# X-updates-raw-codes: the Updates screen never shows a raw code (Sonnet)

**Why:** #657's review. The Settings › Updates screen still shows `install_kind_unknown` and `data_folder_unchecked` (`capability.reason`), and the `install_failed` notice, as raw codes. CLAUDE.md's user-docs rules and the §10 UI rules say users never see a raw code.

## Build
1. **Map each code to plain, matter-of-fact text** in `web/src/lib/features/settings/updates/messages/{en,zh}.json`, following the screen's existing message keys and its `UpdatesScreen.svelte` map. Each message says what happened and what the user can do. For example:
   - `install_kind_unknown`: Lodestar couldn't tell how it was installed, so it can't update itself; update it the way you installed it.
   - `data_folder_unchecked`: Lodestar couldn't check its data folder yet; try again after it finishes starting.
   - the `install_failed` notice: the update didn't install; Lodestar is still on the old version; see the log.

   Keep the existing wording style; you choose the exact words.
2. **Any code the screen doesn't know falls back to a generic plain message,** never the code itself. Check that's true and test it.
3. **Two nits from the #657 review,** in `crates/api/src/dto/updates.rs` doc comments only:
   - `install_kind_unknown` says Lodestar "couldn't tell";
   - the `WindowsHelper` doc says it's "kept for clients and other platforms".

   Regenerate `web/src/lib/api/openapi.json` and `schema.d.ts` with the repo's commands (`cargo run -p lodestar --bin openapi`, `npm run api:types`); never edit them by hand.
4. **Tests in `updates.test.ts`:** each of the three codes shows its message and never the code; an unknown code shows the fallback. No fixed sleeps.

## Owns
- `web/src/lib/features/settings/updates/**`
- `crates/api/src/dto/updates.rs`
- `web/src/lib/api/openapi.json`
- `web/src/lib/api/schema.d.ts`

## Checks
`npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, `cargo fmt --check`, `cargo clippy -p lodestar-api --all-targets -- -D warnings`.

Branch `slice/X-updates-raw-codes`; PR title "X-updates-raw-codes: <title>". Follow the steward skill.
