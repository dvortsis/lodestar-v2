# Settings preparation during baseline work

Status: **source prepared; partial checks passed; not accepted**. This is a
separate product slice for DUR-13–15, 21–23. It is not part of the frozen native
soak candidate and has made no requests or writes to a crawler instance.

October 29: the owner explicitly requested pushing all pending changes while the
native soak continues. This branch is published as work in progress with the
known bundle failures below; it is not merge-ready. Existing validation evidence
is retained, and heavy revalidation is deferred until the active soak finishes.
Local test logs, caches and machine-specific browser adapters remain local.

## Source and dependencies

- Worktree: `target/worktrees/soak-settings`; branch: `work/soak-settings`.
- Checkout base: `5e1a53c06abec4fa7be420d84a2be6052c656b11`.
- The inherited prepared commits are already present. The validated, uncommitted
  frontend follow-ups were copied from `soak-followups`; their 36-file inventory
  is `target/frontend-dependency-files.json`. The bundle configuration's deferred
  manual-mount boundary was also carried over. Integrate this dependency once.
- Windows LF attributes and the shell-state module rename are shared baseline
  portability fixes. They are not new Settings behavior.
- The runner inventory has S-settings-1 ownership. A connected GitHub search
  found no open Settings PR in `dvortsis/lodestar-v2` before dependency integration.
  No publication or merge has occurred.

## Prepared behavior

- Grouped Settings navigation, current-page semantics, section-name filtering,
  a 720px manuscript column, and mobile list/detail navigation with density kept.
- General, Crawler plus Transport, Network, Storage and Search section mappings
  use the existing schema reader and form engine. A save remains atomic across
  API sections with their original revisions, previews, conflicts and leave guard.
- Network is explicitly read-only until its reachability and address recovery
  flows exist. Privacy, destructive and security fields remain locked in this
  generated screen; their dedicated confirmation flows are not implemented here.
- Advanced disclosures remember their state per screen. Changed-only retains
  dirty edits. Deep links reveal Advanced and focus a control after values load;
  later save-state changes do not continually steal focus.
- Load errors can retry; missing backend sections and unknown destinations have
  explicit unavailable states. Screen teardown destroys the form and ignores
  late schema responses.
- English and Chinese screen, group and current core field labels are present.
- Restart requirements now derive from the current server section values. They
  appear on initial load and update after resync or remote changes, as well as save.
- Deep-link and validation-summary focus share one control selector. A cold lazy
  choice loads before focus moves to its selected option, never the header menu.
- General → Backup & portability downloads the backend's saved-settings export,
  with preferences excluded unless selected. It supports cancellation, safe error
  feedback and retry, and distinguishes portable export from config.toml and a
  library backup. It neither exports staged edits nor implements import.

## Executed validation

All paths below are relative to this worktree; logs are local validation evidence.

| Check | Result and evidence |
| --- | --- |
| Offline dependency installation | Passed from the existing bounded npm cache; no new dependencies |
| Full frontend unit suite | 130 files; 2,352 passed, one Windows symlink-privilege skip, four workers; `target/bundle-accounting-unit-all.log` |
| Settings suite including export | 21 files, 341 passed, four workers; `target/settings-export-suite-r2.log` |
| Type checking including export | Zero errors, three existing Discover CSS warnings; `target/settings-export-check-r2.log` |
| Full frontend check | Passed (type checking, ESLint, formatting and token bans); `target/bundle-accounting-check-r2.log`; three existing CSS warnings remain |
| Diff whitespace check | Passed; `target/settings-diff-check.log`; only LF checkout notices |
| E2E TypeScript | Passed including export and cold-font fixtures; `target/bundle-accounting-e2e-types.log` |
| Route-fixture lint/format | ESLint and Prettier passed for the route fixture and restored build configuration; `target/settings-routes-lint.log` |
| Production frontend build | Passed with corrected bundle accounting and setup copy; `target/bundle-accounting-build-r3.log` |
| Windows browser checks | 42 passed in 1.2 minutes, exit 0, managed preview closed; `target/bundle-accounting-browser-r3.log`. Includes twelve Settings checks, startup/focus/auth, error recovery, and three held-font setup cases |
| Bundle resolver regressions | 29 passed across two files; `target/bundle-accounting-tests-r2.log`; missing dependencies, nested layouts/errors, server-load indices and required deferred families |
| Render inspection | Desktop and 390px mobile screenshots inspected; no clipping observed; `target/settings-desktop.png`, `target/settings-mobile.png`; no pinned visual baseline acceptance |
| Bundle gates | **Failed**, limits unchanged; `target/bundle-accounting-size-r3.log`. Corrected accounting: section 50.43KB/45KB; deferred controls 56.25KB/38KB; Alerts 29.12KB/29KB, Alert 45.83KB/45KB, detail deferred 34.45KB/33.9KB. Search initial JS 42.49KB/45KB and total 85.4KB/100KB pass. Earlier measurements omitted the root error node |

