# X-steam-followups (Opus; runner brief 2026-11-20)

#969 ENR-steam merged as 0eca1090. Its fresh review said MERGE, with two Important items and two untested behaviours. They're all done here. UX-first: the user should never lose a working game list because of a bad download.

1. **Refuse a suspicious finished list.** A Steam app list that finishes downloading, but is empty or much smaller than the current one, must not replace the current list.
   - Keep the current list, and record a plain warning in the plugin's status: it names the sizes and says the list will be tried again later.
   - Pick the threshold yourself and record why in the decisions file. For example, below half the current size, with an absolute floor for small lists.
   - The first-ever list has no current list to compare against. Only an empty list is refused then.
   - Tests:
     - an empty list is refused;
     - a much smaller list is refused and the current list is kept;
     - a normal shrink is accepted;
     - the first list is accepted unless it's empty.
2. **Test the 60 s cancel hold.** Whatever the plugin does when a refresh is cancelled, holding its cursor for 60 s per the code and its decisions file, gets a test.
   - Use a deterministic time seam or the existing test clock.
   - No fixed sleeps. A condition wait needs a deadline of at least 10 s.
3. **Test the partial-list age limit.** A partial list older than its limit is discarded or restarted, as the code says, and one inside the limit resumes. Test both sides of the boundary.
4. **Don't build the `ext:` condition.** The runner adds it to `runner/dashboard.yaml`; that file's `conditions` are the runner's only. The condition covers three things: the owner's adult sign-off, a live re-read of Steam's terms, and Valve accepting the `x-webapi-key` header. If your decisions file mentions it, name it only.

## Owns
- `crates/sources-plugins/src/steam.rs`
- `crates/sources-plugins/tests/steam.rs`
- `crates/sources-plugins/fixtures/steam/**`
- `crates/sources-feeds/src/steam.rs` and `crates/sources-feeds/tests/steam.rs`, only if item 1's size check belongs in the parser
- `docs/decisions/2026-11-20-X-steam-followups.md`
- this slice's own `slices.yaml`/`dashboard.yaml` placement entries, not `conditions`

Stay out of the shared plugin lists: catalogue.rs, lib.rs, ops.rs, credential_spec.rs, manifests.rs and fuzz/*. ENR-tmdb's landing is changing them now.

## Not in scope
No live calls to Steam. Fuzz seeds stay as they are.

## Done
- CI green.
- Self-review per the steward skill.
- Send the runner "ready <pr> <full sha>".
