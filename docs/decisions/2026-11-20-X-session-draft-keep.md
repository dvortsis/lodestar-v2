# X-session-draft-keep decisions

Track U step 4, UI review L2 (`docs/review/2026-11-13-ui-ux/03-not-done.md`): a Settings save that finds the session ended now says so, keeps the draft across the sign-in, and restores it on return. The leave dialog's "Save changes" now saves. Architecture §10.10.5 ("unsaved work is never lost silently") and §10.14 (what happened, then what to do).

## What changed

- **A save that finds the session ended asks a question, not "Leave without saving?"**
  - The 401 handler records why the app is going to sign in: `expired`, `notice` (the Legal Notice is due) or `setup`.
  - The Settings leave guard reads that reason. A navigation to `/login` then gets its own decision, `signin`.
  - The question is "You've been signed out" (or "Accept the Legal Notice to save"). It says how many changes aren't saved and whether this browser tab can keep them.
  - **Sign in** keeps the draft and goes on. **Keep editing**, or Esc, stays, and the next save that finds the session ended asks again: the shell re-arms its once-only redirect.
  - The question never promises what it can't keep. When storage is blocked, or the only changes are secrets, it says the changes will be lost. When secrets are among them, it says "Passwords, keys, secret links and download-client entries aren't kept" and must be entered again. The list fits what the flag drops: the whole `integrations.clients` list, and the secret webhook, Discord and ntfy URLs (the runner's review).
