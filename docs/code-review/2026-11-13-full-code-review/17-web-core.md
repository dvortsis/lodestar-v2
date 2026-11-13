# Web core review
Reviewed at 55e155a9.

## Coverage
- Read in full:
  - `web/src/app.html`, `web/src/lib/theme/prepaint.js`, `web/svelte.config.js`, `web/vite.config.ts`, `web/package.json`, `web/scripts/{bans.mjs,check-bans.mjs}`
  - routes: `+layout.svelte`, `+layout.ts`, `+page.ts`, `+error.svelte`, `login`, `setup`, `welcome`, `search`, `t/[hash]`
  - `web/src/lib/api/client.ts`; `web/src/lib/state/{search.svelte.ts,events.svelte.ts,sse.ts}`
  - `web/src/lib/features/auth/*` (all non-test files)
  - shell: `Shell.svelte`, `Chrome.svelte`, `CommandPalette.svelte`, `StaleBanner.svelte`, `StatusPill.svelte`, `status.svelte.ts`, `stale.ts`, `nav.ts`, `density.ts`, `chrome.ts`
  - keys: `registry.svelte.ts`, `combo.ts`, `filter.ts`, `defaults.ts`; shortcuts: `boot.ts`, `capture.ts`
  - `web/src/lib/format/*` (non-test); i18n: `locale.svelte.ts`, `boot.ts`, `region.svelte.ts`; util: `magnet.ts`, `clipboard.ts`, `bidi.ts`
  - ui: overlays `Modal`, `Dialog`, `Drawer`, `Popover`, `Menu`, `motion.ts`; feedback `Banner`, `Toaster`, `toasts.svelte.ts`; `fields/Combobox.svelte`; `lists/VirtualList.svelte`; `charts/LineChart.svelte`
  - search: `query/{QueryBar,Live,FilterStrip,SyntaxPopover}.svelte`, `editor.ts`, `path.ts`, `spans.ts`; `results/{results.svelte.ts,ResultsPanel.svelte,ResultRow.svelte,RowActions.svelte,settle.ts,recover.ts,links.ts,suggestions.ts,composition.ts}`; `live/{live.svelte.ts,LiveHost.svelte,extras.ts}`; `facets/{facets.svelte.ts,pending.svelte.ts,FacetsHost.svelte}`; `bulk/{selection.svelte.ts,actions.ts,ops.ts}` and `BulkBar.svelte` (script); `assist/{Assist.svelte,recent.ts,suggest.ts,fields.svelte.ts}`
  - detail: `model.svelte.ts`, `cache.ts`, `open.ts`, `prefetch.ts`, `target.ts`, `commands.ts`, `magnet.ts`, `send.ts`, `visibility*.ts`, `visible.ts`, `fullscreen.ts`, `manual-mount.ts`, `DetailActions.svelte`, `DetailHostBody.svelte`, `ManualMagnet.svelte`, `adult/*`, `files/{FileTree.svelte,load.ts,tree.worker.ts}`
  - wizard: `WizardPage.svelte`, `steps.ts`, `LegalStep.svelte`, `network.svelte.ts`, `DoneStep.svelte`
  - restart: `flow.ts`, `watch.svelte.ts`, `restart.svelte.ts`, `logic.ts`, `text.ts`, `door.ts`, `AddressPrompt.svelte`, `RestartingPage.svelte`
  - jobs: all non-test files
- Skimmed: `crates/lodestar/src/web.rs` and `web_build.rs` (how the CSP is built, to check `app.html` against it); `crates/api/src/auth*`, `events.rs`, `search.rs`, `crates/search/src/index.rs` (grep and short reads, to confirm findings); `web/scripts/messages.ts` (first 120 lines) and `messages.zh-complete.test.ts`; `.size-limit.mjs` (entry names only); the area's tests (grep for fixed waits and skips; read the ones cited); every `messages/{en,zh}.json` in `web/src/lib` by script (key and placeholder parity)
- Not read (time): `web/scripts/{themes*.ts,assets.ts,licenses.ts,first-load.mjs,size-entries.mjs,fonts.mjs,event-names.mjs,dev-routes.mjs,color.ts}`; charts other than `LineChart`; ui `Table`, `Tree`, `tree-model`, `Tooltip`, `Sheet`, the `fields/*` and `labels/*` primitives other than `Combobox`; shell `TopBar`, `OverflowMenu`, `ErrorView`, `ConnectionView`, `StateScreen`, `status.ts`, `commands.ts`, most of `progress.ts`; shortcuts screens (`KeyboardScreen`, `ShortcutsField`, `ShortcutSheet`); wizard steps other than those listed and `companion/*`; search `clauses.ts`, `tokenize.ts`, `table/*`, `cards/*`, `toolbar/*`, `unverified/*`, `facets/{FacetRail,TermGroup,RangeGroup,SwarmGroup,toggle,bins}`, `assist/{Builder,BuilderRow,Natural,options,builder}`; detail `DetailView`, `Summary`, `Parsed`, `Provenance`, `RawViewer`, `Composition`, `files/build.ts`; restart `RestartBanner`, `RestartDialog`, `ReviewList`; the thin route files of the other reviewer's features; `web/e2e/**`; most unit tests in full. `node_modules` is not installed, so SvelteKit and Bits UI behaviour was not checked against their source.

