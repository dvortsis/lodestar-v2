# features/detail

The torrent detail (§10.5): a drawer on Search, a full page at `/t/{hash}`. Decisions: `docs/decisions/2026-10-23-S-detail.md`.

## For the search page

```ts
import { openDetail, closeDetail, prefetchDetail } from '$lib/features/detail/open';
import { setCommandTarget } from '$lib/features/detail/commands';

openDetail(hash, { hit, returnFocus: () => rowEl, replace: false }); // pushState('/t/' + hash + query)
closeDetail(); // history.back() if openDetail pushed the entry, else replaceState('/search' + query)
prefetchDetail(hash); // after 150 ms idle; a newer call cancels it. Call on hover, focus, j/k.
setCommandTarget(hash | null); // the focused row; `c` and `o` act on it when no drawer is open
```

Mount the host in the trailing grid track, through `import.meta.glob` so the chunk stays out of the search route:

```svelte
const loaders = import.meta.glob('$lib/features/detail/DetailHost.svelte');
```

`DetailHost` shows while `page.state.detail` is set. Its parent must be positioned (`relative`) for the overlay modes. From xx-large (≥ 1366) it sits in flow; at x-large it is a 560 px overlay at the trailing edge; at large a 480 px overlay; below large a full screen with a Back button.

`detail.copy_magnet` (`c`) and `detail.open_magnet` (`o`) are registered here, once, by `registerDetailCommands()`. Don't register them again.

Every detail open revalidates `GET /torrents/{hash}` before showing either cached detail or the associated search hit. Content-settings events, event resynchronization and session transitions clear the caches and cancel requests. Mounted details revalidate on visibility changes; sign-out clears them without another request. Request and visibility generations prevent late responses from restoring cleared metadata. Hash-based magnet actions also revalidate. Prefetch retains the 50-entry, one-minute cache but never authorizes display.

## Files

- `open.ts`, `commands.ts`, `magnet.ts`, `cache.ts`: the API above, the keys, copy/open magnet, the hit and detail caches.
- `visibility.ts`, `visibility-events.ts`: `watchDetailVisibility()` and, loaded on demand after mount, the settings subscription it installs (keeps the event client out of the first view).
- `detail.css` (shared), `host.css` (drawer host only), `page.css` (`/t` only), `raw.css` (the raw viewer only): keep each rule in the file of the one entry that uses it.
- `model.svelte.ts`: one torrent's data (detail, hit, errors, latest-wins).
- `DetailHost.svelte`, `DetailView.svelte`, `DetailActions.svelte`, `ErrorState.svelte`, `Contents.svelte`, `ManualMagnet.svelte`.
- `summary/`, `composition/`, `parsed/`, `files/` (`build.ts`, `tree.worker.ts`, `load.ts`, `FileTree.svelte`), `raw/RawViewer.svelte` (not mounted in the drawer yet: no endpoint serves an info dict).
