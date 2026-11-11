# X-cmp-handover-followups decisions

The findings from #679's review (`reviews/rv-679-243429d1.md` on runner-state), for a natively installed qBittorrent's hand-over to Integrations (`2026-11-11-X-cmp-native-handover.md`). These are the slice's decisions under §2.33. None loosens an owner decision or a security decision. The runner flags them to the owner (§2.19).

## A native install and the guided steps exclude each other (Important 1)
- **One lock.** The guided steps' lock (target, paste, plan, confirm, forget) is now the `Service`'s (`Service::changes`), and a native install of qBittorrent takes it too. Another companion's install doesn't: only qBittorrent is handed over here (self-review I1).
- **Held by the install's own task,** which a dropped request (a closed tab) doesn't stop. So the lock is never let go while the install, its hand-over or its undo still runs (self-review B1). Tested with the install parked once it holds the lock: the request is dropped, and a guided step is still `busy` until the install ends.
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
- **Once the sink is wired** (`Companions::wire_sink`), `Manager::release_pending` releases each, under the guided lock (waited for), the companion's slot, and only while nothing is installed, and removes the marker. It's a tracked task, stopped at shutdown.
- **A guided confirmation clears it first:** under the guided lock, right before its own hand-over, it removes the marker and any pending entry for the companion (`Manager::clear_handover`). The entry is yours from then on, so no later start releases it (self-review B2). Tested: a confirmation over a leftover marker removes it.
- **One skipped because its slot was busy** keeps its marker until the next start (self-review P2). Meanwhile the leftover entry makes a first install answer `managed_client_in_use`. One that fails keeps its marker for the next start, logged as `companions.install_failed`, step `release`.
- **A live undo** removes the marker after its release, and keeps it when the release fails, so the next start finishes it.
- **Tested:** a crash between the hand-over and `configured` is undone by recovery and released once the sink is there, once. A failed release keeps the marker until a later start. An install that finishes leaves none.

## Hardening
- **The runtime config is opened with `O_NOFOLLOW | O_NONBLOCK`** on Unix. A link at the file itself is refused, and a FIFO is never waited on (it isn't a regular file, so it's refused). A linked folder above it isn't refused: the install folder is Lodestar's own, staged and checked by CMP-2 (self-review P7). Tested with both, in `manager.rs`' unit tests.
- **The marker is written with `O_NOFOLLOW`** on Unix, so a link planted at its path is never followed and truncated (P4).
- **`save_clients` reads the list and its revision together,** from one `Registry::section` read, which the registry makes under the section's lock. Before, the list came from the section's watch and the revision from a separate read. The read has no passwords, and the save sends none for your clients either way.
  - **Not separately testable:** the two reads agree in every test, so a stale-save test passes either way.
- **A hand-over gets a generous outer bound** (`HANDOVER_DEADLINE`, 15 min) instead of `LIVE_DEADLINE`, against a store that never answers (self-review I4).
  - Past it, the save may still land, so an install's undo doesn't release it, and its marker stays: the next start releases what it left, once it can't land any more.
  - A restart's or a regenerate's is retried by the next restart.
  - `release` keeps `LIVE_DEADLINE`: one that lands late only takes out an entry the companion's next restart hands over again.
  - **Not tested:** the timeout itself, which would need a fifteen-minute wait.
- **Only the hand-over's refusals answer `409`:** `managed_client_in_use`, `sink_refused`, `not_wired` and `timed_out`. Any other failure of the hand-over or the release, such as an unreadable credential (`credentials`) or a refused sign-in (`unauthorized`), is an internal error (`500`).

- **Left (self-review P1):** a guided step's request that's dropped while its sink save is in flight lets the lock go before the save lands. A native install's check can then miss that entry by one save. The guided steps aren't moved into tasks of their own here.

## API messages (Important 2)
`crates/api/src/companions.rs` gives each new code its own sentence:
- `managed_client_in_use`: the download client Lodestar manages is already in Integrations, from a qBittorrent you added before; remove it there first, then install again. It names the entry by its role, since you may have renamed it (self-review P5);
- `already_added`: you added it yourself; forget it first;
- `sink_refused`: Lodestar couldn't update its download client in Integrations; check Integrations, then try again. It's worded for a removal's release too;
- `timed_out`: it took too long and Lodestar stopped waiting; check the companion, then try again. A late save may still land, so it doesn't say nothing changed.

The OpenAPI document doesn't change (descriptions and shapes are as they were). No web file is edited; CMP-9a and CMP-9c show these sentences from the envelope.

## For the user docs (from #679's review)
A restart hands a natively installed qBittorrent over again, so a `lodestar-qbittorrent` entry you deleted in Integrations comes back. The guide should say so. That's for the docs slice, not this one.

## Tests
- `crates/companions/tests/manager.rs`:
  - the busy removal releases nothing;
  - the crash-and-release cases above;
  - a pending release is skipped once the companion is installed again before the sink is wired.
- **Mutation-checked:** removing each of these fails a test:
  - the install's share of the lock, Guided's, and the lock being held by the install's own task;
  - the guided confirmation's clearing;
  - the live undo's unmark;
  - the marker write's error;
  - the marker;
  - recovery's pending list;
  - the pending release's installed check;
  - `O_NOFOLLOW`;
  - `O_NONBLOCK` (the FIFO test then times out);
  - the narrowed `409` mapping;
  - the busy removal's release check.
- `crates/companions/src/manager.rs`' unit tests (Unix): a linked runtime config and a FIFO in its place are refused, the FIFO within 10 s. They moved out of `tests/supervised.rs`, which isn't this slice's (scope).
- `crates/companions/tests/manager.rs` also: a live undo removes the marker after its release and keeps it after a failed one; a marker that can't be written fails the install before any hand-over.
- `crates/lodestar/src/companion_wiring_guided_tests.rs`:
  - the shared lock, both ways;
  - another companion's install isn't held up by it;
  - a dropped install request keeps the guided steps out;
  - a guided confirmation clears a leftover marker.
- `crates/lodestar/src/companions.rs`: `credentials` and `unauthorized` aren't refusals.
- `crates/api/src/companions.rs`: each new code has its own sentence, and `managed_client_in_use` names Integrations.
