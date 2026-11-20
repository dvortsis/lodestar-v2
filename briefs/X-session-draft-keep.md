# X-session-draft-keep (runner brief 2026-11-20)

This is Track U step 4, from UI review L2 (docs/review/2026-11-13-ui-ux/). Read the slice's `runner/slices.yaml` entry, docs/decisions/2026-11-14-runner-ui-review-placement.md (L2), and §10.10 and §10.14 as #996 left them.

## The problem
When the session ends while a setting is unsaved, the user signs in again and loses the draft. The Save dialog doesn't say why the save failed. Separately, the leave dialog's "Save changes" button currently saves nothing.

## Scope
1. When a save fails because the session ended, the dialog says so in plain words. Wording follows §10.14, for the §10.0 reader. Then it keeps the draft and sends the user to sign in.
2. After sign-in returns to Settings, the draft is restored, and a one-line note says the changes were kept and aren't saved yet.
3. Fix the leave dialog's "Save changes" so it actually saves. If the save fails, it stays on the page with the error.
4. Add e2e specs and unit tests. No fixed sleeps; condition waits need deadlines of at least 10 s.

## Safety (runner's calls; /security-review before merge)
- **No secret value goes into the kept draft.** That covers passwords, API keys, tokens and any field the settings schema marks secret or write-only. Exclude them by the schema's flag, not by a name list. Test it with every secret field in the schema.
- **Keep the draft in `sessionStorage`, not `localStorage`.** Key it by account, and drop it if a different account signs in. Drop it on an explicit Sign out. Test both cases.
- Cap the stored draft size and ignore a draft that fails to parse. Wrap every storage access in try/catch, so a blocked storage only loses the convenience.
- The `next` return path (auth/next.ts) must stay same-origin, relative, and limited to the app's own routes. Keep its existing tests, and add one for the Settings return.

## Owns
- web/src/lib/features/settings/form/**
- web/src/lib/features/auth/session.svelte.ts
- web/src/lib/features/auth/next.ts
- their own e2e specs (new files `web/e2e/settings-draft*.test.ts`)
- en and zh messages under those folders
- docs/decisions/2026-11-20-X-session-draft-keep.md
- this slice's own yaml entries

Touch nothing else. If you need another file, ask the runner first.

## Checks
In web/:
- `npm run check`;
- `npx vitest run`;
- `npm run build`;
- `npx size-limit`;
- `PW_CHROMIUM_PATH=/opt/pw-browsers/chromium npx playwright test` for your specs.

## Done
CI green. Send the runner "ready <pr> <full sha>".