The first browser run left test-owned preview PID 93244 and test runner PID 60860.
Their commands and start times were verified before cleanup. PowerShell stop
failed; the follow-up termination was not executed because automatic approval
review hit its usage limit. Both processes subsequently exited; the original
runner recorded four passes after a 20.8-minute teardown. A second run lost the
server during that shutdown (`settings-browser-r2.log`, four infrastructure
failures). The final run uses an in-process Vite preview with explicit connection
closure and finishes cleanly. No crawler, Docker or unrelated process was targeted.

## Remaining acceptance work

1. Fix bundle growth without raising limits or hiding mount-time imports. The
   measured Settings section is about 50KB against 45KB, and its deferred controls
   about 56KB against 38KB. The graph shows shared menu/floating/focus primitives
   dominating deferred bytes. Alerts and detail also regressed in this composition.
2. Complete field-level search (localized label/help/keys/synonyms, lazy index,
   value/source context, Advanced reveal), extension registry and palette integration.
   Current navigation input is deliberately named **Find a section**.
3. Add truthful overview data from existing system/stats/network contracts:
   attention rows, changed counts and global restart review. The current overview
   explicitly says these measurements are unavailable. Do not infer healthy status.
4. Finish the per-screen config.toml view, guarded import and remaining existing
   backend General/Storage capabilities. Saved-settings export now has executed
   unit/browser coverage, but it is not the per-screen config view. Never invent
   unsupported settings.
5. Implement and test Network reachability, exact target address, authenticated
   keep/revert handling and recovery before enabling writes. Security/filter flows
   and a full first-run wizard stay separate.
6. Extend route fixtures to delayed-response races, dedicated confirmation
   cancellation and global restart review as those flows become available. The
   current atomic-save, validation, conflict, leave-guard and resync cases now pass;
   this does not establish acceptance of the remaining screen capabilities.
7. Complete both-locale, dark-theme and expanded-state rendered review, and retain
   pinned Linux visual and native assistive-technology/zoom gates. Managed Windows
   browser checks and the first desktop/mobile render inspection now pass.
8. Finish DUR-33 Settings user instructions and rendered screenshots after the
   feature and bundle gates pass. This developer ledger is not end-user guidance.

## Route-fixture expansion and bundle investigation (October 28)

The ten executed browser fixtures use only intercepted APIs. The six new cases
verify one Crawler/Transport batch with distinct revisions; a server refusal
revealing and focusing a collapsed Advanced field; a stale revision that cannot
save until the conflict is resolved; Keep editing and Discard navigation;
failed Save-on-leave retaining edits followed by a successful explicit Retry;
and resync updating restart requirements and environment locks while preserving
an unrelated edit. Every locator deadline is at least ten seconds. No test writes
to a crawler. The first expanded run had three incorrect fixture expectations
(numeric control role, conflict announcement and offline banner), corrected
against the existing component contracts; no production behavior was changed to
make those tests pass. Its seven passes and three failures remain recorded in
`target/settings-browser-r4.log`.

An attempted synchronous form-core chunk group was removed. Recursive dependency
capture made Search load unrelated modules, caught by the existing reachability
guard (`target/bundle-core-size.log`). Disabling that capture was rejected by
Rolldown because SvelteKit uses strict entry signatures
(`target/bundle-core-isolated-build.log`). Neither entry semantics nor budgets
were weakened. The save/merge/draft pipeline remains synchronous as required by
the earlier owner decision.

