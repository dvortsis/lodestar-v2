# X-cmp-handover-followups decisions

The findings from #679's review (`reviews/rv-679-243429d1.md` on runner-state), for a natively installed qBittorrent's hand-over to Integrations (`2026-11-11-X-cmp-native-handover.md`). These are the slice's decisions under §2.33. None loosens an owner decision or a security decision. The runner flags them to the owner (§2.19).

## A native install and the guided steps exclude each other (Important 1)
- **One lock.** The guided steps' lock (target, paste, plan, confirm, forget) is now the `Service`'s (`Service::changes`), and a native install takes it too.
  - The install holds it through `already_added`, the manager's `can_hand_over`, the install, its hand-over and any undo.
  - While a guided step runs, an install is `busy` before it checks or downloads anything. While an install runs, every guided step is `busy` and writes nothing.
  - So a guided confirm can no longer write the user's entry while a native hand-over overwrites it, or while a failed native undo deletes it.
- **The guided steps take the lock before they check for a companion Lodestar installed or is installing** (`handed_over`), no longer after. An install that finished just before is seen.
- **Restart, regenerate and remove don't take it.** Each needs a companion Lodestar installed, which every guided step refuses, under the lock.
- **Tested** over the real `Service` and `Guided`: with the guided lock held, an install is `busy`; with the install's held, every guided step is `busy` and nothing is written or handed over. No native install can run in the binary's tests (no catalogue pin), so the interleave is the lock itself, held by the test, not sleeps.

## The removal's busy guard (Important 3)
A removal over a journal that still holds an install or update is refused (`busy`) before anything is released. The test now asserts that nothing was released and the folder is still there, so removing the guard fails it. #679's decisions file is corrected.

## The crash residual is closed (#679's I2)
- **A marker:** a first install writes `<data>/companions/.handed-over-<id>`, durably (the file and its folder synced), before it tries the hand-over, and removes it once `configured` is saved.
  - It sits beside the lock, outside the companion's own folder, which a rollback deletes only if it holds nothing else.
  - A marker that can't be written fails the install before the hand-over (`handover`, `marker`).
- **Recovery** keeps a list of companions whose first install it undid while their marker was there. For one that's still installed (an update rolled back, or one rolled forward), it removes the marker, since its entry is right.
- **Once the sink is wired** (`Companions::wire_sink`), `Manager::release_pending` releases each, under its slot and only while nothing is installed, and removes the marker. One that fails keeps its marker for the next start, logged as `companions.install_failed`, step `release`.
- **A live undo** removes the marker after its release, and keeps it when the release fails, so the next start finishes it.
- **Tested:** a crash between the hand-over and `configured` is undone by recovery and released once the sink is there, once. A failed release keeps the marker until a later start. An install that finishes leaves none.

## Hardening
- **The runtime config is opened with `O_NOFOLLOW | O_NONBLOCK`** on Unix. A link is refused, and a FIFO is never waited on (it isn't a regular file, so it's refused). Tested with both.
- **`save_clients` reads the list and its revision together,** from one `Registry::section` read, which the registry makes under the section's lock. Before, the list came from the section's watch and the revision from a separate read. The read has no passwords, and the save sends none for your clients either way.
  - **Not separately testable:** the two reads agree in every test, so a stale-save test passes either way.
- **A hand-over is no longer abandoned at a deadline.** A save given up on could still land after an undo's release, because the blocking save isn't cancelled. It's one local settings write, bounded by the store, so it's awaited instead. `release` keeps its deadline: one that lands late only takes out an entry the companion's next restart hands over again.
- **Only the hand-over's refusals answer `409`:** `managed_client_in_use`, `sink_refused`, `not_wired` and `timed_out`. Any other failure of the hand-over or the release, such as an unreadable credential (`credentials`) or a refused sign-in (`unauthorized`), is an internal error (`500`).

## API messages (Important 2)
`crates/api/src/companions.rs` gives each new code its own sentence:
- `managed_client_in_use`: remove the "qBittorrent (Lodestar)" client in Integrations first, then install again;
- `already_added`: you added it yourself; forget it first;
- `sink_refused`: it couldn't be saved as your download client; nothing was changed; try again;
- `timed_out`: it took too long and was stopped; try again.

The OpenAPI document doesn't change (descriptions and shapes are as they were). No web file is edited; CMP-9a and CMP-9c show these sentences from the envelope.

## For the user docs (from #679's review)
A restart hands a natively installed qBittorrent over again, so a `lodestar-qbittorrent` entry you deleted in Integrations comes back. The guide should say so. That's for the docs slice, not this one.

## Tests
- `crates/companions/tests/manager.rs`:
  - the busy removal releases nothing;
  - the crash-and-release cases above;
  - a pending release is skipped once the companion is installed again before the sink is wired.
- **Mutation-checked:** removing each of these fails a test:
  - the install's share of the lock, and Guided's;
  - the marker;
  - recovery's pending list;
  - the pending release's installed check;
  - `O_NOFOLLOW`;
  - `O_NONBLOCK` (the FIFO test then times out);
  - the narrowed `409` mapping;
  - the busy removal's release check.
- `crates/companions/tests/supervised.rs` (Unix): a linked runtime config and a FIFO in its place are refused, the FIFO within 10 s.
- `crates/lodestar/src/companion_wiring_guided_tests.rs`: the shared lock, both ways.
- `crates/lodestar/src/companions.rs`: `credentials` and `unauthorized` aren't refusals.
- `crates/api/src/companions.rs`: each new code has its own sentence, and `managed_client_in_use` names Integrations.
