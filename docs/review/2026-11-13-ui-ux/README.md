# UI/UX review, 2026-11-13

A review of the web UI on `main` at `55e155a9`, made by a review session at the owner's request and passed to the build runner for triage. It records findings and a plan; it changes no product code.

| File | What it holds |
|---|---|
| [`01-review.md`](01-review.md) | Part 1: every screen against architecture §10. B1–B3, I1–I20, P1–P14 |
| [`02-skills-pass-and-future-modules.md`](02-skills-pass-and-future-modules.md) | Part 2: the pass with all 15 design skills, blind spots, opportunities, the UI plan for unbuilt modules, and the owner's decisions |
| [`03-not-done.md`](03-not-done.md) | Checks neither pass made, and how to run each |
| [`scripts/`](scripts) | The capture and measurement scripts, so any check can be repeated |

Screenshots and per-screen JSON are not committed (about 500 files); the scripts regenerate them.

## Running the scripts

They drive the real server on its fixture data, not the OpenAPI mock.

```sh
(cd web && npm ci && npm run build)
cargo build --release -p lodestar
LODESTAR_HTTP_BIND=127.0.0.1:8291 LODESTAR_FIXTURE_PASSWORD='correct horse battery' \
  ./target/release/lodestar serve --fixture /tmp/lodestar-fixture &
cd docs/review/2026-11-13-ui-ux/scripts
node sweep.mjs      # every route × width × mode: screenshot, axe, overflow, targets
node measure.mjs    # spacing, radii, control heights, type, icons, motion
```

| Variable | Default | Meaning |
|---|---|---|
| `BASE` | `http://127.0.0.1:8291` | The server to test |
| `PASSWORD` | `correct horse battery` | The fixture password |
| `FIXDIR` | `/tmp/lodestar-fixture` | Where `fixture.json` is (torrent hashes are read from it) |
| `OUT` | `scripts/out` | Where screenshots and JSON go; don't commit it |
| `CELLS` | six widths, light and dark | `sweep.mjs` only, e.g. `CELLS='1440:light,2560:light'` |
| `ONLY` | all routes | `sweep.mjs` only: a regular expression over route names |
| `PW_CHROMIUM_PATH` | `/opt/pw-browsers/chromium` | The browser to launch |

| Script | What it does |
|---|---|
| `sweep.mjs` | Visits 44 routes per width and mode; writes `<route>@<width>-<mode>.png` and `.json` |
| `measure.mjs` | Reads computed styles on 17 screens at 1366 and 390 px; writes `_measure.json` |
| `flow-desktop-search.mjs` | Query typing, facets, drawer, palette, menus, table, cards at 1366 px |
| `flow-phone.mjs` | Menu, filter sheet, detail, select mode, dialogs at 390 px with touch |
| `flow-drawer-and-focus.mjs` | Drawer scrolling, deep-list drawer position, tab order |
| `flow-settings-auth-locale.mjs` | Drawer at 1440 and 1920, settings save bar and leave guard, sign-in, `zh-CN`, dark |
| `flow-first-run.mjs` | Setup and the wizard on an unclaimed server (`TOKEN` from its log; see the note below) |
| `flow-touch-targets.mjs` | Lists controls under 44 px on touch layouts |
| `tile.py` | Joins screenshots side by side for reading |

Two torrent hashes in `measure.mjs` and `flow-touch-targets.mjs` come from the fixture as built on 2026-11-12. If the fixture format changes, take new ones from `fixture.json`.

`flow-first-run.mjs` needs a second server started without `LODESTAR_FIXTURE_PASSWORD`, and its address is fixed at `http://127.0.0.1:8292` in the script.
