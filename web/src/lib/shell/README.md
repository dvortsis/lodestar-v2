# lib/shell

App chrome (§10.3, §10.13, §10.19). `routes/+layout.svelte` renders `Shell.svelte`; `routes/+error.svelte` is a stub that loads `ErrorView.svelte` (which renders `StateScreen.svelte`) only when something failed, because kit loads the error node with every route. Decisions and the size budget: `docs/decisions/2026-10-22-W9.md`. Strings: `lib/features/shell/messages`.

| File                                      | Role                                                                                                  |
| ----------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| `Shell.svelte`                            | Skip link, top bar, `<main id="main">`, toaster, palette. Owns the keydown listener and the 401 hook. |
| `TopBar.svelte`, `nav.ts`                 | Wordmark, five destinations, status pill, palette button, overflow menu. Collapses below 1024 px.     |
| `StatusPill.svelte`, `status*.ts`         | Crawler and rebuild state from `/events`; numbers through `$lib/format`; "—" when unknown.            |
| `CommandPalette.svelte`, `commands.ts`    | ⌘K / Ctrl+K palette over the key registry; the shell's own "Go to …" commands.                        |
| `StateScreen.svelte`, `StateScene.svelte` | Full-page states with the §10.19 illustration (lazy, decorative). `ConnectionView.svelte` picks them. |
| `chrome.ts`                               | Which routes render without chrome (`/login`, `/setup`, `/welcome`).                                  |

## Hand-off points

- **M-auth:** merged. `Chrome.svelte` calls `installAuthHandlers()` from `features/auth/session.svelte` (it owns W5's single `onLoginRequired` slot: a 401 goes to `/login?next=…` or `/setup`) and uninstalls it with the chrome. `chrome.ts` lists the screens that render bare.
- **S-keys:** the shortcut sheet reads `keys.list()` and registers `shortcuts.open` (`?`). See `lib/keys/README.md`.
- **Appearance:** the shell has no theme or density switch. Density is carried in nav links; the scene variant follows `color-scheme` (`theme-mode.ts`).

## Keep the first load small

Only the frame and the top bar are in the search route's first load (§10.1). Anything heavier (a dialog, a menu, the event stream, the toaster) loads on first use or when the browser is idle. The top bar draws its icons with `Glyph.svelte` instead of `~icons` for the same reason. Check with `npx size-limit` after any change here.