## Findings

### WEBCORE-01 "Matched in N files" never shows more than 20
- Severity: blocking
- Kind: correctness
- Confidence: confirmed
- Where: `web/src/lib/features/search/results/ResultRow.svelte:91,217-224`; server cap at `crates/api/src/search.rs:54,569-580` and `crates/search/src/index.rs:1714-1737` (`out.truncate(limit)`); `docs/architecture.md:2216` ("at most 20")
- What: the row prints `formatCount(hit.matched_files.length)` as "Matched in {count} files". `matched_files` is the list of matched positions, cut to `MATCHED_FILES = 20` on the server with no "more" flag and no separate count. So the client computes a count from a truncated list (standards §4: the client never computes counts) and shows it as exact.
- Failure scenario: search `ext:mkv` (or any `file:(…)` clause). A season pack with 150 matching files shows "Matched in 20 files", the same as one with exactly 20. The only test (`ResultRow.test.ts:121-128`) uses three positions.
- Suggested fix: have the API send the matched-file count (and whether it is capped) on the hit and print that; until then show "20 or more" wording when the list is at the cap. `docs/architecture.md:447` asks for a "Matched in files: …/S01E03.mkv" line (a path, not a count); the path form would avoid the count altogether.
- Tracked?: not found (`runner/slices.yaml`, `docs/decisions/2026-10-24-S-search-2a.md` only fixes the row height)