- **Restored on return, with one line.** Before going to sign in, the form writes its draft and a "kept" marker naming the form and the screen's path.
  - When the sign-in returns to that screen, the draft is restored at once, without the usual Restore / Discard offer. A note says "Your changes are back after you signed in, but they aren't saved yet." When some no longer apply (now locked, or gone), the note says how many were left out, as the usual offer would.
  - The marker is honoured once, only on the screen it names, and only within 24 h (the runner's call). Its own form consumes it even when the draft no longer applies. A second expiry starts a new marker from the current draft. A different screen, a bad marker or an old one falls back to the usual draft offer.
  - The note goes away on a save or a discard.
- **The leave dialog's "Save changes" saves.** If a save, Undo or Retry is already in flight, it waits for that (`settled()`), then saves what's left, instead of answering "busy" and closing having saved nothing. Any failure stays on the page, where the bar, the summary or the sign-in question says why. The review's case (the session ended) is now the signed-out question.

## Safety (the runner's brief; `/security-review` before merge)

- **No secret reaches a draft, by the schema's flags.**
  - The form already skipped `writeOnly` fields. It now also skips any field whose schema holds a `writeOnly` subschema anywhere (`FieldSpec.holdsSecret`).
  - That covers `integrations.clients`, whose items' `password` key is the server's `secret_keys`. Before, a password typed into a client could have been drafted.
  - A schema that can't be serialized counts as holding a secret.
  - The restore re-checks the same flag, so a crafted stored draft can't bring a secret back.
  - `draft-keep.test.ts` walks every field of B2's golden `GET /settings` and finds each one that is or holds a `writeOnly` subschema: the seven server secrets today, plus any new one automatically. It then checks that none is drafted or restored.
- **`sessionStorage` only, scoped to this tab.** The draft, the marker and the sign-in reason are never in `localStorage`.
  - **Sign out drops every draft and the marker** (`logout()`).
  - So does **every way the app learns the instance needs its first-run setup again**: a 401 with `setup_required`, `GET /auth` on a reload or on the sign-in page, or a refused sign-in. The instance was re-claimed, so it may have a new owner. `setSession('setup_required')` drops the drafts and records a `setup` block, however the status was learned. The leave guard then lets the navigation to `/setup` go and discards what was staged.
  - **Account = the one admin; no server identity field** (the runner's call). Lodestar has one admin password and no usernames, and no API exposes an account or session identity. So "keyed by account" means the draft is kept only across a session expiry and dropped on every deliberate end. Sign out and a re-claimed instance are the only ways another owner reaches this tab. No `crates/api` or schema change.
  - **After a Sign out or a re-claim, drafts are blocked until a real sign-in or setup.** `logout()` records a `signedout` state, and a re-claim a `setup` state. Every draft write then only removes, so a pending debounce or a form torn down later can't write one back.
    - **What lifts the block: a live session answering after it** (the runner's call, a "block epoch").
      - Each Sign out or re-claim bumps an in-memory epoch.
      - A request that merely succeeds can't lift the block by itself, since it may answer one sent before the block. Instead it starts one fresh `GET /auth`, sent at the current epoch. Getting to the sign-in page also runs that check.
      - The block lifts only if that check reports a live, set-up session and no newer block was set while it was in flight. So a late answer to a request from before the Sign out (a preview within its debounce) never lifts it.
      - A sign-in from another tab lifts it, on this tab's reload or its next request. Without this, the tab stayed blocked for good: it never offered a draft again, and a later expiry was treated like a Sign out, so the guard went to sign in without asking and lost the changes silently.
      - The sign-in and setup forms' success lifts it directly (`signedIn()`).
      - A success also ends a pending expiry or Legal Notice. Choosing to stay re-arms the redirect but keeps the block.
      - A probe's answer lifts the block only while the block is still in place. If the form already lifted it and a real expiry followed, a late probe answer leaves the expiry alone.
    - **Known limit (accepted under "no server identity"):** suppose the instance is re-claimed and set up again elsewhere before this tab makes any request. The tab then sees only `login_required`, so it records `expired` and keeps the old admin's draft for the next sign-in. That's accepted because it's the same browser tab and no API exposes who the admin is. The runner logged `Cache-Control: no-store` on `GET /auth` as a separate server follow-up.
    - The 401 handler doesn't overwrite either state with `expired`, and the leave guard lets the navigation to `/login` go without keeping anything.
    - **Two lines outside this slice's files, approved by the runner:** `LoginForm.svelte` and `SetupForm.svelte` call `signedIn()` on success instead of `setSession('signed_in')`. No running slice owns them.
- **Bounded and fail-safe.** Drafts stay capped at 64 KiB. A marker over 4 KiB, unparseable or of the wrong shape reads as none and is removed. Every storage access is wrapped, so blocked or full storage loses only the convenience.
- **`next` is unchanged.** It stays same-origin, relative and never an auth or API path. New tests cover the Settings return (path, query and field anchor) and reject `/settings/../login`, `/settings/../api/…` and off-site forms.

## How the pieces share state

- The shell's session (`auth/session.svelte.ts`) and the form's `settings/form/draft-scope.ts` share only `sessionStorage` keys and one window event (`lodestar:signin-rearm`), never an import.
  - Why: an import either way made the bundler split a shared chunk into every Settings first view. That cost about 1 to 3 kB against a 45 KB route budget with 0.24 kB of headroom.
  - `session.test.ts` checks that the key names match.
- **The 401 path is fully lazy** (the runner's call under §2.26). The first view keeps only the leave decision and a one-line `import('./signed-out')` hook. `signed-out.ts` asks the question, writes the draft and the marker, and re-arms the redirect. It takes the edit chunk's `ask`, so the dialog code isn't duplicated or split. If loading it fails (offline), the guard falls back to the plain leave question.
- **Restore-on-return is on demand too.** `kept-restore.ts` and `KeptNote.svelte` load only when a kept marker is stored, checked with one read in `Engine.offer()`. They aren't in the edit chunk.
- **What stays in the always-loaded code** is what safety needs: the secret flag for list fields, the draft-write block after a Sign out, the sign-in state read, and `settled()` with the in-flight tracking.
- `lazy-imports.test.ts` checks that the edit chunk never imports `signed-out.ts`, `kept-restore.ts` or `KeptNote.svelte` statically.

## Size budgets (the runner's call under §2.26)

One change outside this slice's files, approved by the runner: `web/.size-limit.mjs` lists `signed-out.ts`, `kept-restore.ts` and `KeptNote.svelte` in `settingsOnDemand`. The screen entries then stop at them, and "Settings controls (on demand)" gates them.

Measured with those lines (kB). The runner confirmed these exact limits as its call under §2.26: each over-budget entry rises by its measured overage, rounded up to the next 0.1 kB, and no more. No other limit moves.

| Entry                                        | main         | here         | Δ            | Limit before | Limit now   |
| -------------------------------------------- | ------------ | ------------ | ------------ | ------------ | ----------- |
| Settings section (own JS + CSS)              | 44.76        | 45.45        | +0.69        | 45           | 45.5        |
| InsightsScreen first view                    | 44.58        | 45.28        | +0.70        | 45           | 45.3        |
| LanguageScreen first view                    | 44.38        | 45.08        | +0.70        | 45           | 45.1        |
| NotificationsScreen first view               | 44.79        | 45.49        | +0.70        | 45.2         | 45.5        |
| Wizard steps (on demand)                     | 53.65        | 54.32        | +0.67        | 53.7         | 54.4        |
| Settings edit chunk                          | 11.12        | 11.22        | +0.10        | 11.3         | 11.3 (fits) |
| Settings controls (on demand)                | 33.64        | 35.73        | +2.09        | 38           | 38 (fits)   |
| LoggingScreen, IntegrationsScreen first view | 43.97, 43.95 | 44.68, 44.65 | +0.71, +0.70 | 45           | 45 (fit)    |

- **Why the first views grow:** what stays always loaded is safety. That is the secret flag for list fields, the tracking of a save in flight (`settled()`), the draft-write block after a Sign out, the sign-in state read, the leave decision and the one-line hook. The 401 path itself, the question and the restore all load on demand.
- **Why the wizard grows:** `wizard/FieldsStep.svelte` and `wizard/UpdatesStep.svelte` import `SettingsForm` statically (through `settings/form/embed`, with `leaveGuard={false}`). So they pay for the same first-view engine code, though they never use the leave path.
- **Planned give-back:** the runner is queuing `X-settings-leave-guard-split`. It splits the leave guard out of `SettingsForm`, so the wizard's embed stops paying for it and the 0.67 kB comes back.
