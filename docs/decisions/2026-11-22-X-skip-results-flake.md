# X-skip-results-flake decisions

Slice `X-skip-results-flake`, from one failure of `web/e2e/search-toolbar.keyboard.test.ts` ("Skip to results shows on focus and lands on the list's active row") on #1080 at ea5b2cd0. The runner chose the product fix (option A) under `docs/owner-decisions.md` §2.26 (UX): a user's key press must not be lost.

- **The cause was a product race in the shell, not in search.** The window keydown listener that runs the shortcuts isn't on the page from the start. `Chrome.svelte` attaches it in `whenEngaged`, after the first interaction, once a `Promise.all` of 7 lazy imports resolves: keys, shell commands, shortcuts boot, appearance boot, status, StatusPill and ConnectionView. Before this change, only the key that woke the shell was replayed (`Shell.svelte`'s `first` listener removed itself after one key). Every key pressed after it and before the attach was dropped. In the test, the first Tab wakes the shell; Tab, Enter on Skip and j then follow at machine speed. On a loaded runner the chain is still loading when j arrives, so row 2 stays "inactive". A user on a slow device or connection who presses Tab, Enter and j quickly after load lost the j the same way. `data-keys-ready` (ResultsPanel) doesn't cover this: it means the results and detail commands are registered, not that the shell's listener is attached.
- **Evidence (local repro, not committed).**
  - 30 runs at `--workers=8` passed: it doesn't show plainly.
  - Holding back the chunks that contain `results.previous` reproduced the CI failure exactly (`Received: inactive` at the j step). Those chunks are the results commands plus the shortcuts and keys chunks that the chrome's chain waits on.
  - Adding a wait on `[data-keys-ready]` before j still failed. Focus was on row 1 and the keys were registered, but j was dropped.
  - Waiting until the chain finished (a local check only) made j work. A log at the attach showed j had been pressed but there was no listener.
- **The fix: every press until the attach is kept and replayed, never twice.**
  - `Shell.svelte` keeps each keydown from the first one until the chrome reports its listener attached (`onkeys`), up to 32. It leaves out modifier-only presses and typing (an editable target, the search box included): the text is already there.
  - It also takes Ctrl/Cmd+K for the palette on any press in that window, not only the first. Before, a later Ctrl/Cmd+K went to the browser's address bar.
  - `Chrome.svelte` hands the kept presses to `keys.handle` in order, right after `attach`. It hands them only to the registry: the page has already seen the real events, so handing it new ones would act twice.
  - It skips a press:
    - whose target is gone;
    - that is default-prevented (the list's own Enter and arrows);
    - that the browser acted on natively: Tab, and Enter or Space on a link, button or `summary`;
    - that is older than 5 s at the attach (`STALE_KEY_MS`), so a stalled load never fires a late burst of shortcuts.
  - The Enter on Skip is a link press, so it isn't run again as a shortcut (it would open the detail drawer).
  - The filters live in the lazy chrome chunk, not the shell, to keep the first load in budget. The shell keeps only the 32 cap and the typing check it already had.
- **The flaky test needs no wait.** It's unchanged. With the shortcuts chunk held 1.5 s in a local copy (j about 2.5 to 3.1 s old at the attach, under the 5 s cap), it passed 20 of 20 at `--workers=8`. Before the fix it failed every time under a hold.
- **Tests.** The new `keys pressed before the key listener attaches` group in `search-toolbar.keyboard.test.ts` holds the chunk that starts the shortcuts (the one exporting `startShortcuts`, which only the chrome's chain loads) until the test lets it go. The waits are on conditions, never sleeps. It checks four things:
  - j right after Skip still moves to row 2, and the Enter on Skip doesn't open the drawer. This one fails without the fix.
  - Enter on a button (Columns) acts once: the chooser stays open.
  - Text typed into the search box appears once.
  - The very first key (`/`) still finds its shortcut.
- **Size (`npm run size:ci`).** The search route's initial JS is 44.97 kB against its 45 kB limit (44.95 kB without the change, measured the same way). A first version that filtered in the shell measured 45.02 kB, so the filters moved to the chrome. No entry is red.
