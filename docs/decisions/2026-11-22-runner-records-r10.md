# runner-records-r10 decisions

The runner's records PR after r9 (#1040). It records the runner's calls on 2026-11-22Z, #1039 and #1041 to #1050. Each call is flagged to the owner, who can veto it (§2.19). The slices' own decisions files hold the detail.

## Calls in merged slices
- **X-insights-source-names (#1039):**
  - **The fuzz targets skip only what serde_json can't round-trip.** A JSON key that starts `$serde_json::private::`, unescaped, makes serde_json read the value as its private number or raw-value form. `tpdb`, `stashdb` and `json` now skip only the final `Value` comparison for such input, and every other check still runs. Escaped keys don't count. A seed for each is under `fuzz/seeds/`.
  - **Size:** Insights stage pages went from 40.98 to 40.66 kB.
  - **Its release note** is in r9: the Insights CSV export gains a `name` column.
- **X-size-ci-script (#1041):**
  - **`npm run size:ci` is CI's exact measurement.** It runs `npm run build`, then the stable-name build (`LODESTAR_BUILD_VERSION=0000000`, `LODESTAR_STABLE_NAMES=1`), then size-limit. Each step goes through node with no shell, so it also works on Windows.
  - **Two stylesheets with the same name key fail the build.**
  - **CLAUDE.md and `runner/standards.md` now say to measure with `npm run size:ci`.** Plain `npx size-limit` on the shipped build can be off by −19 to +35 B, which is enough to pass locally and fail in CI.
- **X-relevance-finish-flake (#1042), nits fixed here:** macOS is "possibly" affected, not "by the same reasoning". XNU's `posix_spawn` closes close-on-exec descriptors before it returns, so only a fork-then-exec fallback could show this. The `CHILDREN` doc comment now says a child holds every close-on-exec descriptor until exec, not every descriptor.
- **X-ui-checks-2 (#1043):** the overlay drawer, popover, menu and toast cases, with the layout-shift check recorded as not done, as r9 said.
- **X-backoff-escalate-passthrough (#1044):** fixed `main`'s `infra` job.
  - The `docs/soak/tools/linux/README.md` paragraph about the recorded switches is reflowed here.
  - **One gap is left:** `infra/fleet/tests/test_workflow.py` reads every `.github/workflows/*.yml`, but the `INFRA` filter doesn't list `.github/workflows/`. A workflow-only change can break that test without running it. It's low risk. X-infra-filter-workflows (below) closes it.
- **X-ui-checks-3 (#1045):**
  - **The pseudo-locale check** runs nightly and on manual dispatch, in the `all-themes` job. The runner dispatched it on the slice branch before merging, and the pseudo step passed.
  - **Its allowlist entries are tied to an element, a time or a message, never to the whole page.** The scratch folder is per user and per port, and is checked with `realpath`.
  - **It found two pieces of hard-coded English,** listed as `KNOWN_GAPS`. X-pseudo-text-gaps (below) fixes them.
- **X-group-chip-label (#1046), calls under §2.26:**
  - **Group chips, Builder rows and facet-click rows show the rail's label when one is known.** The labels are kept in one session map of at most 500 entries, checked against S1-20's rule on entry.
  - **The Builder's input stays raw.** It's the query editor, so it shows exactly what Apply writes.
  - **A label matches its value in `fit_label`'s direction.** Every value a response lists moves to the newest end of the map, and a listing without a label deletes that value's entry.
  - **Size:** Facet rail (lazy) went from 18.85 to 18.86 kB (18,856 B).
- **X-sample-cooling-metric (#1047):**
  - **A new family, `dht_sample_cooling_reason{reason}`,** has seven reasons: `interval`, `unsupported`, `timeout`, `error`, `first_failure`, `restored` and `sealed`. **`dht_sample_failures_total`** is now exported too.
  - The old gauge is unchanged, because the soak tools read it.
  - The reason enum lives in `node/mod.rs`, as a label key.
  - Its `slices.yaml` notes are updated here to match.
- **X-ui-checks-4 (#1048):** still open. Its baseline run was in progress when this was written.
- **X-fleet-stacked-arms (#1049):**
  - **A rules file's `winners` uses the treatment keys** (`adm`, `utp`, `route`, `lc`, `hint`, `split`). `lc`, `hint` and `split` become treatment values.
  - **Only a stacked treatment takes winners** (runner's review, r5.1 A2). A stage-1 treatment runs against the shipped defaults, and `analyze_cr` refuses `--winners` with it.
  - **`collect()` passes `--winners`** for a stacked block, so its segments aren't all lost as `analysis_failed`.
  - The scrubber stays an allowlist.
- **X-recased-word-flake (#1050, runner PR):** two facet property tests failed now and then on `main`.
  - **`a_recased_word_is_found`** failed when the surrounding text already held the value; the test now assumes it doesn't.
  - **`samples_are_nested_and_spread_by_rank`** failed for totals of 0, 5 and 6, which can give fewer distinct ranks. Exact equality is now asserted only from 7 documents up, and a unit test pins the small cases.
  - Tests only. Their origin is #1033. Its `slices.yaml` entry is added here.

## Calls in running slices
- **X-fixture-updates-on (#1051):**
  - **It switches on Updates only.** Backups have no server feature yet.
  - **There's no fixture trust key.** `fixture` is a default feature, so a key behind it would ship in every build. The runner's first brief suggested one and withdrew it.
  - **Writes answer 409** with a fixture reason.
  - **`serve --child` refuses `--fixture-variant`.**
  - Its own PR corrects its `slices.yaml` title and notes.
- **X-api-openapi-400s:**
  - **Every route that reads its query declares its 400s,** with the codes each can return. A test reads the generated document and fails on any query route without a 400, unless the route is on a named exception list with a reason.
  - **`bad_path` isn't documented** on these routes, as on every other path route. A follow-up for all path routes is named in its decisions file.
  - **On `POST /settings/import`,** extractor 400s use `ErrorEnvelope`, and a wrong-shaped body stays 422.
- **X-search-chunk-retry** waits for X-ui-checks-4. Both own `web/e2e/ui-checks/**`, so its dependency is added.

## The cloud test VM (leads only)
These are leads from the X-test-vm lane, not results. They don't change a gate or a default.
- **CR-4's route-backoff screen is warranted:** IPv6 `no_route` stayed above 0.
- **A follow-up, X-leaktest-vpn-held-down:** the leak test's `vpn_interface_held_down` harness failure came back once after #988, with 0 packets leaked, and a second run passed.
- **A follow-up, X-block-on-only-mode:**
  - A screen sometimes needs every segment with the treatment on, but `start-block.sh` always opens segment 1 as control.
  - Two runbook facts go with it: the image has no `env` binary, so a running process's switches are read from `/proc/<pid>/environ`; and a fresh-database screen needs at least 12 h before its restart drill.
- **No baseline exists for capacity rejections** (millions an hour, under uTP), so they can't be judged yet. The next A/B screens report them for both arms.

## New slices
- **X-pseudo-text-gaps:** fixes the two pieces of hard-coded English that #1045 found.
  - Relative-time callers pass the catalogue's "just now".
  - Insights annotations look up the setting and event-kind values in the catalogue instead of inserting them raw.
  - It removes both `KNOWN_GAPS` entries.
- **X-leaktest-vpn-held-down:** why phase B still runs out of clean attempts after #988, and a fix in the harness only. It never loosens the leak verdict.
- **X-block-on-only-mode:** a block mode with the treatment on in every segment, plus the two runbook facts above.
- **X-infra-filter-workflows:** adds `.github/workflows/` to the `INFRA` filter, with a test. It never runs beside X-ci-advisories, which owns the same script.