`target/settings-bundle-profile.json` records per-file compressed sizes and module
membership from the restored build. The three largest deferred chunks are shared
overlay/scroll utilities (~12.55KB), positioning (~11.86KB), and menu (~8.69KB).
Module membership alone does not prove unused exports survive tree shaking. This
profile is investigation evidence, not a completed bundle fix. No native Lodestar
process was observed at this validation checkpoint; C: had 116.2 GiB free.

## Draft change description

Settings routes currently show an unavailable page. This slice prepares a grouped
shell and generated core forms using the existing atomic-save engine, with mobile
return navigation, remembered Advanced fields, load recovery and localized labels.
General also downloads the existing backend's saved-settings export with optional
interface preferences, cancellation and error recovery.
Network changes remain read-only until address recovery is available. Source is
prepared; bundle and feature-completion gates remain open. No release acceptance
or native baseline modification is claimed.

## Export validation notes (October 28)

Twelve new unit cases cover the export request and component: backend bytes,
preference selection, unexpected content types, expired authentication, pre-abort,
late responses, duplicate clicks, cancellation focus, unmount, replacement cleanup,
superseded requests and safe retry. Two browser cases verify actual downloaded
file bytes/name and recovery from a successful HTML error response. The current
390px General screenshot includes the export controls; inspected without clipping.

The first unrestricted Settings test run hit existing ten-second setup-hook
timeouts in `SettingField.test.ts` and `SettingsForm.test.ts` while checks were
running: 294 passed, 47 skipped, two failed files. Preserve that failure in
`target/settings-export-suite.log`. A separate four-worker run passes all 341
Settings cases, and the full four-worker suite passes 2,337 tests. No timeout or
assertion was weakened. Bound subsequent local suites to four workers and avoid
overlapping heavy checks. Existing jsdom scroll/canvas limitations and the Windows
symlink skip remain explicit. No native crawler was running; C: was 117.7 GiB free
before these checks. No soak artifact or live application data was changed.

## Bundle accounting and cold-font follow-up (October 28)

The old route resolver silently omitted SvelteKit's root error node and could
misread nested layout/error arrays. The corrected resolver shares `routeNodes`,
includes server-load indices and rejects missing/ambiguous manifest entries. The
installed SvelteKit client loads error nodes during navigation. The existing
preload-list stripping now includes the root error node; lazy error recovery still
passes its browser cases. Required Settings controls cannot silently disappear
from the budget if a source key changes. Numeric budgets are unchanged.

An entry-aware Settings chunk group was removed after it increased the section
to 107.28KB and eliminated the separately measured controls entry. Preserve
`target/settings-entry-group-{build,size}.log`; the source uses the original
three chunk groups. The first resolver fixture run contained two parameter-shape
errors; corrected fixtures pass all 29 tests. These are test-authoring errors,
not production failures or evidence of a successful optimization.

The first browser run passed 38 of 39 cases and exposed setup CLS 0.00218.
Ten ordinary repeats then passed, but holding font responses reproduced the
failure: the introduction collapsed from two lines to one after the font arrived.
The new 1280/390/320px fixtures initially failed at desktop and 390px. Shortening
the introduction fixed desktop; the next run (41/42) exposed the same wrapping
issue in the token help at 390px. Both messages now use shorter English copy,
with Chinese counterparts, retaining the token format, startup-log location and
whole-line paste instructions. No artificial form height, hidden initial page,
font policy, CLS threshold or visual baseline changed.

The final 42-case browser run passes, including all three held-font fixtures.
The fixture confirms a real font request was held, releases it after fallback
paint, waits for the font and paint completion, and attaches shift sources. All
failed runs and geometry diagnostics remain under `target/bundle-auth-*` and
`target/bundle-accounting-browser*`. This does not establish native zoom, screen
reader or pinned Linux visual acceptance. The baseline's frozen r14 artifacts
remain unchanged; shared source fixes require fresh baseline packaging evidence.
