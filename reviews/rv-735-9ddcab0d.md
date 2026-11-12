VERDICT: CHANGES

# Review: PR #735, OUT-2 (Settings → Outputs), round 2, head 9ddcab0d7fa55c9e744d2b4c04a52a686d3b8866

Reviewer: fresh Opus reviewer, 2026-11-12 (UTC). Read only.

- **Scope of this review:** the delta `69aa129b..9ddcab0d`. It is one commit ("Match the export job by id; review fixes for OUT-2") with no merge of main. It touches 9 files, +282/−47.
- **Previous review:** `rv-735-69aa129b.md`.

## Checks re-run (own worktree, hard-linked node_modules)
- **`npm run check`:** passes, exit 0.
  - svelte-check: 0 errors. Its 3 warnings are the existing `@reference`/`@variant` ones in `DiscoverPage.svelte`.
  - eslint: 1 existing warning in `src/app.d.ts`.
  - prettier and check-bans: OK.
- **`npx vitest run src/lib/features/settings`:** 56 files, 709 tests passed. No skips.
- **`npm run build && npx size-limit`:** all entries pass.
  - Settings section: 44.97 kB against a 45 kB limit.
  - Outputs parts: 26.32 kB against 27 kB.
  - Outputs icon and purpose: 678 B against 750 B.
  - OutputsScreen first view: 34.01 kB against 45 kB.
- **Playwright:** `e2e/settings-outputs.routes.test.ts` and `e2e/settings-security.keyboard.test.ts`, 7 passed.
- **Mutation spot-checks:** each guard was removed in turn and the outputs tests re-run. The guard's source was restored after each run. Results are under Blocking 2.

## Claimed fixes, verified
1. **Size limit: verified.**
   - `'0.75 KB'` for 678 B (+10.6%).
   - The comment now says 26.32 kB with about 2.6% headroom, which is true.
   - Against origin/main, the only limit changes are "Settings section" lowered from '45.1 KB' to exactly '45 KB', plus the two new Outputs entries. No limit is raised.
2. **Job matching by id: partly verified.** The id logic is right. Two of its new guards are untested, and one race remains (Blocking 1 and 2).
   - **Server side:** ids come from `Jobs.next.fetch_add(1) + 1` (`crates/api/src/jobs.rs:468`), so they start at 1 and only grow while the server runs. `/jobs` sorts by id, highest first (`jobs.rs:511`). `lastExportId` returning 0 for an empty list is therefore safe.
   - **Kind and format:** `ourExport` requires `kind === 'export'` and `export.format === 'bitmagnet'`, so a purge, a CSV export or another format never matches. `lastExportId` counts every export, which is fine as a lower bound.
   - **Fallback:** it is reachable. When the mount read rejects, `seen` stays null, `ready` still resolves, and the poll compares the start time against the click. Once a found job finishes, `seen` becomes a number, so later presses match by id.
   - **Unmount:** `onDestroy` sets `live = false` and clears `timer`. The `ready.then` callback and both poll callbacks check `live`, so nothing schedules after unmount.
   - **Double press:** `clearTimeout(timer)` plus the `press` token end the older chain, including a request that is still in flight. The logic is right but untested (Blocking 2).
3. **Failed `/jobs` after a job is found: verified and tested.**
   - The screen keeps the last state and shows the `stale` line with a Jobs link, inside the status region.
   - The test "keeps the last progress when /jobs stops answering…" fails if `else if (job) stale = true` is removed.
4. **The curl example: verified.**
   - The `<pre>` has `tabindex="0"`, an aria-label "Example import command" (en and zh) and `FOCUS_RING`, which is token-based (`outline-primary`).
   - The e2e test asserts `tabindex`, and axe passes.
5. **The file name: verified.** The example uses `lodestar-export-YYYYMMDD-HHMMSS.bitmagnet.ndjson`.
6. **Banners: the error banner is fine, the warning banner is not (Blocking 3).** The cancelled and failed banners are `Banner kind="error"`, which renders `role="alert"`, so inserting them is announced. The truncated banner is `kind="warning"`, which renders `role="status"`.
7. **The adult switch: verified and tested.** It is `disabled={adultLock !== null || saving}`. The test "the switch waits while its change is saving" fails if `saving` is removed.
8. **New tests: verified.**
   - **Name refused on create:** a 409 with `field: 'name'` puts the message beside the field, the dialog closes, and the secret isn't in the DOM.
   - **Env-var lock:** the switch is disabled, the screen says "Set by LODESTAR_EXPORT_ADULT", and nothing is written.
9. **The decisions file: verified.** It records "share the key flow with Security" as a follow-up, and its id-matching text matches the code.

## Blocking

### 1. `Bitmagnet.svelte` `started()` and `onMount`: a click before the mount read answers can leave the export's job in the baseline, so it is never found
- **What happens:**
  - `onMount` sends `GET /jobs`.
  - If the user presses Download before that answer arrives, `started()` waits on `ready` (correct so far).
  - If the server runs the export's `jobs.start()` before it handles that read, the baseline answer already contains the new job. This is possible because the browser can queue the read behind other first-view requests while the navigation request jumps ahead, or because the read is slow on a busy NAS.
  - `seen = lastExportId(r.jobs)` then equals the new job's id. `lastExportId` counts the job even while `export` is still null during `resolve`, before `set_export`.
  - Every poll looks for `id > seen`. Nothing matches, and after 30 s the screen says it "couldn't find this export's progress" for an export that is running.
