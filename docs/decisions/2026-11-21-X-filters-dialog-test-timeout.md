# X-filters-dialog-test-timeout decisions (#PR)

Runner brief, 2026-11-21Z. Test-only.

## What was found
- **The flake:** "titles the dialog with the rule's name and says what it checks" (`web/src/lib/features/filters/screens/screen.test.ts`, "rule and pack names (I5)") timed out at 5,011 ms once during a full `vitest run` and passed when re-run.
- **The cause:** it's the first test in the file to open a change dialog. The screen loads the dialog on demand (`import('../flows-door')`, then `load()`, which pulls in `ChangeDialog` and Bits UI's overlay code), so that test paid the cold transform and import inside its 5 s budget. Every later test reuses the module cache.

## The fix
- **A file-level `beforeAll` imports the screen's on-demand modules once:** the dialogs (`flows-door` → `load()`), `Queue.svelte`, `Packs.svelte` and `packs/PacksScreen.svelte`. Those four are every `import()` the screen makes. No single test pays a cold import any more.
- **The hook has a 15 s timeout,** the repo's ≥10 s rule. A hook timeout fails the file with a clear message instead of failing whichever test happened to run first.
- **No per-test or describe timeout,** because the measurements below leave 13× headroom under vitest's 5 s default. No fixed sleeps.

## Measurements
Each run executed the file 10 times on a 4-CPU container, with one full `vitest run --pool=threads` started 8 s earlier as load. Durations come from vitest's JSON reporter.

| | main | this branch |
|---|---|---|
| Slowest test in each run | 1,408–1,632 ms | 350–381 ms |
| The dialog-title test | 1,408–1,632 ms (the slowest in every run) | 164–224 ms |
| Failures | 0 | 0 |

- **After the fix, the slowest tests are the danger-style flows,** at about 0.37 s. Their time is their own work, not an import.
- **A first attempt at the "before" numbers let load runs pile up** (load average 39, because killing `npx` left vitest behind). It was discarded. Under that overload, 24 tests across the file hit 5 s, so no timeout value would make such a host pass. It isn't the CI case.
