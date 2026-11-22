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
  - `ApiConfigure::hands_over` can turn it off for a companion whose hand-over saves nothing. It's yes by default, which fails safe: a marker it didn't need only makes the next start release nothing. The binary doesn't override it, so every first install is marked, Prowlarr's included, and a hand-over added later can't forget to mark (#692's review, B1).
  - It sits beside the lock, outside the companion's own folder, which a rollback deletes only if it holds nothing else.
  - A marker that can't be written fails the install before the hand-over (`handover`, `marker`).
- **Recovery** keeps a list of companions whose first install it undid while their marker was there. For one that's still installed (an update rolled back, or one rolled forward), it removes the marker, since its entry is right.
- **Once the sink is wired** (`Companions::wire_sink`), `Manager::release_pending` releases each, under the guided lock (waited for), the companion's slot, and only while nothing is installed, and removes the marker. It's a tracked task, stopped at shutdown.
- **A guided confirmation clears it first:** under the guided lock, right before its own hand-over, it removes the marker and any pending entry for the companion (`Manager::clear_handover`). The entry is yours from then on, so no later start releases it (self-review B2). Tested: a confirmation over a leftover marker removes it.
  - **Before the hand-over, not after (#692's review suggested after):** cleared after, a crash between the hand-over's save and the clear would leave a marker, and the next start would release the entry you had just added. Cleared before, a hand-over that then fails leaves the crashed install's entry, which the confirmation's retry overwrites with yours. So clearing first loses nothing.
- **The confirmation's saves are held under the lock** (#692's review, Important 1): the clear, the hand-over, the origin row and the watch's update (`Wired::added`, N2) run in a task that owns the guided lock. The task has no bound of its own: a store that never answers keeps the guided steps and a qBittorrent install `busy` until shutdown, as a late release does (N4). A dropped request (a closed tab) no longer lets the lock go while the hand-over's save is in flight, so a native install can't pass `can_hand_over` before that save lands and then overwrite or, on its undo, delete your entry. Tested: with the save parked and the request dropped, an install is `busy`; once the save lands, the watch has it and an install is `already_added`.
- **A release that passes its deadline is waited for** (#692's review, Important 3). It's logged `timed_out` at the deadline, then waited for under the companion's slot and the caller's guided lock (serve's pending release, or an install's undo), because it may still save. A guided confirmation can't save an entry in the meantime that the late release would then take out. It keeps its marker, and the next start releases whatever is left. A store that never answers keeps qBittorrent's guided steps and install `busy` until shutdown, which stops the task. Logged once, at the deadline: the undo and the pending release don't log it again. Tested with a release held past a short deadline: it isn't dropped, the slot stays held, and the marker stays until a later start.
- **One skipped because its slot was busy** keeps its marker until the next start (self-review P2). Meanwhile the leftover entry makes a first install answer `managed_client_in_use`. One that fails keeps its marker for the next start, logged as `companions.install_failed`, step `release`.
- **A live undo** removes the marker after its release, and keeps it when the release fails, so the next start finishes it.
- **Tested:** a crash between the hand-over and `configured` is undone by recovery and released once the sink is there, once. A failed release keeps the marker until a later start. An install that finishes leaves none.

## Hardening
- **The runtime config is opened with `O_NOFOLLOW | O_NONBLOCK`** on Unix. A link at the file itself is refused, and a FIFO is never waited on (it isn't a regular file, so it's refused). On Windows, it's opened with `FILE_FLAG_OPEN_REPARSE_POINT` and refused if it's a reparse point (a link or junction), as on Unix (#692's review). The Windows branch isn't separately tested: creating a link there needs privileges CI doesn't have. A linked folder above it isn't refused: the install folder is Lodestar's own, staged and checked by CMP-2 (self-review P7). Tested with both, in `manager.rs`' unit tests.
- **The marker is written with `O_NOFOLLOW`** on Unix, so a link planted at its path is never followed and truncated (P4).
- **`save_clients` reads the list and its revision together,** from one `Registry::section` read, which the registry makes under the section's lock. Before, the list came from the section's watch and the revision from a separate read. The read has no passwords, and the save sends none for your clients either way.
  - **Not separately testable:** the two reads agree in every test, so a stale-save test passes either way.
- **A hand-over gets a generous outer bound:** three of the live deadline (15 min in shipped builds, against a store that never answers; self-review I4). It's derived from the live deadline, which tests shorten (#692's review, Important 2).
  - **One past it is logged there, then waited for** (#692's review, N1), as a late release is. It may still save, and the install holds the slot and the guided lock meanwhile, so a guided confirmation can't save your entry for the late native save to overwrite. Once it has settled it answers `timed_out`; the install's undo then releases what it saved and removes the marker, as for any other failed hand-over. A restart's or a regenerate's holds the slot the same way.
  - **Unbounded, as documented:** a store that never answers keeps the companion's slot (and, for an install, the guided lock) until shutdown.
  - `release` keeps the live deadline, and one past it is waited for (above).
  - **Tested:** a hand-over held past the shortened bound is logged once there, isn't dropped, and the install waits; once it settles, the install fails `timed_out`, its undo releases it once, and no marker is left.
- **Only the hand-over's refusals answer `409`:** `managed_client_in_use`, `sink_refused`, `not_wired` and `timed_out`. Any other failure of the hand-over or the release, such as an unreadable credential (`credentials`) or a refused sign-in (`unauthorized`), is an internal error (`500`).

- **Self-review P1 is closed** for the confirmation, whose hand-over is the only guided save a native install's check reads (above). The other guided steps write only their own rows, which a native install doesn't read.

## API messages (Important 2)
`crates/api/src/companions.rs` gives each new code its own sentence:
- `managed_client_in_use`: the download client Lodestar manages is already set up in Integrations; remove it there first, then install again. It names the entry by its role, since you may have renamed it (self-review P5), and not where it came from, which may be a crashed install as well as a qBittorrent you added (#692's review). The API's messages are English only; there's no zh string to change;
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
  - a hand-over past its bound is waited for, then undone; a release past its deadline is waited for and keeps its marker; a companion that isn't handed over writes no marker.
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
  - the busy removal's release check;
  - #692's round: the confirmation's tail owning the lock, the hand-over bound derived from the live deadline, the hung hand-over keeping its marker, the late release being waited for, its marker kept, and the marker only for a companion that's handed over; in the next round, the binary's `hands_over` (a test that it says yes for every companion), the late hand-over being waited for, and its timeout logged once.
- `crates/companions/src/manager.rs`' unit tests (Unix): a linked runtime config and a FIFO in its place are refused, the FIFO within 10 s. They moved out of `tests/supervised.rs`, which isn't this slice's (scope).
- `crates/companions/tests/manager.rs` also: a live undo removes the marker after its release and keeps it after a failed one; a marker that can't be written fails the install before any hand-over.
- `crates/lodestar/src/companion_wiring_guided_tests.rs`:
  - the shared lock, both ways;
  - another companion's install isn't held up by it;
  - a dropped install request keeps the guided steps out;
  - a guided confirmation clears a leftover marker.
  - a dropped confirmation keeps the lock until its save lands.
- `crates/lodestar/src/companions.rs`: `credentials` and `unauthorized` aren't refusals.
- `crates/api/src/companions.rs`: each new code has its own sentence, and `managed_client_in_use` names Integrations.
