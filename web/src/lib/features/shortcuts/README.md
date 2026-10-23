# features/shortcuts

The keyboard preferences (§10.16, §10.10.8). Decisions: `docs/decisions/2026-10-23-S-keys.md`.

| File                                 | Role                                                                                                                                                                                                                                             |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `boot.ts`                            | `startShortcuts()`: the shell calls it on the first interaction. Registers `help.shortcuts` and the palette toggle, applies the cached then the saved mapping, refetches on the `settings` event. Stays small; imports nothing heavy statically. |
| `toggle.ts`                          | The palette's "Turn on/off single-key shortcuts" (loaded on use).                                                                                                                                                                                |
| `catalog.ts`                         | The eight remappable actions (the server's `DEFAULT_SHORTCUTS`), in §10.16's order.                                                                                                                                                              |
| `capture.ts`                         | What a captured key may be, and clash logic. Mirrors `check_shortcuts` in `preferences.rs`.                                                                                                                                                      |
| `ShortcutSheet.svelte`               | The `?` sheet (lazy; `boot.ts` mounts it on first use).                                                                                                                                                                                          |
| `ShortcutsField.svelte`              | The `x-widget: shortcuts` editor. Controlled: `value`, `onchange`, `errors?`, `disabled?`. Saves nothing.                                                                                                                                        |
| `KeyboardScreen.svelte`, `screen.ts` | Settings → Keyboard. `screen.ts` is the shape the settings route is expected to find.                                                                                                                                                            |

Adding a remappable action means adding it to the server's list first, then to `catalog.ts`, `defaults.ts` and the messages; `keys.test.ts` checks the two lists agree.
