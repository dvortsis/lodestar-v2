# X-alert-search-headroom decisions

The Alert route, the lazy search results, the Alerts route and the results toolbar sat within bytes of their size limits (§3, §10.1), so any change to a shared chunk turned them red. This slice gets real bytes out of them. No limit was raised.

- **Links build density with `$lib/shell/density`, not `$lib/shell/nav`.** `withDensity` was a five-line helper in `shell/nav.ts`. Importing it brought along the destination table, the More items and their eight labels: about 670 B gzip in two chunks. Alerts, Alert and the results panel paid for it. The helper now lives alone in `shell/density.ts`. `nav.ts` re-exports it for the top bar, and Rolldown follows that re-export to `density.ts`, so even an import through `nav` no longer loads the table. Callers changed: `features/alerts/api.ts` and `features/search/results/links.ts`. `features/settings/shell/SettingsShell.svelte`, outside this slice's paths, still imports it from `nav`. Through the re-export it no longer loads the table either, so every Settings first view also fell by about 537 B with no Settings file changed. Settings limits are not this slice's to lower.
- **The Alerts list registers its New alert key after mount, from `commands.ts`, as the alert page already does.** The list page imported `$lib/keys` statically, so the key registry (2.7 KB) was part of its first view. It is now loaded with `import('./commands')` in `onMount`. The command and its palette entry are the same; they show up a moment after the page mounts, as they do on the alert page and in Detail. The component test now waits for the registration (10 s deadline), and a new test covers leaving the page before `commands.ts` loads. **This is partly a gate reclassification:** the registry is still fetched on every Alerts visit, just after mount, not before first paint. `.size-limit.mjs` counts mount-time imports as first view, so its header rule now names key registration as the exception, as the Alert page and Detail already practise. The cut to the Alerts first view is real bytes before paint; the fetch itself remains. The Alerts route's first view stops at `commands.ts`, as the Alert route's already did. A new entry, "Alerts key commands (on demand)", gates `commands.ts` and the registry, measured beyond both pages' first views, so no chunk shared with the dialogs drops out of every gate: 3.5 KB (measured + 10%).
- **Follow-up, not fixed here:** with the scope pushed when `commands.ts` resolves, a dialog opened in that gap pushes its scope first, so `alerts` lands above it. The Alert page already behaves this way. The window is a few milliseconds after mount.
- **Save search's dialog loads through a one-hop loader, `features/search/toolbar/save-dialog.ts`.** A dynamic import's preload list sits in the importing chunk. For the alerts' dialog, which brings Bits UI's overlay code, that list is about 30 files and 290 B gzip, and the toolbar loads with every search's results. The list now sits in the loader, which is fetched on the first Save search press. That first press costs one extra small request before the dialog and its dependencies are fetched in parallel. First paint and the toolbar's own load don't change. This is the pattern Settings' `body.ts` already uses. A new entry, "Save search loader (on demand)", gates the loader at 0.55 KB (0.49 KB measured + 10%). The Alerts list's own import of the dialog is unchanged: that route now has 0.57 KB of headroom without it.
- **A guard keeps these views apart from shared churn.** `.size-limit.mjs` now fails, before measuring, if a view's files contain certain modules. The rules: Alerts, Alert or the toolbar must not contain `src/lib/shell/nav.ts` (the destination table; not on the search first load), `src/lib/keys/**` or `src/lib/features/settings/**`. Search results must not contain the nav table or Settings code. Search results does carry the key registry, because it registers j/k with its first rows. The guard reads the recorded chunk graph through `modulesIn()` in `scripts/first-load.mjs`. It was checked by making the results panel use `DESTINATIONS` again: the size step failed with `Search results (lazy): src/lib/shell/nav.ts`. A comment by the Alerts entries states the rules.
- **Not done: one manual chunk for the code these routes share** (the brief's Do 2b). The Alert route is 42 files. Gzipped one after another, they come to 7.8 KB less than gzipped one by one, so merging co-loaded modules would be the biggest win. A Rolldown group whose test is "reached by both the Alert page and the results panel" was tried. It doesn't work as things stand:
  - With `includeDependenciesRecursively` on (the default), the group pulls in its first-load dependencies (the Paraglide runtime, `locale.svelte.ts`, `ui/classes.ts`). The search route's first load would then import the group chunk, and `checkFirstLoad` rightly fails.
  - Turning that option off needs `preserveEntrySignatures: 'allow-extension'`. SvelteKit forces `'strict'` and overrides the setting.
  - The module graph also counts every message as "reached" through the `messages.js` barrel.

  So the isolation here comes from imports, not from chunk groups. Revisit this if Kit allows the setting, or with Rolldown's `tags`.
- **Limits** (`web/.size-limit.mjs`). The brief allows either: lower the limit to measured + 10% of the cut, or keep the old limit when the cut is the headroom.
  - **Alerts route:** lowered to 26.3 KB. Measured + 10% of the cut would leave less than the 0.5 KB headroom target, so the limit keeps that 0.5 KB.
  - **Results toolbar:** back to 4.6 KB. This reverses X-build-stable-name's SHA-noise raise and still leaves 308 B of headroom.
  - **Alert route and Search results:** limits kept, so the cut is their headroom.
- **Measured** (bytes, gzipped; `npx size-limit --json`; local builds without `GITHUB_SHA`; two runs of the same build, identical):

  | Entry | `main` (7650aeb) | This branch | Limit before → after | Headroom |
  |---|---|---|---|---|
  | Alert route | 45,974 | 45,401 | 46 KB → 46 KB | 599 B |
  | Search results (lazy) | 43,272 | 42,732 | 43.3 KB → 43.3 KB | 568 B |
  | Alerts route | 29,165 | 25,732 | 29.2 KB → 26.3 KB | 568 B |
  | Results toolbar (lazy) | 4,598 | 4,292 | 4.65 KB → 4.6 KB | 308 B |
  | Alerts key commands (on demand) | (new) | 3,171 | 3.5 KB | |
  | Save search loader (on demand) | (new) | 493 | 0.55 KB | |
  | Settings overview | 11,028 | 10,491 | 45 KB | |
  | Settings section | 43,639 | 43,102 | 45 KB | |
  | Settings first views (each hand-built screen) | | −532 to −539 | 45 KB | |
  | Search cards (lazy) | 8,223 | 8,201 | 8.3 KB | |
  | Search route JS (initial) | 43,063 | 43,062 | 45 KB | |

  Every other entry moved by 8 B or less.
- **No change to what the page shows or to first paint.** The search first load is unchanged in files and size, so no request was added before first paint. Lighthouse was not run in this session. The visual baselines are untouched: no markup or CSS changed.
