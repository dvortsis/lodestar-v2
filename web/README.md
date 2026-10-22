# Lodestar web

SvelteKit 2 + Svelte 5 (runes) SPA, embedded in the Rust binary. Architecture: `docs/architecture.md` §10.

## Run

```sh
npm ci
npm run dev        # dev server
npm run build      # static SPA into build/ (adapter-static, fallback index.html)
npm run preview
```

## Test and check

| Command                           | What it does                                                                                                                                                                                                                                                                                                                                          |
| --------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `npm run check`                   | svelte-check + ESLint + `prettier --check` + `scripts/check-bans.mjs`                                                                                                                                                                                                                                                                                 |
| `npx vitest run`                  | unit and component tests (`src/**/*.test.ts`, jsdom)                                                                                                                                                                                                                                                                                                  |
| `npm run e2e`                     | Playwright + axe (`e2e/`). Never run `playwright install`. `@playwright/test` is pinned to 1.56.x because it matches the preinstalled Chromium 1194 in `/opt/pw-browsers` (`PLAYWRIGHT_BROWSERS_PATH`, `PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1`). Bump both together                                                                                      |
| `npm run size` / `npx size-limit` | gzipped budgets from `.size-limit.mjs` (needs a build). Size only (`running: false`). The search route's files come from the SvelteKit build manifest (`scripts/size-entries.mjs`), so other routes never count against it. Total first load includes fonts once they are wired; the drawer (≤ 15 KB) and Insights (≤ 45 KB) budgets are placeholders |
| `npm run api:types`               | `src/lib/api/openapi.json` → `schema.d.ts`; a no-op if the JSON is absent                                                                                                                                                                                                                                                                             |

Before pushing: `npm run check && npx vitest run && npm run build && npx size-limit`.

## Conventions

- **Layout** (`src/lib`, each folder has a README): `ui` (primitives), `features` (compositions), `charts`, `api`, `state`, `format`, `util`, `theme`, `shell`, `keys`. Dependencies point one way: `ui` → `features`, never the reverse.
- **Dev-only routes** (UI showcases for Playwright and axe) live in `src/dev-routes/`, **outside** `src/routes`, so production builds never contain them (they would otherwise grow the shared chunk the search route loads). `scripts/dev-routes.mjs`, called from `svelte.config.js`, links the folder in as `src/routes/dev` (gitignored) for `vite dev` and for any run with `VITE_DEV_ROUTES=1` (Playwright's web server sets it), and removes it only on `vite build` without them (so vitest, check and a running `vite dev` keep it); it refuses a real `src/routes/dev` folder. Put a showcase at `src/dev-routes/ui/<group>/+page.svelte` (+ a `+page.ts` with `export const load = guardDevRoute` from `src/dev-routes/guard.ts`, which 404s unless dev or `VITE_DEV_ROUTES`); it is served at `/dev/ui/<group>`. Never import from `dev-routes` in app code.
- **SPA only:** `ssr = false`, `prerender = false` in the root `+layout.ts`.
- **Lint bans:** no hex colour literals and no Tailwind arbitrary values (`bg-[#fff]`, `w-[13px]`) in `src/**/*.svelte` and `src/**/*.ts`. Enforced by an ESLint rule and by `scripts/check-bans.mjs` (logic shared in `scripts/bans.mjs`, tested in `scripts/bans.test.ts`). Comments and `href="#x"`-style fragment values are ignored. Exempt: `src/lib/theme/**` and generated files.
- **Pinned dependencies:** exact versions, no `^`. Adding one needs main-session approval.
- **Budgets** (§10.1): search route JS ≤ 45 KB, CSS ≤ 20 KB, total first load ≤ 100 KB, gzipped.
- **Messages:** one file per feature, merged by a script; see [Messages](#messages).

## Messages

Compile-time i18n with Paraglide 2 (`en` base, `zh`), tree-shaken: a component bundles only the messages it calls. Wording follows `docs/ui-glossary.md` and architecture §10.14.

**Add a message to your feature:**

1. Put it in `src/lib/features/<feature>/messages/en.json` (and `zh.json` when translated; zh is filled later, and a missing zh key only warns and falls back to en). Keys are snake_case and start with the feature name: `search_results_count`, never `count`. For `kebab-case` feature folders use underscores (`saved-searches` → `saved_searches_`).
2. Use `{name}` placeholders for values (`"{count} results"`). No ICU plural or choice syntax; keep one message per form. zh must use the same placeholders as en.
3. Import and call it: `import { m } from '$lib/paraglide/messages.js'`, then `{m.search_results_count({ count })}`. Numbers and dates go through `src/lib/format` first; pass the formatted string.
4. Run `npm run messages` (also part of `npm run check` and `prebuild`). It writes the merged `messages/{en,zh}.json` and compiles `src/lib/paraglide/`; both are generated and gitignored. Dev, build and vitest also merge on start.

**Shared strings** (`src/lib/i18n/messages/`) are the glossary terms, keyed exactly as the key column of `docs/ui-glossary.md` (`torrent`, `seeders`, `video`, …), plus `common_*` for other cross-feature strings. Glossary zh labels stay out of `zh.json` until the glossary marks them settled.

`scripts/messages.ts` fails on a duplicate key, a key outside the feature's namespace, a zh key with no en key, an invalid placeholder, or zh placeholders that differ from en. Missing zh keys are only listed as a warning.

**Locale:** `src/lib/i18n/locale.svelte.ts` picks the settings value, then `navigator.languages`, then en. `initLocale(setting)` applies it to Paraglide and `<html lang>`; `currentLocale()` is what `Intl` formatters (`src/lib/format`) use. After a language change, re-render with `{#key currentLocale()}` since messages are plain function calls.

Paraglide's JSON plugin is the pinned devDependency `@inlang/plugin-message-format`, loaded from `node_modules` (not jsDelivr), so the compile never needs the network.
