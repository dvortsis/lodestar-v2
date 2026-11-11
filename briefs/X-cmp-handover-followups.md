# X-cmp-handover-followups (Opus, core, concurrency, /security-review): the findings from #679

Assigned by the runner on 2026-11-11 to the session that built #679 (X-cmp-native-handover, merged as 520ad918). The review is `reviews/rv-679-243429d1.md`.

## Do
1. **The guided vs native race** (Important 1). A native qBittorrent install, and its hand-over and undo, and the guided confirm must be mutually exclusive, through one shared lock or slot. Check `already_added` and `can_hand_over` under that lock. Add a test that runs a guided confirm and a native install at once and shows the user's entry is never overwritten or deleted. Use a condition-driven interleave, not sleeps.
2. **The removal's busy guard** (Important 3). Assert `releases == 0` in the busy-removal test (`tests/manager.rs` ~1371), mutation-check `manager.rs:1604`, and correct #679's decisions file.
3. **I2.** Add a journalled "handed over" marker, released once the sink is wired, so recovery can release an orphaned managed entry after a crash. Test it with a crash between the hand-over and configured.
4. **Hardening:**
   - Open the runtime config with `O_NOFOLLOW|O_NONBLOCK` on Unix (manager.rs ~2106), and refuse anything that isn't a regular file.
   - `save_clients` takes one consistent revision+list read.
   - A timed-out hand-over cancels its save, or makes sure the save can't land after undo's release.
   - Non-refusal errors (`credentials`, `unauthorized`) map to their proper status codes, not 409.
5. **API messages** (Important 2). Give `managed_client_in_use`, `already_added`, `sink_refused` and `timed_out` plain-language messages in `crates/api/src/companions.rs` (~173). `managed_client_in_use` tells the user to remove the entry in Integrations first. Regenerate openapi.json and schema.d.ts if anything changes, never by hand. CMP-9a (#681) and CMP-9c (#665) are open on web; don't edit web/.

## Owns
- `crates/companions/src/manager.rs`, `crates/companions/tests/manager.rs`
- `crates/lodestar/src/companion_wiring.rs`, `crates/lodestar/src/companions.rs`
- `crates/api/src/companions.rs` (+ its tests)
- `docs/decisions/2026-11-11-X-cmp-native-handover.md` (correction)
- `docs/decisions/2026-11-11-X-cmp-handover-followups.md`

## Bar
- fmt, clippy `-D warnings` (workspace), and tests.
- Mutation-check each new guard. No fixed sleeps (condition waits ≥ 10 s).
- Branch `slice/X-cmp-handover-followups`, PR `X-cmp-handover-followups: <title>`, steward skill.
- Reply "ready" with the head when CI is green, Windows included.
