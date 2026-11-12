# X-banner-live-region decisions

- **Banner gains `live` (default `true`).** `live={false}` renders no `role`, so a Banner inside a live region the caller owns doesn't nest a second one. The default is unchanged, so no existing caller moves.
- **Callers still to change (outside this slice's owned paths; the runner assigns them):**
  - `web/src/lib/features/wizard/StorageStep.svelte`: a `role="status"` div wraps `<Banner kind="warning">`. The wrapper must stay always in the DOM so the change is announced; set `live={false}` on the Banner.
  - `web/src/lib/features/blocklists/BlocklistsScreen.svelte:104`: a `role="alert"` div wraps `<Banner kind="error">`; set `live={false}` on the Banner.
- **Audit:** a grep of every `<Banner` caller found no other one wrapped in `role` or `aria-live`. `FormAlert` relies on Banner's own role.
