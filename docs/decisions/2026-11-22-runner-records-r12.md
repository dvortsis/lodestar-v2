# runner-records-r12 decisions (#1068)

The runner's records PR after r11 (#1059). It records the runner's calls on 2026-11-22Z for #1060 to #1067, and the slices they leave. Each call is flagged to the owner, who can veto it (§2.19). The slices' own decisions files hold the detail.

## Calls in merged slices
- **X-block-on-only-mode (#1060):** as r11 recorded.
  - start-block's answer shows `mode_on` only as one more `ok`, because `run.py summary()` drops ok names. The runbook names `MODE=on` in `block.env` as the sure check, and the output format is unchanged.
  - **Agreed with the review:** the 12 h before a fresh-database screen's restart drill runs on the test VM, with the screen's own switches.
  - **A visible mode in vm-status is optional.** It's queued as X-vm-status-mode (below).
- **X-pool-first-request-docs (#1061), runner calls under §2.26:**
  - **At the test-only 1 MB budget, at 10× seeds, the model's 30 s wait fails.** Seven first reservations in five seeds waited 30–44 s; all were granted. A past-cap growth no longer fits beside one other first request: 88,448 + 88,448 + 583,641 = 760,537 B > 750,000 B.
  - Production budgets are 64 MiB for the pool and 4 MiB for a worker, and neither is configurable. The default run passes at both budgets, and the worker budget passes at 10×.
  - **So the doc claim is scoped,** and the pool rule question goes to X-pool-past-cap-room (below).
  - The review's nits go to that slice too, rather than costing a CI round here.
- **X-openapi-400s-tidy (#1063):** each `QUERY_WITHOUT_400` entry must name one operation that still takes a query, and be listed once. A stale or duplicate entry fails. `GET /feeds/{id}`'s only 400 is the undocumented `bad_path`.
- **The url 2.5.9 bump (#1064), a runner fix:**
  - **The advisory:** RUSTSEC-2026-0592, a stack overflow in `Url::origin()` on nested `blob:` URLs. It turned the RustSec job red on main's lock and on every PR that runs that job. Both lockfiles move to url 2.5.9 and form_urlencoded 1.2.3, and `web/licenses/rust.json` is regenerated.
  - **The canonical tracker form changes for `^` in a path:** 2.5.9 percent-encodes it (2.5.8 already encoded `` ` ``). The kept form is still a fixed point, and the passkey filter is unchanged. A longer form fails the length cap closed.
  - A tracker stored under 2.5.8 with a `^` in its path may appear in a second spelling. That's rare, and nothing compares stored trackers against the guard.
  - SRC-13c's cache templates refuse anything the parser would encode, so a template with `^` is now refused. 2.5.9 also refuses `udp://host:port\…`, which 2.5.8 accepted. Both are tightenings only.
  - **Not new:** `cargo deny` in `fuzz/` flags RUSTSEC-2026-0385 (ctutils 0.4.2). It's the same on main, and CI doesn't check the fuzz workspace. That goes to X-fuzz-advisories (below).
- **X-search-retry-flush (#1065), runner calls under §2.26:**
  - **The Search route JS cap stays at 45 kB, with no raise.** The route measures 44,948 B in `size:ci` and 44,955 B in a plain build.
  - **The detail drawer's failure panel has no Close button.** Close cost 57 B and took the route over the cap. Back clears the panel, as it closes the drawer, and opening another row replaces it.
  - **The panel sits in `#results`, sticky at the top, before the toolbar.** A fixed panel at the bottom would share the Toaster's slot, and one at the top would cover the top bar. The global CSS has 8 B of headroom, so there are no new utilities.
  - On failure only, it pushes the list down about 90 px, and Retry comes before the row in Tab order. Focus isn't moved, and the panel is role=alert.
  - **A Retry whose flush outlives the page commits nothing.** `cancelPending()` bumps a generation the pending write checks. An e2e test pins the leave guard, and a unit test pins the generation check.
  - **The `hostFailed` reset has no test.** Chromium 153 keeps a failed import failed, and a component test would need a loader module the route can't afford. Its decisions file records this.
  - **The chunk-load harness's `blockChunk`** tries a `route.fetch` that resets up to three times, then aborts the route. A test that needs the block still asserts that the block was hit.
- **X-fixture-server-kill-on-drop (#1066):**
  - The helper's `Drop` kills and reaps a child that hasn't been reaped yet, and `stop()` behaves as before. A new 0.08 s test, which needs no fixture build, catches an empty Drop and a kill without a wait.
  - Two nits wait for the next edit of that test: a 20–50 ms pause in its poll loop instead of `yield_now()`, and a note that the first panic message is expected.
- **X-msi-upgrade-env-value (#1067), runner call under §2.26: documented, not restored.**
  - A major upgrade by MSI removes and recreates the Lodestar service, so a variable set on the service is lost on every install or update, in-app updates included.
  - **A restore was declined.** It would put user variables, possibly secrets, into the verbose MSI log, and would write inside the service key the service-account hardening relies on.
  - **The Windows guide now recommends Settings → Updates,** which survives updates. It says a service variable is lost on update and how to set one again. Appendix B lists each `LODESTAR_UPDATES*` variable's exact accepted values, including the three `NOTIFY_*` ones, because a wrong value stops Lodestar from starting.
  - **Not yet checked on a real upgrade.** The next Windows lane sets the service's `Environment` before an upgrade and checks that it's gone afterwards.

## Calls in running slices
- **X-workflow-tests-filter (#1062), Option A:** one step in ci.yml's `changes` job runs `test_workflow_concurrency.py` on every PR, and the duplicate in `lint` is removed.
  - **Option C was declined.** It would run the test from the infra tests. That couples two test trees, puts a 0.1 s result behind a 6.5–7.5 minute job, and depends on the infra filter; and a change to the script alone doesn't match INFRA.
  - **This corrects the runner's step-0 reason.** Since #1058, a web.yml-only PR already runs the infra job, so C would have cost little extra CI time; its cost was the wait for the result.
  - `infra/fleet/tests/test_workflow.py` globs `.yml` and `.yaml`.
  - It brought `main` in after #1064 landed, so the RustSec job runs on the fixed lock.

## Cloud testing
- **The runner approved a real-network uTP screen of #1054** (X-test-vm, about $2.50 and 15 h, within the caps). It reports first-window timeouts per transport per hour, uTP against TCP. Those are the numbers owner queue item 34 lacks. Results come as numbers only.

## Process
- **Size measurements:** a plain `npm run build && npx size-limit` can differ from `npm run size:ci` by a few bytes. On #1065 a plain build failed Facet rail by 6 B and Insights stage pages by 17 B, while both passed in `size:ci`, and the Search route measured 7 B more. CI gates on `size:ci`, so that's the build a size call is judged on. CLAUDE.md already names `size:ci`; `web/README.md` and `docs/build-plan.md` still named plain `npx size-limit`. This PR fixes the build plan's two lines, and X-size-limit-one-build (below) fixes the rest.

## New slices
- **X-pool-past-cap-room (core):** should a past-cap growth leave room for a second first request when the budget is small? The pool's 1 MB test budget at 10× seeds shows first reservations waiting 30–44 s behind one. It's a pool rule, so the slice sets out the options at step 0, with the model's numbers, before building. It also takes #1061's review nits.
- **X-fuzz-advisories:** CI's RustSec job also checks `fuzz/Cargo.lock`, and the slice resolves RUSTSEC-2026-0385 (ctutils 0.4.2) there. It never runs beside another slice that owns `ci_changes.py` or ci.yml's `advisories` job.
- **X-size-limit-one-build:** a plain `npx size-limit` on a build that isn't the measurement build stops with an error naming `npm run size:ci`, instead of showing numbers a few bytes off. It doesn't rebuild inside the config: that would silently replace `build/` with the measurement build. `web/README.md` names `size:ci`.
- **X-vm-status-mode (optional):** vm-status shows a block's mode (`abba` or `on`) as its own line.
