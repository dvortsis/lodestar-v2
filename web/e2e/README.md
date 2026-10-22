# Web end-to-end harness

Playwright + axe + an OpenAPI mock, plus Lighthouse CI. Architecture §10.17; budgets §10.1.
Chromium only. Locally it is the preinstalled one in `/opt/pw-browsers`: **never run `playwright install`** (CI does, in `e2e.yml`).

## Projects

Filter with `--project=<name>` (globs work). Every project starts the web server from `playwright.config.ts` (`npm run build && npm run preview`, cold ~73 s on CI, so its timeout is 180 s; an already-running server on :4173 is reused locally).

| Project                   | Runs                                                                                                                                                              | Script                   |
| ------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------ |
| `core`                    | `routes`, `tokens`, `harness` (mock, rotation, keyboard guard self-tests), `theme-gate`                                                                           | `npm run e2e:core`       |
| `bp<w>-<theme>-<density>` | `smoke.test.ts` at widths 320, 390, 768, 1024, 1366, 1920, 2560 × Riverstone light/dark × comfortable/compact (28 projects), e.g. `bp768-riverstone-dark-compact` | `npm run e2e:matrix`     |
| `sample-<theme-id>`       | the same smoke at 1366 for a rotating 6 non-Riverstone themes                                                                                                     | `npm run e2e:sample`     |
| `reduced-motion`          | smoke + the motion-token test under `prefers-reduced-motion: reduce`                                                                                              | `npm run e2e:motion`     |
| `keyboard-only`           | `*.keyboard.test.ts`                                                                                                                                              | `npm run e2e:keyboard`   |
| `all-themes`              | axe on every route in all 48 themes. Opt-in (`--project=all-themes` or `E2E_ALL_THEMES=1`); nightly in CI                                                         | `npm run e2e:all-themes` |

`npx playwright test` runs everything except `all-themes`.

**Rotating sample.** `sampleThemes(seed)` (`support/themes.ts`) takes the 46 non-Riverstone themes, permutes them with a coprime stride and returns a 6-wide window that moves by 6 each run. The seed is `GITHUB_RUN_NUMBER`, or the day number locally. Eight consecutive runs cover every theme.

**Theme and density are applied the way the app applies them.** Theme: the `lodestar.theme` localStorage key (`{ mode, light|dark: { id, vars } }`) that the pre-paint script in `app.html` reads, written by an init script before the page loads. Riverstone stores only the mode. Density: the `density` URL parameter (§10.3), added by the `go(path)` fixture. Do not inject CSS or set attributes by hand.

## Writing a test

Import `test`/`expect` from `./support/test` (not `@playwright/test`) to get the fixtures:

- `themeId`, `colorMode`, `density`, `motion`: per-project options. Override per file or `describe` with `test.use({ themeId: 'nord-dark', colorMode: 'dark' })`.
- `go(path)`: `page.goto` with the project's density in the URL.
- `mock`: the OpenAPI mock (below). It is installed for **every** test, so an unmocked `/api/v1` call fails the test.
- `kbd`: the keyboard-only guard (below).

Helpers: `expectNoA11yViolations(page, opts)` (`a11y.ts`, WCAG 2.2 AA, prints rule, targets and help URL) and `expectPageSnapshot(page, name)` (`snapshot.ts`, full page, fonts ready, animations frozen).

New routes: add an entry to `support/routes.ts`; the smoke matrix then covers it everywhere. `coverage.test.ts` annotates any route under `src/routes` that is missing.

## The OpenAPI mock

`mock/openapi-mock.ts` intercepts `/api/v1/**`, matches the request to an operation in `src/lib/api/openapi.json` (method + path template, static segments before `{params}`) and answers with that operation's documented example (`example`/`examples` on the JSON response, or on its schema). A test overrides any operation with data typed from `schema.d.ts`:

```ts
test('search shows results', async ({ page, mock, go }) => {
	mock.on('searchGet', { hits: [], total: 0 /* … typed as SearchResponse */ });
	mock.on('getTorrent', (request) => ({ status: 200, body: fixtureFor(request.url()) }));
	mock.error('searchGet', 400, 'unknown_sort', 'Unknown sort', { field: 'sort' }); // error envelope
	await go('/search?q=ubuntu');
	expect(mock.calls('searchGet')).toHaveLength(1);
});
```

A request that matches no operation, or an operation with neither example nor override, gets a `501` error envelope and **fails the test at teardown**, naming the operation. When the fixture server (`lodestar serve --fixture`, B16) exists, point `baseURL` at it and drop the mock.

## Snapshots

Baselines are **Linux only** (`*-linux.png`, next to each spec in `<spec>-snapshots/`); never commit macOS or Windows ones. Compare with a pixel ratio of 0.001.

To update after an intended visual change:

1. Locally, on Linux with the preinstalled Chromium: `npm run e2e:update` (or `npx playwright test --project=<name> --update-snapshots`), review the PNG diffs, commit.
2. If your local output differs from CI's (fonts, GPU), run the **E2E** workflow by hand (Actions → E2E → Run workflow → `update_snapshots`). It regenerates every baseline in the job and uploads them as the `snapshot-baselines` artifact; unzip into `web/e2e/` and commit.

When a snapshot fails in CI, the `playwright-report` artifact holds the HTML report and the expected/actual/diff images (`test-results/`).

## Keyboard-only flows

Name the file `*.keyboard.test.ts`; it runs in the `keyboard-only` project. Use the `kbd` fixture:

```ts
import { expect, test } from './support/test';

test('search → open → send', async ({ page, go, kbd }) => {
	await go('/search?q=ubuntu');
	await kbd.press('/'); // focus search
	await kbd.tabTo(page.getByRole('button', { name: 'Send' }));
	await kbd.press('Enter');
	// …
});
```

`page.mouse.*` throws, and a trusted pointer event (`pointerdown`, or a click with `detail > 0`) is recorded; the fixture fails the test at teardown if any occurred. Do not call `locator.click()`, `hover()` or `fill()` in these tests: `click` and `hover` use the pointer; drive inputs with `kbd.press` / `page.keyboard.type`.

## Theme gate and reduced motion

`theme-gate.test.ts` loads with a stored dark theme while the OS asks for light and asserts that the root's `data-theme` and background are already the dark token at `DOMContentLoaded` and in the first animation frame (expected values come from `critical.css` / `static/themes.json`, not from the page). `reduced-motion.test.ts` asserts every duration token is 0 except `--duration-reduced-fade`.

## Size and Lighthouse

- `npx size-limit` also fails if a brand raster (`/brand/**/*.{avif,webp,png}`, favicons excepted) is referenced from the first-load html, search JS or CSS (`.size-limit.mjs`).
- `npm run lhci` runs Lighthouse CI against `npm run preview` (build first): `lighthouserc.json` (mobile) then `lighthouserc.desktop.json`. It asserts LCP (mobile < 2.5 s, desktop < 1.0 s), TBT ≤ 100 ms (the lab proxy for INP < 100 ms), CLS ≤ 0.02 and total byte weight ≤ 100 KB. Reports are written to `.lighthouseci/` and uploaded as a CI artifact (no upload server). Locally set `CHROME_PATH=/opt/pw-browsers/chromium-1194/chrome-linux/chrome`.
