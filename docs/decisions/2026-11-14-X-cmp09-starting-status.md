# X-cmp09-starting-status decisions

A follow-up to X-cmp09-fix (#887) and X-cmp09-fix-2 (#893). Until now, a request the loopback gate refused read as a plain "unreachable". It now has its own error, words and log event. This file says what the code now does (`docs/security/v1/README.md` §5). The calls marked "runner's call" were the runner's. The ones marked "this PR's call" came from this PR's review, and the runner flags them to the owner (`docs/owner-decisions.md` §2.19).

## Not ready has its own error

- **One sentence everywhere** (`lodestar_integrations::net::NOT_READY`): "Lodestar's own copy of this companion isn't ready yet. If you stopped or removed it, restart Lodestar to use your own." `NetError::NotReady` displays it.
- **Download clients:** `ClientError::NotReady`, in place of `Unreachable`.
  - A client test or a send that hits it answers the code `companion_not_ready` with that sentence.
  - The test's message no longer adds a full stop to a text that already ends with one.
- **Companions' adapters:** `AdapterError::NotReady`, in place of `Unreachable`.
  - Serve's guided steps report it as the refusal code `companion_not_ready`. The configure step's own code for it is `not_ready`.
  - It comes from the gate, so only a request to an entered URL meets it: the companions' own client for loopback and container targets opts out of the gate.
  - **Deliberately unchanged (runner's call):** `CompanionHttp`'s own refusal for a managed port that isn't ready stays `Unreachable`. Detection at a loopback target reads unreachable as "not here", and `NotReady` there would turn that into an error. Showing "starting" in the guided steps is a follow-up, X-companion-starting-steps.
  - Detection at an entered URL now fails with it, where it used to read as "not detected".
  - Health reads it as unreachable, as before. So does the check for whether anything accepted the connection (`listening`), which can't meet it today.
- **Sources (runner's call: a new `HttpError::NotReady(String)`):**
  - It carries the same sentence.
  - It doesn't back the host off, just as `EgressDown` doesn't.
  - The egress paces it with the kill switch's delays, by a streak of its own. The delay grows from `DOWN_DELAY_MIN` to `DOWN_DELAY_MAX` with each refusal in a row, and any answered request through that plugin's egress ends the streak.
  - Unlike the kill switch's pacing, it isn't cut short when the companion becomes ready. A streamed request's pacing is cut short by half its timeout, as before.
  - It counts as a policy refusal in the egress counters.
  - The torrent cache, which never sends to a companion, treats it as any other failure.
- **The download-clients source** (this PR's call):
  - It reads `HttpError::NotReady` as `ClientError::NotReady`, and the poll tries again a poll later.
  - An export refused this way isn't counted as a try, since nothing was sent. No other export is asked for in that poll; new torrents are found at once with the list's claims, as when the export budget runs out.
- **The Prowlarr source** (this PR's call): it's a request never sent, so it isn't counted as a hit towards the daily Usenet limit.
- **Notifications (runner's call):** left as a transient failure, with a comment saying why it can't happen there (see "What the gate's refusals say").
- **The web app** words `companion_not_ready` apart from `companion_unreachable` (en and zh). The download-client test shows the server's sentence, as it does for every code.

## What the gate's refusals say (this PR's call)

- **Every refusal stays a refusal.** Only the error it gives changed, so that "not ready" means a companion Lodestar runs that isn't ready:
  - **A name that doesn't resolve now,** on a port the gate refuses, fails as the failed lookup it is (`NetError::Resolve`), not as not ready. It's still refused, and isn't logged.
  - **A client whose policy never allows loopback** (notifications, for one) is refused as blocked (`NetError::Blocked`), not as not ready, and isn't logged. Its policy would refuse the connection anyway.
- So `NotReady` can't reach a notification.

## A log event, once per port

- **`integrations.companion_not_ready`** (Warn, field `port`; runner's call). It's logged the first time the gate refuses a port as not ready in a process, and not again for that port.
- **Help text (en):** "Lodestar held back a request to one of its companions because the companion isn't ready yet. It clears once the companion starts; if you stopped or removed it, restart Lodestar." zh has a plain translation.

## A hand-over marker reserves the port too

- **Serve reserves a companion's port** before recovery and any start if the companion has a folder here (as before) **or** a hand-over marker. Anything at the marker's path counts, and so does a path that can't be looked at.
- **Runner's call:** a read-only `Manager::marked(id)` was approved. The reservation runs before any manager is opened, so it's a free function over the data folder instead (`lodestar_companions::manager::handed_over`).
  - It uses the manager's marker path.
  - It's broader than the manager's own check, which counts only a file there.
- **The port stays refused after a restart** too, while that companion's folder (even an empty one) or its marker is left behind. It opens once that companion is started and found ready, or at the first restart after what was left is removed.

## No catalogue, no gate (runner's call: a comment, not a gate)

- If the built-in catalogue can't be read, companions are off for the run and no gate is set. It can't happen: the catalogue is compiled in, and its tests load it. A comment at `companions::start` says so.

## Tests

- **`crates/integrations/tests/loopback_gate.rs`:**
  - a refusal reads as the not-ready sentence, never as unreachable;
  - after many refusals of one port, the event is logged once, with `port=` and the port;
  - a name that doesn't resolve fails as a lookup;
  - a client whose policy never allows loopback is refused as blocked;
  - neither of those two is logged: they go to a second refused port that nothing else touches, and no event names it.
- **`crates/sources`:**
  - `HttpError::NotReady` doesn't back the host off;
  - the egress paces it by its own streak, `pace_by` cuts it short, and an answered request ends the streak.
- **`crates/sources-plugins`:**
  - Prowlarr doesn't count it as a hit.
  - The download-clients source, on a qBittorrent whose exports are all refused as not ready (`tests/clients.rs`, its own `Fetch`): each poll asks for one export, the first torrent's; both torrents are found at once with the list's claims; and past `MAX_EXPORT_TRIES` polls that export is still asked for. The test fails with either the mapping or the export rule taken out.
- **`crates/companions` and serve's wiring:**
  - `NetError::NotReady` becomes `AdapterError::NotReady`, coded `companion_not_ready`, not `companion_unreachable`;
  - detection at an entered URL reports it, and health and `listening` read it as unreachable.
- **`crates/api`:** a client test that hits it gives `companion_not_ready` and the sentence with one full stop.
- **`crates/lodestar/src/companions.rs`:** a hand-over marker with no folder reserves the port; a companion with neither is left alone.
- **web:** `companion_not_ready` has its own words.
