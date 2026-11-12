# X-banner-live-region decisions

- **Banner gains `live` (default `true`).** `live={false}` renders no `role`, so a Banner inside a live region the caller owns doesn't nest a second one. The default is unchanged, so no existing caller moves.
- **Two callers changed in this PR (the runner approved taking them, 2026-11-12):** `StorageStep.svelte` (a `role="status"` div wraps the warning Banner; the wrapper stays always in the DOM so the change is announced) and `BlocklistsScreen.svelte:104` (a `role="alert"` div wraps the error Banner). Both Banners now take `live={false}`.
- **Audit:** a grep of every `<Banner` caller found no other one wrapped in `role` or `aria-live`. `FormAlert` relies on Banner's own role.
