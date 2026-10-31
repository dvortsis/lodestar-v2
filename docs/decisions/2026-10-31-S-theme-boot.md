# S-theme-boot decisions

- **The start-up deferral recorded in `2026-10-30-S-appearance.md` is closed.** The shell now calls `startAppearance()` (`features/appearance/boot.ts`) next to `startShortcuts()`, on the same first-interaction path. It fetches `preferences`, applies it with `applyLook` and `applyTheme`, and so refreshes the `lodestar.theme` cache. A new browser or device follows the account (§10.18).
- **No flash, no fight.** Nothing is touched when the page already shows the saved values. A failed fetch is silent. A choice made on Appearance while the fetch is in flight wins: the boot compares `themeGeneration()` (new, read-only) across the await. A server-named custom theme missing from this browser falls back to Riverstone through `applyTheme`, with no toast.
- **Follows changes made elsewhere** with the same mechanism as the shortcuts boot (`onceLive`, the `settings` topic, resync).
- **No pre-paint, limit or baseline changes.** The boot loads on demand with the shell's other start-up modules.
