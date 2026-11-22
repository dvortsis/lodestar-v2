# runner-records-r11 decisions (#1059)

The runner's records PR after r10 (#1052). It records the runner's calls on 2026-11-22Z for #1048, #1051 and #1053 to #1057, and for the slices still running. Each call is flagged to the owner, who can veto it (§2.19). The slices' own decisions files hold the detail.

## Calls in merged slices
- **X-ui-checks-4 (#1048):** five fixture screenshots and their regeneration flow. The X-ui-checks umbrella is now complete.
  - **The coupling is kept (runner):** a PR marked for regeneration only commits its new baselines once the whole fixture suite is green, and `web/e2e/README.md` says so.
  - **The real test comes later:** the first nightly runs after 1da2253a test whether the fixture baselines hold across days, and whether the pseudo step holds in `all-themes`. The runner watches the nightly tracking issue.
- **X-fixture-updates-on (#1051):** as r10 recorded, it switches on Updates only.
  - `/updates` says why the sample release isn't eligible: `blocked_by` comes from the pure `policy::plan` plus a `fixture` capability. Writes answer 409 with a `fixture` reason.
  - Its decisions file records the in-memory pause as a departure from DUR-26.
  - **A follow-up, X-fixture-server-kill-on-drop (below):** the test helper's child server outlives a panicking test.
- **X-api-openapi-400s (#1053):** every route that reads its query declares its 400s. Its review's nits go to X-openapi-400s-tidy (below).
- **CR-UTP-FIRST-WINDOW (#1054), runner calls under §2.26:**
  - **The 4 s first-window deadline stays.** The fix is option (d): for a dict over 5 pieces, the first request asks for 5 pieces, one more than the 64 KiB window. It reserves 80 KiB plus the bound up front; the full reservation and the pace start once the window is in.
  - **The review fix:** a dict of exactly 5 pieces keeps the 4-piece first request, reserves 64 KiB, then grows. An earlier draft reserved it whole, a claim over the pool's `SMALL_CLAIM` (80 KiB). That would have admitted it as a large claim without the floor, grown-cap or per-network checks (review, blocking).
  - **The cost, in the pool's units:** about 87 KiB instead of about 71 KiB per waiting first request, so 18–20% fewer fit when the budget binds. Twenty silent peers pin about 1.7 MB of a worker's 4 MiB budget, against about 1.4 MB before. No pool rule assumes a size.
  - **The result:** misses at 4 s, at 5/10/15/20% loss, went from 0/8/25/75 to 0/2/10/37 of 400.
  - **(b), a bounded uTP extension, is the owner's** (owner_queue n 34). It would loosen the bound against slow peers. (c), a longer deadline for every transport, is advised against.
  - **Its pool docs, fixtures and model** go to X-pool-first-request-docs (below).
  - **Not a finding:** on a retry (MSE then plaintext, or uTP then TCP), a second attempt's first reservation can step up by 16 KiB, and the pool treats that as growth. It stays within 80 KiB plus the bound until a window is in.
- **X-checksum-wrap (#1055):** the install dialog's checksum splits 32 + 32 with a break opportunity, and the copy button still copies the whole value. The clipboard check stays, because it was stable across 20 runs, and the test now clears the clipboard before Ctrl+C.
- **X-search-chunk-retry (#1057), runner calls under §2.26:**
  - **The Search route JS cap stays at 45 kB,** for the owner's smallest-UI priority. The slice shrank its own code instead: 44,847 B, leaving 153 B of headroom.
  - **Lighthouse's first-load weight was met by shrinking, not by a higher limit.** The PR alone went 68 B over (102,468 B against 102,400 B). The runner declined a raise to 102,600 B, following S-locale-boot's precedent.
    - The three explanatory HTML comments in `web/src/app.html` shipped in every first load. Their text moved, unchanged, to `web/src/app-html.md`. `app.html` was X-page-frame's (merged), and the slice's owns were widened for comment removal only.
    - `index.html` gzipped went from 3,646 to 2,966 B. Bare `/search` measured 101,325 B, 1,075 B under the limit.
    - Collapsing the indentation would save about 145 B more; it isn't needed yet.
  - **Retry reloads the page.** Chromium 153 keeps a failed dynamic import failed for the whole module graph.
    - A one-shot `window.name` marker hands focus to the results, only while nothing else has focus (review, blocking). The marker never enters the URL.
    - No Enter-triggered reload.
    - A route-only "Retry" message keeps a shared message chunk off the first load.
  - **This supersedes one point of `2026-11-22-X-search-chunk-retry.md`.** It says the load that uses the marker on bare `/search` is always the one the user started by clicking into the bar. The 30 s fallback timer, or a keydown while the body has focus (a screen reader's browse mode), can start it too. Focus then moves to the empty results region, only while the body has focus. X-search-retry-flush (below) narrows it.
  - **The overlap with X-alert-noun** (`web/src/lib/features/search/messages/**`) ended when #1057 merged. X-search-retry-flush records its own.

## Calls in running slices
- **X-pseudo-text-gaps (#1056):** approved.
  - `common_just_now` lives in `web/src/lib/i18n/messages`. `relative.ts` drops its English table, and a direct caller that passes nothing gets Intl's own word.
  - Movement uses Insights' own level labels: reusing Settings' labels cost Settings messages 176 B, so it was reversed.
  - An unknown level or kind reads as a sentence, not a raw value, with `Object.hasOwn` on both lookups.
  - `KNOWN_GAPS` stays as an empty array with a comment.
  - Size: Search results (lazy) went from 40.84 to 40.85 kB (measured 40,841 B).
- **X-infra-filter-workflows (#1058):**
  - INFRA matches every top-level `.github/workflows/*.y(a)ml`, instead of a list.
  - It also matches the top level of `docs/operations/`, which `test_block.py` reads. `docs/operations/imports/**` is left out: no infra test reads it, and each match would cost a ~7-minute infra run.
  - Its review found no other path the infra tests read outside INFRA. Two similar gaps go to X-workflow-tests-filter (below).
- **X-block-on-only-mode:**
  - **A block may run the treatment in every segment only when its rules file says `"mode": "on"` together with `"purpose": "screen"`,** and never with `winners`. `rules.parse` refuses anything else, and the VM side refuses it again (`mode_needs_control`).
  - An abba rules file, and its args, stay byte-identical.
  - The evidence copy-off returns carries a `mode` field (a fixed value).
  - `analyze_cr` names `no_control_arm` instead of the generic order error.
  - Its owns gain `infra/fleet/steps/rules.py`, `infra/fleet/steps/vm/scrub.py`, `docs/soak/tools/analyze_cr.py` and its test. Its own PR updates its `slices.yaml` entry.
- **Not new:** the vitest timeout in `screen.test.ts` that the #1053 session saw under load is X-filters-dialog-test-timeout, already fixed in #1035.

## Process
- **Adjacent `slices.yaml` lines cost CI rounds.** #1051 conflicted twice with neighbouring entries from other slices, each time a full CI round. From now on the runner plans the landing order when it places entries on adjacent lines. A slice whose entry sits next to another open PR's waits in the landing lane until that PR lands, then brings `main` in once.
- **Usage:** the account's 7-day usage is near its limit. The runner keeps slices small and launches no extra testing sessions unless a merge needs one.

## New slices
- **X-pool-first-request-docs:** the pool's docs, fixtures and budget model follow peer-wire's first request after #1054: 80 KiB plus the bound for a dict over 5 pieces, and 64 KiB then growth for a 5-piece dict.
  - The model's invariant slack becomes "share + one first request", and the model reserves peer-wire's sizes instead of a hand-written 64 KiB.
  - It also takes #1054's review nits in `fetch.rs` and `utp.rs`.
  - Docs, fixtures and the model only; no pool rule changes. `crates/peer-wire/src/lib.rs` is in its owns for one re-export only.
- **X-search-retry-flush:**
  - Retry flushes the bar's text into the URL first, so a query typed within the bar's 200 ms debounce isn't lost.
  - A failed detail-drawer chunk shows a message, instead of an uncaught rejection and nothing.
  - The Retry marker is honoured only for a load that starts soon after the reload.
  - It fixes the stale glob comment. It owns the Search messages for the drawer's failure text, so it never runs beside X-alert-noun.
  - No raise to the 45 kB cap.
- **X-workflow-tests-filter:**
  - `test_workflow_concurrency.py` reads `web.yml` and `e2e.yml` but runs only when Rust changes; it should run on any workflow change.
  - `infra/fleet/tests/test_workflow.py` should glob `.yaml` as well as `.yml`.
  - It never runs beside another slice that owns `ci_changes.py`.
- **X-openapi-400s-tidy:** the OpenAPI 400 test checks that each exception entry matches exactly one route, plus the reason and wording nits from #1053's review.
- **X-fixture-server-kill-on-drop:** the fixture-server test helper kills and reaps its child when a test panics.
