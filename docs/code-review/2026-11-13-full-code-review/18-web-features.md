# Web feature screens review
Reviewed at 55e155a9.

## Coverage
- Read in full:
  - `web/src/lib/features/logs/` (all non-test files).
  - Settings form engine: `settings/form/{form.svelte.ts,saving.ts,batch.ts,merge.ts,validate.ts,instant.ts,draft.ts,draft-store.ts,values.ts,requests.ts,schema.ts,leave.ts,messages.ts,questions.ts,dialogs.ts,display.ts,SettingsForm.svelte,SettingField.svelte,SecretControl.svelte,ChoiceControl.svelte}`.
  - Settings → Updates: `updates/{store.svelte.ts,logic.ts,UpdatesScreen.svelte,Releases.svelte}`.
  - Settings → Security: `security/{Keys,PasswordDialog,ChangePassword,Proxy,SecurityScreen,Sessions,Reveal}.svelte`, `reveal.ts`, `confirm.ts`, `guard.ts`, `leave.ts`.
  - Settings → Companions: every non-test file in `settings/companions/`.
  - Settings connection screens: `screens2/{TestButton,GroupTest,ClientCard,ClearUsage}.svelte`, `clients.ts`, the first half of `ClientsWidget.svelte`.
  - `settings/about/{logic.ts,Diagnostics.svelte}`, `settings/outputs/logic.ts`, `settings/shell/export.ts`.
  - Filters: `filters/screens/{model.svelte.ts,ops.ts,FiltersScreen.svelte,Switches.svelte,Rules.svelte}`, `filters/flows/{machine.ts,flow.svelte.ts}`, the script of `ChangeDialog.svelte`.
  - `blocklists/{BlocklistsScreen.svelte,lists.ts}`.
  - Sources: `sources/{sources.ts,SourcesScreen.svelte,SourceRow.svelte,YieldBar.svelte,config-form.ts}`, the scripts of `ConsentDialog.svelte` and `PurgeDialog.svelte`.
  - Alerts: `alerts/{list.svelte.ts,detail.svelte.ts,throttle.ts,since.ts}`, the script of `AlertDetailPage.svelte`, the path builders in `api.ts`.
  - Insights overview: `insights/overview/{model.ts,query.ts,links.ts,state.svelte.ts,KpiRow.svelte,Funnel.svelte,Segments.svelte,Exceptions.svelte,CustomRange.svelte}`.
  - `appearance/{themes.ts,boot.ts,editor.ts}`, `discover/{discover.svelte.ts,unavailable.ts}`, `jobs/jobs.ts` and the script of `jobs/JobsScreen.svelte`, `restart/{restart.svelte.ts,watch.svelte.ts,logic.ts}`.
- Skimmed (grep for sinks, hrefs, arithmetic, time zones, timers, tokens; not read line by line): `insights/stages/*`, `insights/workers/*`, `insights/digest`, `insights/print`, the rest of `insights/overview/*.svelte`, `settings/logging/*`, `settings/outputs/{Torznab,Bitmagnet}.svelte`, `settings/overview/*`, `settings/shell/*`, `settings/screens2/*` (rest), `settings/updates/Advanced.svelte`, `companions/{NeedChip,SetupFlow}.svelte`, `restart/*.svelte`, `sources/ConfigDialog.svelte`.
- Mechanical checks over all the assigned folders plus `jobs` and `restart`: `{@html}`/`innerHTML`/`srcdoc`/`eval`, every dynamic `href`/`src`, arbitrary Tailwind values, hex colours, off-ramp spacing utilities, en/zh key parity, fixed waits in tests, API paths with no web caller.
- Not read: `compare/*` (only its hrefs were checked), `filters/flows/{OverrideDialog,BlockThisDialog,RevealNameDialog,PreviewTable}.svelte`, `filters/screens/{Queue.svelte,packs/*}`, `appearance/*.svelte`, `alerts/{AlertsListPage,SaveSearchDialog,DeleteAlertDialog}.svelte`, `discover/DiscoverPage.svelte`, `settings/logging/{privacy.ts,rules.ts,DataKinds,EventList,Preview}`, `settings/about/AboutScreen.svelte`, the on-demand form controls (`Number`, `Slider`, `Chips`, `Json`), `settings/form/preview.ts`. Reason: time; the session was interrupted once and the remaining budget went to verifying findings. Tests were sampled, not read.
- Not run: no build, unit test or browser. Every finding is from reading code.

## Findings