- **Reproduced:** a component test held the mount's `/jobs` reply, clicked, added the job (id 5, running) to `jobs`, then released the reply. Progress never appeared within 10 s. The decisions file's claim "Polls wait for that first read, so a quick click still matches by id" holds only if the server read that list before it created the job, and nothing guarantees that.
- **Fix (small):**
  - Record whether `ready` has settled.
  - If the press comes before it settles, don't trust that answer as this press's baseline. Use the clock fallback (`afterId = null`) for that press, and still set `seen` for later presses.
  - Add the test described above, and update the decisions bullet to match.

### 2. Two pieces of new behaviour have no test (standards §5: "a missing test for new behaviour")
Mutation results, each run against `outputs.test.ts` and `logic.test.ts` (30 tests):

| Guard removed | Caught? |
|---|---|
| Id match replaced by the time match (`logic.ts`) | **yes**, 3 tests fail |
| Mount baseline ignored (`seen` never set) | **yes**, 2 tests fail |
| Stale line (`else if (job) stale = true`) | **yes** |
| `saving` in `disabled` | **yes** |
| **Polls wait for the first read** (`ready.then(...)` replaced by a direct `setTimeout`) | **no**, 0 PR tests fail |
| **Per-press tokens** (`mine !== press` removed from `poll` and from its reply handlers) | **no**, 0 PR tests fail |
| `seen` raised after a found job finishes | **no** (see Important 1) |

- The brief asks for a double-click test. There is none.
- The in-flight case the token exists for (press 2 while press 1's `/jobs` request is pending) needs a held reply. A test should:
  - hold one `/jobs` reply;
  - press twice;
  - release the reply;
  - assert that the poll count over a few seconds matches one chain. Wait on a condition with a deadline of at least 10 s, not on a sleep.
- The "polls wait for the first read" test is the Blocking 1 test: hold the mount read, press, release.

### 3. `Bitmagnet.svelte`: the truncated warning has moved outside the status region and may not be announced
- **Why it may not be announced:** `Banner kind="warning"` renders `role="status"`. A polite live region inserted already holding its text is unreliably announced: NVDA and Chrome usually say nothing, because the region must exist before its content changes.
- **What a screen-reader user hears:** the always-mounted progress region announces "Done: 42 torrents in the file." The one message that says the file is incomplete, the reason this banner exists (the file has no trailer), can be missed.
- **What's fine:** the cancelled and failed banners are `role="alert"`, which is announced on insertion.
- **Fix:** put the truncation text inside the always-mounted status region, as a plain icon-and-text line (the round-1 nit's suggestion), and keep the visible warning styling there. Alternatively, keep the Banner visible and add an `sr-only` copy of the sentence inside the region. Either way, a component test should assert that the truncation text is inside `outputs-bitmagnet-progress`.

## Important

### 1. A second press can still pick up the previous press's job
- **When it happens:** after a press whose job was lost, or went stale, or was still running, `seen` is not raised (it is raised only when polling ends with a finished job). On the next press, a poll can match the earlier job, because its id is above `seen` and it's a bitmagnet export.
- **Why the window is real:** the new job is listed with `export: null` until its count search (`resolve`) finishes, so it doesn't match yet.
- **The bad case:** if the earlier job has finished by then, `keepPolling` stops at once and shows the earlier export's "Done: N" for the new download. This is the round-1 bug in another form.
- **Fix:** in `started()`, before `job = null`, add `if (job) seen = Math.max(seen ?? 0, job.id);`. A test should press, let the job finish, press again, and check that the old "Done" is not shown.

### 2. Server restart between opening the screen and pressing Download
- Job ids reset to 1, because `Jobs` is in memory, so the new job's id is at or below `seen` and is never found. The result is the same "couldn't find" message.
- This is rare. Making the 30 s give-up fall back to the clock match once would cover it, as would re-reading the baseline when the screen becomes visible. Recording it in the decisions file is enough.

## Nits
- **Stale line:** "…It may still be running: check Jobs." is followed by a link that also says "Jobs", so a screen reader reads "check Jobs. Jobs". Consider "It may still be running." followed by the link.
- **The `<pre>`'s `aria-label`:** `pre` maps to the generic role, where ARIA 1.2 prohibits naming. axe passes today, but `role="region"` (or `group`) with the label would make the name valid and reachable.
- **Test name:** "never shows an export that finished before the click as the new one" covers the server-clock-ahead direction. The browser-ahead direction is covered in the component only indirectly: the stale test's job has `started_at: 0`. A one-line comment there would make the coverage explicit.

## Security (unchanged from round 1)
- No findings. This delta doesn't touch the key flows, Reveal, the password dialog, CSP, `app.html` or any Rust.
- The secret is still shown once through Security's `revealSecret`, and is never logged, stored or put in a URL. The e2e test still asserts it is gone from the DOM, both storages and `location.href`.
- The copied Torznab URL has no key.
- The outputs code has no `console`, `localStorage` or `sessionStorage` use.

## Quality
- **Tokens and spacing:** the hex and arbitrary-value bans pass. The delta adds no new spacing steps; `p-3` and `gap-1` are on the §10.2 ramp. The focus ring is the token-based `FOCUS_RING`.
- **Messages:** en and zh have the same key set, and both add `bitmagnet_stale` and `bitmagnet_example`.
- **Tests:** all waits are `waitFor` with a 10 s timeout, and the e2e test uses condition waits only. No fixed sleeps, no `.skip` or `.only`.
- **Hygiene:** no model identifiers and no 12-digit numbers. Dates are UTC.
