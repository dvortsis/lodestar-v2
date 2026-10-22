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

| Command | What it does |
|---|---|
| `npm run check` | svelte-check + ESLint + `scripts/check-bans.mjs` |
| `npx vitest run` | unit and component tests (`src/**/*.test.ts`, jsdom) |
| `npm run e2e` | Playwright + axe (`e2e/`). Never run `playwright install`: use the preinstalled Chromium (`PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1`; set `PLAYWRIGHT_BROWSERS_PATH` if needed) |
| `npm run size` / `npx size-limit` | gzipped budgets from `.size-limit.json` (needs a build) |
| `npm run api:types` | `src/lib/api/openapi.json` → `schema.d.ts`; a no-op if the JSON is absent |

Before pushing: `npm run check && npx vitest run && npm run build && npx size-limit`.

## Conventions

- **Layout** (`src/lib`, each folder has a README): `ui` (primitives), `features` (compositions), `charts`, `api`, `state`, `format`, `theme`, `shell`, `keys`. Dependencies point one way: `ui` → `features`, never the reverse.
- **SPA only:** `ssr = false`, `prerender = false` in the root `+layout.ts`.
- **Lint bans:** no hex colour literals and no Tailwind arbitrary values (`bg-[#fff]`, `w-[13px]`) in `src/**/*.svelte` and `src/**/*.ts`. Enforced by an ESLint rule and by `scripts/check-bans.mjs` (patterns shared in `scripts/bans.mjs`). Exempt: `src/lib/theme/**` and generated files.
- **Pinned dependencies:** exact versions, no `^`. Adding one needs main-session approval.
- **Budgets** (§10.1): search route JS ≤ 45 KB, CSS ≤ 20 KB, total first load ≤ 100 KB, gzipped.
- **Messages:** one file per feature (`features/*/messages/{en,zh}.json`); not wired yet.