### WEBFEAT-01 Wizard steps built from string keys never load their section's labels: raw field names and enum values are shown
- Severity: important (a deviation from §10.14 and standards §4 "Strings"; blocking under standards §5 item 6 once confirmed in a browser)
- Kind: correctness
- Confidence: likely (traced in code, and a unit test pins the raw value; not run in a browser)
- Where: `web/src/lib/features/settings/form/SettingsForm.svelte:44-53`; `web/src/lib/features/settings/form/form.svelte.ts:289-305` (`load()`); `web/src/lib/features/settings/form/messages.ts:27-34`; `web/src/lib/features/wizard/IntensityStep.svelte:13`; `web/src/lib/features/wizard/StorageStep.svelte:35`; `web/src/lib/features/wizard/FieldsStep.svelte:22,39-43`
- What: `SettingsForm` reads `form.specs` once, untracked, when it mounts, and loads only those sections' message maps. A form created from `FieldKey[]` awaits `loadSettingsIndex()` before it sets `specs`, so `specs` is `[]` at that moment. `needed([])` is then only `['group']`, and the `crawler` and `storage` maps are never requested. `fieldLabel`, `fieldHelp` and `optionLabel` fall back to `spec.name`, no help and the raw enum value. The labels are `$derived` from a non-reactive module map, so they never correct themselves.
- Failure scenario: on first run, the wizard's "Crawl intensity" step shows a field labelled `intensity` with options `low`, `balanced`, `aggressive` and no help. "Storage & retention" shows `archive_raw` and `mmap_mib`. The messages exist ("Crawl intensity", "Low", "Database mapping limit"). `wizard/WizardPage.test.ts:102` asserts `findByRole('radio', { name: 'low' })`, an exact match, while `settings/form/LazyControl.test.ts:99` finds "Balanced" once the map is loaded. The e2e check (`e2e/wizard.keyboard.test.ts:127-131`) uses Playwright's case-insensitive substring match, so it passes either way. The step only looks right if a Settings screen loaded the same map earlier in the tab. `DownloadStep` passes `FieldSpec[]` and is not affected.
- Suggested fix: make the map load follow `form.specs` (an `$effect` that calls `loadMessages` for the specs' sections and gates `labelsReady` on it). Add a wizard test that asserts the translated label and option.
- Tracked?: not found.

### WEBFEAT-02 Filters & Safety never shows the server's pack-state conditions
- Severity: important
- Kind: gap
- Confidence: confirmed
- Where: `crates/api/src/dto/filters.rs:468-477`; `crates/api/src/filters.rs:2499-2513`; no reader in `web/src` (only test fixtures, which set `pack_conditions: []` and `packs_seeded_over_rules: null`); `web/src/lib/features/filters/screens/Switches.svelte`, `screens/Packs.svelte`, `screens/packs/PacksScreen.svelte`
- What: `GET /filters` returns `pack_conditions` (`packs.lost`, `packs.differ`, `packs.skipped.*`, `packs.shadow-in-doubt`, `packs.review-unreadable`, `packs.sp4.row.<n>`, unreadable-override labels) and `packs_seeded_over_rules`. `docs/decisions/2026-11-05-RP-3.md:9` says they are "for the Filters & Safety notice". The screen reads neither field and renders each pack as a plain On/Off badge.
- Failure scenario: a pack-version row is lost or unreadable. The server runs the interim (stronger) mode and reports `packs.lost`. The admin sees every pack "On" with nothing saying the rule set is degraded, that shadow evidence is in doubt, or that review marks were lost. The server does fail closed; the UI presents the degraded state as fine.
- Suggested fix: a warning banner on Filters & Safety and on the Packs screen that words each condition, with a fallback for an unknown code. Add tests with a non-empty fixture.
- Tracked?: not found in `runner/slices.yaml` or `docs/build-plan.md`. Insights does show the related `filter_rows_unreadable` exception (`insights/overview/Exceptions.svelte:45-49`).

### WEBFEAT-03 Saving a source's settings replaces every saved setting with what the form shows, and the form always starts from defaults
- Severity: important
- Kind: data-loss
- Confidence: confirmed (a recorded decision; listed because the follow-up has no slice)
- Where: `web/src/lib/features/sources/config-form.ts:149-157,236-262`; `web/src/lib/features/sources/ConfigDialog.svelte:52-64`; `web/src/lib/features/sources/SourcesScreen.svelte:139`
- What: `Source` carries no saved `config`. The dialog fills each field from the schema default, and `PATCH /sources/{id}` with `config` replaces the whole config. The dialog's intro says so, and Save is disabled when a setting can't be drawn.
- Failure scenario: a user who saved a list of feed or cache addresses opens Settings… to change one number. The list field shows its default (empty). Saving discards the saved list unless every value is typed again. The intro sentence is the only guard.
- Suggested fix: return the saved (non-secret) `config` on `Source` and start the form from it, as the decision file proposes. Until then, ask for a second confirmation that names what is replaced.
- Tracked?: recorded in `docs/decisions/2026-11-07-SRC-15c.md:16` ("Follow-up for the API: return the saved `config`"). No slice for the follow-up found in `runner/slices.yaml`.

### WEBFEAT-04 Live log: a `resync` frame empties the view, also while paused, and reopens with no backoff
- Severity: important
- Kind: resource
- Confidence: confirmed for the wipe; suspected for the loop (not reproduced; it needs a sustained rate above about 256 matching lines between the history answer and the stream attach)
- Where: `web/src/lib/features/logs/logs.svelte.ts:285-307` (`handle`), `:135-174` (`open`); `crates/api/src/logs/stream.rs:258-270`
- What: on any `resync` the client calls `open()`. That empties `lines` and `heldBuf`, zeroes the counters, refetches the newest 500 lines and reconnects, with `paused` left true. The server keeps the stream going after `lagged`, and treats a resume more than 256 matching lines behind as `lagged` again (`docs/decisions/2026-10-23-B-log2.md:18`). `open()` is not routed through `backoff()`.
- Failure scenario: (1) the user pauses to read a burst; one `lagged` resync replaces the frozen view (up to 10,000 lines) with the newest 500, although the header comment says pause freezes what is shown. (2) At a sustained high rate (Everything verbosity on a busy crawler) each reopen can be answered by another `lagged`, so the tab issues a `GET /logs` plus a stream request per iteration as fast as the network allows, at the moment the server is busiest.
- Suggested fix: on `lagged`, keep the lines and the open stream and show the skipped count (the server's design). Reopen only for `restarted`, `expired` and `malformed`, and send repeated reopens through `backoff()`. Add a `resync` test.
- Tracked?: `docs/decisions/2026-10-31-S-logs-1.md:5` records "A `resync` reloads from the ring", so the reload is deliberate. The paused case and the missing backoff are not recorded. `logs.svelte.test.ts` has no `resync` test.

### WEBFEAT-05 Updates store drops the newer of two overlapping status reads
- Severity: polish
- Kind: correctness
- Confidence: likely (the logic is confirmed; the timing window is narrow)
- Where: `web/src/lib/features/settings/updates/store.svelte.ts:116-133`
- What: `refresh()` takes `ticket = seq` when it asks, and `take()` increments `seq`. Two reads in flight share a ticket. The first answer is taken; the second, asked later, is dropped because `ticket !== seq`.
- Failure scenario: an `updates` event `failed` starts read 1, and `idle` right after starts read 2. Read 1 was served before the state changed and arrives first. Read 2, the current state, is dropped. The screen keeps the older status (for example `installing`, which hides Install now) until the next event or a reload.
- Suggested fix: number each request and accept an answer when its number is higher than the last one applied.
- Tracked?: not found.

### WEBFEAT-06 Cancel on a running import or export has no confirmation
- Severity: polish
- Kind: correctness
- Confidence: confirmed
- Where: `web/src/lib/features/jobs/JobsScreen.svelte:136-170,309-320`
- What: the Cancel button calls `POST /jobs/{id}/cancel` on the click. A cancelled job can't be resumed. §10.10.7 grades not-undoable actions as needing a dialog with focus on Cancel.
- Failure scenario: a mis-click (Pause and Cancel sit side by side) ends a multi-hour import.
- Suggested fix: ask first, naming the job and how far it is.
- Tracked?: not found (`docs/decisions/2026-11-01-SRC-16.md` covers the client-derived progress only).

### WEBFEAT-07 A companion's log lines are shown without the live log's control-character handling
- Severity: polish
- Kind: security
- Confidence: confirmed
- Where: `web/src/lib/features/settings/companions/Logs.svelte:20-32`
- What: `lines.join('\n')` goes into a `<pre>` as text. There is no markup risk. But a companion's output (qBittorrent, Prowlarr) carries torrent and indexer names, and bidi overrides, ANSI escapes and zero-width characters pass through. The live log makes all of these visible (`logs/text.ts` `safe()`).
- Failure scenario: a torrent name with U+202E reorders the text of a line in the panel an admin reads to judge what a companion did.
- Suggested fix: run each line through `safe()` (or a shared copy of it) with a length cap.
- Tracked?: not found.

### WEBFEAT-08 Raw identifiers shown where a message is expected
- Severity: polish
- Kind: gap
- Confidence: confirmed
- Where: `web/src/lib/features/logs/LogsPage.svelte:314-322,392-394` (component names as filter buttons and in rows); `web/src/lib/features/insights/overview/Segments.svelte:78-81` (source key as the bar label); `insights/overview/Funnel.svelte:57` and `KpiRow.svelte:41` (an unknown loss or health reason shows its code); `web/src/lib/features/sources/ConsentDialog.svelte:118,201` (a regex edits a translated sentence: `replace(/ Nothing was changed\.$/, '')`, `replace(/\.$/, '')`)
- What: these strings bypass `messages/{en,zh}.json` (standards §4 "Strings"). The two `replace` calls match English punctuation only, so in zh the sentence is left as it is and contradicts the save that just happened.
- Suggested fix: messages for the 13 log components; the source's display name for the Insights bars; separate message keys instead of editing a sentence.
- Tracked?: not found.

### WEBFEAT-09 Jobs formats a rate with `Intl.NumberFormat` directly
- Severity: polish
- Kind: maintainability
- Confidence: confirmed
- Where: `web/src/lib/features/jobs/JobsScreen.svelte:68-71`
- What: standards §4 says numbers go through `src/lib/format` only. The rate is formatted in place. (The client-computed percentage on the same screen is a recorded decision, `docs/decisions/2026-11-01-SRC-16.md:7-8`.)
- Suggested fix: use `formatRate`.
- Tracked?: not found.

## Gaps (the brief asked for a list)

Backend with no UI. "No web caller" means no reference anywhere in `web/src` outside `schema.d.ts`, tests and fixtures.
- **Settings import**: `POST /settings/import` (dry-run diff, then apply) exists and §10.10.6 promises "Import … shows a diff … with Undo". Only export has a UI (`settings/shell/SettingsExport.svelte`). Tracked?: not found.
- **Workers management**: `/workers/tokens`, `/workers/{id}` and its `drain`, `pause`, `resume`, `revoke` have no web caller (`/workers` is read only for names in Insights). Tracked: WK-16 "Settings: Workers screen" in `runner/slices.yaml`, not built. The brief for this review mentions a workers panel in Settings; there is none.
- **.torrent upload and folder watch**: `/torrents/upload` has no web caller, and the `watch` settings section has a message map but no screen. Tracked: UP-2.
- **No web caller, tracking not checked one by one**: `/collections*`, `/user-items*`, `/classify`, `/classify/correct`, `/classify/rules*`, `/webhooks*` (token management for the Sonarr/Radarr webhooks), `/filters/tombstones`, `/filters/test`, `/filters/preview` (the UI uses the job form), `/alerts/unread`, `/keys/{id}`, `/companions/{id}/confirm`, `/companions/{id}/plan`, `/companions/{id}/guided/forget`.
- **"Reset all settings"** is named in §10.10.7 as a Danger-zone action; there is no such control (only `lodestar settings reset --all` in §10.10.6).

UI whose backend is a stub or partial.
- Updates → Roll back on Windows: the server answers `rollback_unavailable` with `not_built_yet`, and the screen has words for it (`UpdatesScreen.svelte:92,214`). PR 795 is open.
- The restart banner appears only on the next `/system` poll (route change, focus, or every 15 s on Settings: `restart/watch.svelte.ts:17-41`), so it lags a save. PR 790 (X-restart-save-lag) is open for this.
- Insights exceptions: the client words six kinds; `docs/metrics.md:371` lists more that still wait for a server counter. An unknown kind shows a generic sentence, so nothing is hidden.

## Checked and found sound
- No `{@html}`, `innerHTML`, `srcdoc` or `eval` in any reviewed folder. Server-supplied links are checked before use: `updates/logic.ts` `safeHttps`, `about/logic.ts` `repoBase`/`httpsUrl`, `insights/overview/links.ts` `fixHref` (pattern-checked target), `alerts/api.ts` (`torrentPath` only for 40-hex, numeric ids truncated), `logging/LogFiles.svelte` (`encodeURIComponent` on file names). External links carry `rel="noopener noreferrer"`.
- Live log text: every untrusted string goes through `safe()` (`logs/text.ts`), which escapes C0/C1 controls (so ANSI ESC), bidi and zero-width characters and lone surrogates, and bounds the length before the regex runs. Rendering is text nodes only, the list is virtualised and capped at 10,000 lines (held lines too), reconnects back off with jitter, and `stop()` releases the stream, timers and listeners.
- Settings secrets: `SecretControl.svelte` never pre-fills and uses `type=password` with `autocomplete=new-password`. Staged secrets are cleared after every save attempt (`saving.ts` `clearSecrets`, `form.svelte.ts` `dropSecrets`), are left out of Undo and of `sessionStorage` drafts (`draft-store.ts:44`), and are never sent half-typed in instant mode. Drafts are capped at 64 KiB and shape-checked on read.
- Settings save: one atomic `PATCH /settings` with `if_revision` per section; only dirty fields are sent, so a field the server added later is never dropped; `409 stale_revision` runs a three-way merge with explicit per-field conflicts; a `settings` event from another tab is held during the form's own save; an unknown `x-danger` is treated as `security`; a missing gate fails closed; Undo is never merged on a stale revision.
- "Test connection" (`screens2/TestButton.svelte`, `GroupTest.svelte`, `ClientCard.svelte`): a typed secret is sent only with the values on screen, and the server uses a saved secret only when the address matches the saved one (`crates/api/src/settings/checks.rs:4-7`).
- Security screen: passwords are cleared before each request and never drafted; a key's secret lives only in the reveal dialog's props; open dialogs are dismissed on unmount; the lock-out (429) holds the button.
- Companions: install echoes only the offer's version and SHA-256; remove shows the server's deletion plan first; a revealed credential is dropped on Hide and on unmount; every failure is worded from a stable code, never the server's text.
- Filters change flow (`filters/flows/machine.ts`): the server's assessment decides whether the password is asked; apply names the previewed floor entries, held version and pack versions; a close while applying waits for the answer; no switch is flipped locally. Blocklists show "measured by Lodestar, not yet fetched", the held state and the last error next to the Active badge.
- Insights: no client arithmetic on shown numbers (grep for `Math.`, `reduce`, `* 100`, `toFixed` is clean; bar widths are drawn from counts); nulls go through the formatters; harvested trends, latency percentiles and time-to-searchable carry the approximate mark; time axes and labels use the server's `range.tz`, and the custom range is in UTC days; every number sampled has a `data-testid`; chart panels carry a table alternative and a CSV link.
- Sources: the row is replaced by the PATCH reply, and a list read that started before the reply is dropped; credentials are write-only and cleared after every submit; purge previews the counts, asks for the password and follows the job's own state.
- Appearance: custom themes read from `localStorage` are filtered to hex values and exact token-key lists before they are applied with `style.setProperty`; an imported theme goes through the build's `validateFamily`.
- Tokens and i18n: no arbitrary Tailwind values, no hex in components and no off-ramp spacing utilities in the reviewed folders. en and zh key sets are identical in every reviewed `messages/` folder.

## Area-level concerns
- **zh placeholders.** `sources/messages/zh.json` has 36 strings that are English copies (`sources_cfg_*`, `sources_cx_credential_*`, `sources_purge_*`). This is recorded in `docs/decisions/2026-11-07-SRC-15c.md` ("zh" section). `web/scripts/messages.zh-complete.test.ts` checks keys and placeholders only, so it can't catch an English placeholder.
- **Tests match loosely where it matters.** The wizard e2e and the settings e2e select radios by `name: 'low'`, which Playwright matches case-insensitively as a substring, so a raw enum value and its label both pass (WEBFEAT-01 went through this way). The live log has no `resync` test. No fixture anywhere sets a non-empty `pack_conditions`.
- **Fixed waits.** None of the "assumes work finished" kind in the reviewed features. Three unit tests flush with `setTimeout(r, 0)` (`insights/workers/workers.test.ts:15`, `alerts/controllers.test.ts:16`, `companions/NeedChip.test.ts:283`), which is deterministic with mocked fetches. `web/e2e/support/events-server.ts:38-46` nudges 40 times at 100 ms; it paces a writer, but assumes the chrome mounts within 4 s on the slowest runner.
- **Lazy dialogs outlive their owner in a few places.** `ask()` is called without an abort signal in `SecretControl.svelte` (Remove), `security/Keys.svelte` (Revoke), `screens2/ClearUsage.svelte` and `security/guard.ts`, so the question stays open after a navigation and its confirm still sends. Each needs an explicit click, so the risk is small, but the other screens pass `store.signal`.
- **Settings is large (about 20,900 non-test lines) and hand-built screens repeat patterns** (load, stale banner, busy mark, lazy dialog door, focus restore) in Updates, Companions, Sources, Blocklists, Filters and Jobs. The copies agree today; `security/leave.ts` already needs a test to keep it equal to `form/leave.ts`.