### WEBCORE-02 After "Block this…", the "N results removed" announcement is usually missing and sometimes the wrong number
- Severity: important
- Kind: correctness
- Confidence: confirmed
- Where: `web/src/lib/features/search/results/settle.ts:8-19`; `web/src/lib/features/search/results/results.svelte.ts:128-140` (`reload(keep)` refills to at least `keep` rows, in pages of 50 or 200)
- What: `gone = had - rows.length`, where `rows` is the list after a reload that refetches at least as many rows as were shown. The difference is a count computed in the client from two page lengths, not the number of results the rule removed.
- Failure scenario: (a) 50 rows shown of 10,000; block one: the reload returns 50 rows again, `gone = 0`, nothing is announced (the usual case). (b) 100 rows shown; the reload uses 200-row pages and returns 200: `gone = -100`, nothing announced. (c) 100 shown of 103; the rule hides 10: the reload returns 93, "Blocked. 7 results removed." though 10 went. Only a fully loaded list gives the right number, which is the one case the test covers (`RowActions.block.test.ts:59-65`, 3 rows to 2).
- Suggested fix: announce from server numbers: the difference of `results.total` before and after when neither is approximate (or the count the block flow's own preview already has), else a message without a number.
- Tracked?: not found

### WEBCORE-03 One 4xx answer from `/events` stops the stream for the life of the tab
- Severity: important
- Kind: correctness
- Confidence: confirmed (by reading; not run)
- Where: `web/src/lib/state/events.svelte.ts:77,159,193-195,271-297`
- What: any 4xx other than 401, 408, 429 and `notice_required` sets `rejected = true`. The flag is cleared only when the consumer count reaches zero (`refresh()`, line 280). `refresh()` returns early while it is set (line 285), so a change of the topic set does not retry, and nothing else does either. The shell always holds `crawler` and `rebuild`, so the count never reaches zero outside the sign-in screens.
- Failure scenario: a tab from a newer build asks for a topic an older server no longer has (a rollback; `crates/api/src/events.rs:977` answers 400 `unknown_topic`), or a reverse proxy answers 403/404 once while the server restarts. The stream goes to `stopped` and stays there: the status pill, live result counts, settings sync and visibility invalidation (`detail/visibility-events.ts`) stop silently until the page is reloaded, even after the consumer with the bad topic has unsubscribed.
- Suggested fix: clear `rejected` when the topic set differs from `connectedTopics`, and retry a rejected stream with a long backoff (or on `visibilitychange` / `onSessionOk`) instead of never.
- Tracked?: not found

### WEBCORE-04 `LineChart` rebuilds uPlot on every data update once its shape or format has changed
- Severity: important
- Kind: resource
- Confidence: likely (reasoned from Svelte 5 dependency tracking; not run)
- Where: `web/src/lib/charts/LineChart.svelte:97-108,114-136`
- What: the rebuild effect is `void [shape, fmt]; create?.();`. On the first run `create` is undefined, so the effect depends only on `shape` and `fmt`, and the first chart is built from the import's promise callback (untracked). When `shape` or `fmt` later changes (bucket, unit, locale, time zone), the effect calls `create()` inside itself; `mount` reads `times`, `series`, `height` and the other props synchronously, so they become dependencies of the rebuild effect. From then on every new sample destroys and reconstructs the chart, as well as calling `setData`. The comment above the effect says the opposite ("New samples go through setData and redraw in place").
- Failure scenario: on Insights, change the bucket or range once, then let the series refresh: each refresh tears the chart down (hover legend and cursor reset, extra work per poll). The unit test passes because it checks the constructor count only before the first shape change (`LineChart.test.ts:66-76`).
- Suggested fix: `untrack(() => create?.())` in the effect; extend the test with a data update after the `unit` change.
- Tracked?: not found

### WEBCORE-05 `LineChart.test.ts` waits a fixed 20 ms and then asserts that nothing happened
- Severity: important
- Kind: test
- Confidence: confirmed
- Where: `web/src/lib/charts/LineChart.test.ts:95-101`
- What: "does not create a chart if unmounted before uPlot loads" sleeps 20 ms and expects the constructor not to have been called. If the mocked dynamic import settles later than 20 ms (a loaded Windows runner), the assertion passes whether or not the guard works, so the test cannot fail for the bug it names. `runner/standards.md` §2 counts a sleep that assumes work has finished as blocking in a PR.
- Suggested fix: await the same `import('uplot')` promise (or a hook the component exposes) before asserting.
- Tracked?: not found (PR 778 is about other flakes)

### WEBCORE-06 After Back from the sign-in page, a 401 no longer redirects
- Severity: polish
- Kind: correctness
- Confidence: confirmed (by reading; `client.test.ts` "a successful public /auth does not reset the de-duplication" pins the mechanism)
- Where: `web/src/lib/api/client.ts:117-134,151-156`; `web/src/lib/features/auth/nav.ts:21-23` (the redirect pushes, "so Back leaves the login")
- What: `loginPending` is set by the first 401 and cleared only by a successful request other than `/auth` and `/logout`, or by re-registering the handler (done once). After the redirect to `/login?next=…`, pressing Back returns to the app page; its requests answer 401 again and are dropped without a redirect.
- Failure scenario: session expires on `/search?q=x`; the app goes to sign-in; the user presses Back: the page shows its error states and never offers sign-in again until a reload.
- Suggested fix: clear `loginPending` when the handler's navigation settles, or on any navigation away from `/login` and `/setup`.
- Tracked?: not found

### WEBCORE-07 A superseded reload of the same query clears the newer request's "refining" state
- Severity: polish
- Kind: correctness
- Confidence: confirmed (by reading)
- Where: `web/src/lib/features/search/results/results.svelte.ts:95-162` (the `finally` at 156-161), `reload`/`retry` at 273-283, callers `RowActions.svelte:5`, `LiveHost.svelte:69-70`
- What: `reload()` and `retry()` call `run(current, true)` with the same query object. When a second one starts while the first is in flight, the first is aborted, returns from `catch`, and its `finally` sees `current === q` and sets `refining = false` and `inflightKey = null` while the second request is still out.
- Failure scenario: `ls-stale` (a Block in the drawer) arrives during a "Show" reload, or Retry is pressed twice: the hairline and `aria-busy` go off early, and with `inflightKey` null a `load()` for the same key after an earlier failure sends a duplicate request.
- Suggested fix: guard the `finally` with a per-run token (as `live.svelte.ts` does with `pending`) instead of object identity.
- Tracked?: not found

### WEBCORE-08 The bulk selection keeps rows that a reload removed
- Severity: polish
- Kind: correctness
- Confidence: confirmed (by reading)
- Where: `web/src/lib/features/search/bulk/selection.svelte.ts:39-43`; `web/src/lib/features/search/results/ResultsPanel.svelte:72`; `results.svelte.ts:148` (a reload keeps `shownKey`)
- What: the selection is cleared only when `results.shown` changes. A reload of the same query (after Block, after "Show", after an expired cursor) keeps the key, so hashes that are no longer in the list stay selected.
- Failure scenario: select five rows, block one of them: the bar still says five selected, and Copy magnets, Export and Compare act on a row that is no longer shown.
- Suggested fix: after a reload, intersect the selection with the loaded hashes (or clear it when rows were dropped).
- Tracked?: not found

### WEBCORE-09 Recent searches stay in `localStorage` after sign-out
- Severity: polish
- Kind: security
- Confidence: confirmed that nothing clears them (`features/auth/session.svelte.ts:59-63`); likely that a signed-out visitor sees them (the list opens on focus of an empty bar before any request has answered 401)
- Where: `web/src/lib/features/search/assist/recent.ts:3-30`; `web/src/lib/features/search/assist/Assist.svelte:85-91,172-174`
- What: the last eight queries are kept under `lodestar.search.recent` with no tie to the session or to "Show adult content". `docs/decisions` (S-search-4) records "this browser only" but not what happens at sign-out. The app otherwise treats what was searched as private (the privacy banner, the adult gate).
- Failure scenario: on a shared browser profile, after Sign out, opening `/search` and focusing the bar lists the previous person's queries for a moment before the 401 redirect; they also remain readable in storage.
- Suggested fix: remove the key in `logout()` and when the session becomes `signed_out`; consider not recording while adult content is shown.
- Tracked?: not found

### WEBCORE-10 Jobs screen formats the rate with its own `Intl.NumberFormat`
- Severity: polish
- Kind: maintainability
- Confidence: confirmed
- Where: `web/src/lib/features/jobs/JobsScreen.svelte:67-70`
- What: standards §4 routes every number through `src/lib/format`; the rate builds a formatter per call in the component (also not cached). The client-derived progress percentage on the same screen is a recorded, runner-accepted decision (`docs/decisions/2026-11-01-SRC-16.md`), so only the rate is raised here.
- Suggested fix: use `formatRate`/`formatCount` or add the formatter the screen needs to `lib/format`.
- Tracked?: not found

## Checked and found sound
- No `{@html}`, `innerHTML` with data, `eval` or `new Function` in the area. The one `insertAdjacentHTML` (`app.html:150`) inserts a constant string. The Legal Notice Markdown is parsed to nodes and rendered as text; its links are limited to literal `https://` (`features/auth/markdown.ts`, `LegalNotice.svelte`).
- `safeNext` (`features/auth/next.ts`): rejects `//host`, `/\host`, control characters, dot-segment forms that resolve to `//host`, the auth pages and `/api`; length-capped. `toNext` falls back to `/` if the router refuses.
- Magnets: only the server's `magnet` field is used, after an `isMagnet` scheme check, for both copy and open (`features/detail/magnet.ts`, `util/magnet.ts`). The adult-enrichment link is shown only when it parses as `https:` with a host and no credentials, with `rel="noopener noreferrer"` (`detail/adult/link.ts`).
- Torrent names, raw names, file names and enrichment text go through `showBidi` wherever they are rendered in the files read (`ResultRow`, `Card`, `ResultsTable`, `DetailHostBody`, `/t/[hash]`, `files/build.ts`, `RawViewer`, `AdultEnrichment`).
- Nothing secret in web storage: storage holds theme, region, shortcuts, column and drawer-width preferences, wizard progress and recent queries (WEBCORE-09). The setup token and passwords live in component state and are cleared after a successful submit; server error text is never shown on the auth forms (`machine.ts` `classify`, `errors.ts`).
- The Legal Notice gate is enforced by the server (`crates/api/src/auth/guard.rs:174`, 409 `notice_required` on session routes; `POST /setup` takes `notice_version`); the client only routes to it. The `/welcome` wizard is not a gate and nothing depends on it being completed.
- `app.html` against the CSP (`crates/lodestar/src/web_build.rs:397-434`): both inline scripts and the static `style="…"` attributes are covered by hashes, there are no inline event handlers, `frame-ancestors 'none'` covers clickjacking, and the file-tree worker is a same-origin module (`script-src 'self'`), not a blob.
- `latest()` (`api/client.ts:273-295`) drops a late answer even when the abort loses the race. `search.svelte.ts` (`parse`, `setAst`, `sync`, `queryReady`) and `results.svelte.ts` hold up against overlapping requests: an older parse or search cannot overwrite a newer one, and `queryReady` stops an old AST being sent for a new URL query. No retries of non-idempotent calls anywhere in the client.
- The query editor (`query/editor.ts`) builds DOM nodes from tokens, handles IME composition (`isComposing`, keyCode 229, `compositionend`), submits once for hardware and soft-keyboard Enter, and pastes plain single-line text.
- The key registry (`keys/registry.svelte.ts`): no single-key command fires in an input, select, contenteditable or ARIA textbox/combobox, inside a dialog or menu, during composition, or with a conflict; Enter and Space are left to focused controls; modifiers are part of the combo, so Ctrl+C never runs `c`.
- Events client: one stream per tab, listeners and timers are removed when the last consumer leaves, a hidden tab closes its stream after 10 s and reopens with `Last-Event-ID`, a superseded connection cannot queue events or move `lastId`, and the SSE parser handles CR, LF, CRLF and split frames.
- Formatters: null and non-finite values give "—", estimates take "≈", negative sizes and ratios give "—" (`format/shared.ts` `present`).
- Token bans: no hex colours, colour functions, named colours or arbitrary Tailwind values found outside `lib/theme` by an independent grep; raw lengths in feature CSS are 1-2 px borders and breakpoints only.
- Messages: all 39 catalogues have the same keys and placeholders in `en` and `zh` (3,944 keys), and `scripts/messages.zh-complete.test.ts` keeps it so. No hard-coded user-visible strings found in the area's components (test harnesses and code samples aside).
- uPlot is loaded with a dynamic import on first chart mount; the search route's results, facets, toolbar, drawer, Bits UI menu and the chrome are all lazy.
- Dependencies in `web/package.json` are pinned exactly.

## Area-level concerns
- **Counts derived in the client from list lengths.** WEBCORE-01 and WEBCORE-02 are the same mistake: a length of a capped or paged list shown as a count. A grep for `.length` passed to a message or `formatCount` found no third case in this area, but the API has several capped lists (`matched_files`, `file_list`, facet values), so the rule is worth a lint or a review checklist line.
- **Raw `fetch` beside the API client.** `bulk/actions.ts` (`POST /export`), `i18n/boot.ts`, `auth/notice.ts`, `shell/stale.ts`, `shell/status.svelte.ts` and `live.svelte.ts` (`pagehide`) call `fetch` directly, each for a stated reason. The cost is that a 401 there never reaches the sign-in redirect: an expired session on Export or Copy magnets shows "failed" and stays put. Not a finding on its own; worth a shared helper so the choice is explicit.
- **The 401 handler is installed from a lazy chunk** (`Shell.svelte:98-111`). A 401 that answers before `features/auth/session.svelte` has loaded is dropped (`client.ts:130`), and `Live.svelte` deliberately shows nothing for `login_required`. On a normal load the handler chunk wins (one round trip against two), and the e2e spec `search-query.routes.test.ts:307` covers it against a mock; I could not rule the race out on a slow asset path.
- **Live "N new results" count and URL changes.** The effect in `LiveHost.svelte:61-67` calls `live.reset()` and also reads `search.q/ast/sort/…`, so it re-runs on any URL change, not only when `results.shown` changes. A density or view toggle would then zero the count until the next `search_live` event. Suspected only: I could not confirm how `page.url` behaves under shallow `pushState` without the SvelteKit source, which also decides whether opening the drawer triggers it.
- **Boot shell language.** The inline script in `app.html:150` picks the language from `navigator.languages` only, while the app prefers the saved language (`+layout.ts`, `lodestar.region`). A person with an English browser and Chinese saved sees the English static bar first; one with a Chinese browser and English saved gets no static shell. Cosmetic.
- **Nested live regions.** `Banner` always carries `role="status"`/`alert`; several callers in this area sit beside or inside their own polite regions (`QueryBar`/`Live`, `ResultsPanel`, `FacetsHost`). PR 794 (X-banner-live-region) is open on exactly this; nothing further raised here.
- **Server text shown untranslated.** Parse-error messages (`Live.svelte:269`), job errors and refusals (`JobsScreen.svelte:157-164,305`), restart failures (`AddressPrompt.svelte:94`) and send refusals (`detail/send.ts:72`) print the server's English message in a `zh` UI. Rendered as text, so not a safety issue; a localisation gap the message catalogues cannot close.
- **Test quality.** Besides WEBCORE-05, `e2e/support/events-server.ts:41-47` `engage()` always spends 4 s (40 nudges, 100 ms apart) and is called 15 times; it paces a writer, so it is within the rule, but it adds a minute to the e2e run and could stop at the first sign of the chrome. The zero-delay `setTimeout` flushes in `AuthPage.test.ts`, `labels.test.ts`, `boot.test.ts` and `stale.test.ts` wait for microtask work and are deterministic. Fixture-backed e2e specs are skipped unless `E2E_FIXTURE_URL` is set; `e2e.yml` sets it in one job.
