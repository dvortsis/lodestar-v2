# X-cmp09-starting-status decisions

A follow-up to X-cmp09-fix (#887) and X-cmp09-fix-2 (#893). Until now, a request the loopback gate refused read as a plain "unreachable". It now has its own error, words and log event. This file says what the code now does (`docs/security/v1/README.md` §5). Each call below was the runner's (`docs/owner-decisions.md` §2.19).

## Not ready has its own error
- **One sentence everywhere** (`lodestar_integrations::net::NOT_READY`): "Lodestar's own copy of this companion isn't ready yet. If you stopped or removed it, restart Lodestar to use your own." `NetError::NotReady` displays it.
- **Download clients:** `ClientError::NotReady`, in place of `Unreachable`. A client test that hits it answers the code `companion_not_ready` with that sentence.
- **Companions' adapters:** `AdapterError::NotReady`, in place of `Unreachable`. Serve's guided steps report it as the refusal code `companion_not_ready`. The configure step's own code for it is `not_ready`.
- **Sources (runner's call: a new `HttpError::NotReady(String)`):**
  - It carries the same sentence.
  - It doesn't back the host off, just as `EgressDown` doesn't.
  - The egress paces it as it paces a kill switch refusal, with a streak of its own: a delay that grows from `DOWN_DELAY_MIN` to `DOWN_DELAY_MAX` with each refusal in a row. An answered request ends the streak. A streamed request's pacing is cut short by half its timeout, as before.
  - It counts as a policy refusal in the egress counters.
  - The torrent cache, which never sends to a companion, treats it as any other failure.
- **Notifications (runner's call):** left as a transient failure, with a comment saying why it can't happen there. A notification's policy never allows loopback, and loopback is all the gate refuses.
- **The web app** words `companion_not_ready` apart from `companion_unreachable` (en and zh). The download-client test shows the server's sentence, as it does for every code.

## A log event, once per port
- **`integrations.companion_not_ready`** (Warn, field `port`; runner's call). It's logged the first time the gate refuses a port in a process, not again for that port.
- **Help text (en):** "Lodestar held back a request to one of its companions because the companion isn't ready yet. It clears once the companion starts; if you stopped or removed it, restart Lodestar." zh has a plain translation.

## A hand-over marker reserves the port too
- **Serve reserves a companion's port** before recovery and any start if the companion has a folder here (as before) **or** a hand-over marker. Anything at the marker's path counts, and so does a path that can't be looked at.
- **Runner's call:** a read-only `Manager::marked(id)` was approved. The reservation runs before any manager is opened, so it's a free function over the data folder instead (`lodestar_companions::manager::handed_over`). The manager's own `marked` uses the same path.
- **The port stays refused after a restart** too, while that companion's folder (even an empty one) or its marker is left behind. It opens once that companion is started and found ready, or at the first restart after what was left is removed.

## No catalogue, no gate (runner's call: a comment, not a gate)
- If the built-in catalogue can't be read, companions are off for the run. Nothing is started, so no port is Lodestar's and no gate is set. A companion you run yourself stays reachable. A comment at `companions::start` says so.

## Tests
- **`crates/integrations/tests/loopback_gate.rs`:** a refusal reads as the not-ready sentence, never as unreachable. After many refusals of one port, the event is logged once, with the port.
- **`crates/sources`:** `HttpError::NotReady` doesn't back the host off. The egress paces it by its own streak, and `pace_by` cuts it short.
- **`crates/companions` and serve's wiring:** `NetError::NotReady` becomes `AdapterError::NotReady`, coded `companion_not_ready`, not `companion_unreachable`.
- **`crates/lodestar/src/companions.rs`:** a hand-over marker with no folder reserves the port; a companion with neither is left alone.
- **web:** `companion_not_ready` has its own words.
