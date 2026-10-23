# lib/keys

Keyboard map (§10.16): the key registry, the command list behind the palette, and the data the shortcut sheet needs. Decisions: `docs/decisions/2026-10-22-W9.md`.

## Register a command

From the feature's own file, in an `$effect` or `onMount` so it unregisters with the screen:

```ts
import { keys } from '$lib/keys';

$effect(() =>
	keys.register({
		id: 'magnet.copy', // ids in DEFAULT_KEYS get their key from §10.16
		title: () => m.search_copy_magnet(),
		section: () => m.search_section(),
		scope: 'search', // omit for global
		run: copyMagnet
	})
);
```

| Field          | Meaning                                                                                                  |
| -------------- | -------------------------------------------------------------------------------------------------------- |
| `id`           | Stable id. A second registration of the same id throws.                                                  |
| `title`        | Palette and sheet text. A function so a language change shows.                                           |
| `keys`         | `['mod+k']`, `['/']`, `['Escape']`. `mod` is ⌘ on macOS, Ctrl elsewhere. Defaults to `DEFAULT_KEYS[id]`. |
| `scope`        | `global` (default) or a name you activate with `keys.pushScope(name)` (returns the pop function).        |
| `allowInInput` | Fires while typing, but only for a Ctrl/Alt/Meta combo or Escape. Bare characters never fire in a field. |
| `when`         | Extra condition; false hides the command from the palette and mutes its keys.                            |
| `palette`      | `false` for shortcut-only commands.                                                                      |
| `keywords`     | Extra words the palette matches.                                                                         |

## Rules

- **Conflicts:** two commands on one combo in one scope are listed by `keys.conflicts()` (and logged); neither fires. Different scopes never conflict. The most recently pushed scope wins; `pushScope(name, { exclusive: true })` mutes everything below it (a modal surface).
- **Typing is never hijacked:** text inputs, selects, contenteditable and `textbox`/`searchbox`/`combobox` roles only receive `allowInInput` commands with a modifier or Escape. Bare characters are also ignored inside `dialog` and `menu` roles, and while composing (IME).
- **Settings (WCAG 2.1.4):** `keys.setSingleKeys(false)` turns off every bare-character shortcut; `keys.remap(id, ['y'])` rebinds, `remap(id, null)` restores. Persisting and replaying them at startup belongs to the Settings slice.

## For the shortcut sheet (S-keys)

`keys.list()` returns `{ id, title, section, scope, keys, keywords, palette, active }[]` (reactive), and `formatCombo('mod+k')` from `$lib/keys/display` gives `⌘K` or `Ctrl+K`. `keys.singleKeys` says whether bare characters are on. The shell registers no `?` command: S-keys registers `shortcuts.open`.

## Files

- `registry.svelte.ts`: `createKeys`, the `keys` singleton, `isEditable`.
- `combo.ts`: spec and event → canonical combo. `display.ts`: combo → text. `defaults.ts`: the §10.16 map. `filter.ts`: palette matching.
