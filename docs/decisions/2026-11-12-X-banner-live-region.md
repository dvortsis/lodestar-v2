# Banner: opt-out of its live role

Banner gains `live` (default `true`). `live={false}` renders no `role`, so a Banner placed inside a live region the caller owns does not nest a second one. Default behaviour is unchanged, so no existing caller moves.

Audit: `StorageStep.svelte` wraps a `role="status"` div around `<Banner kind="warning">`. That wrapper must stay always in the DOM so the change is announced, so the fix there is `live={false}` on its Banner. `StorageStep.svelte` is outside this slice's owned paths and is left for the runner to assign. Other callers were not audited beyond that one.
