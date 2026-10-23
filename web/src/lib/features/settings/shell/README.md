# Settings shell and screen registry

The route (`src/routes/settings/**`), the navigation, the two-pane and list → detail layout, and the registry every Settings screen joins (architecture §10.10.2, §10.10.8). Decisions: `docs/decisions/2026-10-23-S-settings-1.md`.

## Register a screen

Export a `screen` (or `screens`, or a default of either) from `src/lib/features/<feature>/screen.ts` or `src/lib/features/<feature>/<group>/screen.ts`. The shell finds it with `import.meta.glob`.

```ts
import { m } from '$lib/paraglide/messages.js';
import IconTrackers from '~icons/ph/radar';
import type { SettingsScreen } from '$lib/features/settings/shell/registry';

export const screen: SettingsScreen = {
	id: 'trackers', // /settings/trackers
	group: 'library', // preferences | crawling | library | connections | system
	title: () => m.trackers_screen_title(),
	sections: ['trackers'], // generated from these schema sections ...
	load: () => import('./TrackersScreen.svelte'), // ... or hand-built: loaded on open, renders its own <h1 tabindex="-1">
	icon: IconTrackers,
	search: () => [{ id: 'scrape', label: () => m.trackers_scrape() }],
	attention: async ({ system, signal }) => [] // Overview rows
};
```

- **`screen.ts` stays tiny.** It may import only `$lib/paraglide/messages.js`, `~icons/ph/*` and types (`registry.test.ts` fails on anything else), because the shell loads every one for every Settings page. Components load through `load`.
- **A screen with `sections` and no `load` is generated** (`screens/GeneratedScreen.svelte`). With `load`, the route renders the component and nothing else. `groups` limits a screen to some `x-group`s of its sections.
- **Where a field lives:** the first registered screen whose `sections` (and `groups`) include it. Two screens claiming one field is a dev error (`registry.doubleClaims`) and a failing test. Fields nobody claims form an interim generated screen per section at `/settings/<section>`, so a new server setting is never unreachable; it disappears when its slice claims the fields. Preferences-only screens apply instantly with Undo, everything else is staged.
- **Ids and order** are fixed in `registry.ts` (`SCREEN_ORDER`); an id not listed goes last in its group, by title. `/settings/backup` and `/settings/import` redirect to `/settings/general#backup`. Any other unknown id is the app's 404.
- **One id per screen**, matching `Connection.settings` and Insights' fix links.

## Pieces

| File                   | What                                                                                                                                                          |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `registry.ts`          | Types, discovery (`collect`, `registered`), order, `buildRegistry(index)` → `{ screens, get, fieldsFor, screenOf, doubleClaims }`, `fieldsFor(screen, index)` |
| `state.svelte.ts`      | The layout's shared state: the schema index, the registry resolved from it, `wide` (x-large)                                                                  |
| `SettingsShell.svelte` | The frame: two panes from x-large, list → detail below                                                                                                        |
| `SettingsNav.svelte`   | The grouped links with icon, label and badge                                                                                                                  |
| `ScreenHost.svelte`    | `/settings/<id>`: finds the screen, loads it, moves focus to its `<h1>`                                                                                       |
| `Overview.svelte`      | `/settings`                                                                                                                                                   |

Screens live in `../screens/`: `GeneratedScreen.svelte`, `SettingsGroups.svelte` (the groups and Advanced, for hand-built screens too: put it inside your own `<SettingsForm>`, call its `reveal(key)` from `onReveal`, and `await loadSettingMessages(sections)` before creating the form), Network's reachability panel and lockout dialog.

## Messages

Server messages (labels, help, options, keywords, groups) are split by section and load on demand (`loadSettingMessages` in `form/messages.ts`; `form/message-map.gen.ts` writes the units). After adding a key to `messages/en.json`, run `node --experimental-strip-types src/lib/features/settings/form/message-map.gen.ts`. The screens' own strings are `settings_ui_*`.
