# Handover: the build is held by the owner's Claude Duet chat

**Send runner messages to: `session_01MMjhnCpMMYF4uEW49L9SpF`** (the same session as `cse_01MMjhnCpMMYF4uEW49L9SpF`). This is the holder's bookkeeping thread. It answers for the runner: slice and PR readiness, X-test-vm and API-lane questions. Don't message the paused runner session `session_01EYhgh4JX3AmHzKXPjYCoUg`.

**Holder:** the owner's Claude Duet chat. Its coordinator session (`session_01MMjhn8V1uDvWKEpC8V3BHU`) starts one thread session per build slot; its bookkeeping thread (`session_01MMjhnCpMMYF4uEW49L9SpF`) keeps `state.json`, the dashboard, API-lane answers, X-test-vm support and the merge gate.
**Since:** 2026-11-24T06:44Z.
**Why:** the owner moved the runner's work to the Duet chat to save standard-plan credit. Owner, 2026-11-24T06:19Z: "Yes, behave as the runner does." Owner, 06:30Z, approving the coordinator's proposal to pause the runner, take on all of its work (API lane and cloud testing included) and mark the build handed over: "Perfect. Thanks!" Owner, 06:44Z: "I paused it."

**Runner and watchdog:**
- Runner session `session_01EYhgh4JX3AmHzKXPjYCoUg` is paused by the owner (its own record, `state.json` `pause.runner`, 06:30:19Z). Its heartbeat `trig_01U8KNWyDovQ9omwvZi8cuLW` is disabled; it didn't fire at 06:38Z.
- The watchdog `trig_0172BngAUoA4NRaRWBYiRWVf` is still enabled. While this file exists it does nothing (`docs/handoff.md` §5).

**How this holder differs from Path B (`docs/handoff.md` §5), by the owner's instruction to behave as the runner does:**
- Up to 4 build slices run at once (`limits.phase_total`, owner 06:26Z: "You can leave it at 4 slots"), each in its own thread session. They don't overlap `owns`.
- The holder keeps `state.json` `slots` current, because the dashboard reads it.
- The holder merges, under the runner's gate unchanged: a fresh Opus review (plus `/security-review` where the runner requires it); `ci-ok`, `web-ok` and `e2e-ok` green on the newest head; the freshness rule; a merge commit pinned to the reviewed head.
- Spend stops and hard rules are the runner heartbeat's, unchanged.

**Claims** (`<slice> · <agent> · <branch> · <owned paths> · <date> · <state>`):
- ENR-11 · Duet thread cse_01MMjhnBQhGk9LHSMPPEyyEr · slice/ENR-11 · slices.yaml owns · 2026-11-24 · building
- NOS-2b · merged #1109 (32396391), 2026-11-24 · slot 2 freed
- X-escalate-full-table · slot 2 · slice/X-escalate-full-table · brief briefs/X-escalate-full-table.md · 2026-11-24 · starting
- IRC-1 · Duet thread cse_01MMjhnD539Mu5nKX1dp6JLD · slice/IRC-1 · slices.yaml owns · 2026-11-24 · building (PR after NOS-2b, shared lockfiles)
- GS-5 · Duet thread cse_01MMjhnCYYLLydZtF2G8z2S2 · slice/GS-5 · slices.yaml owns · 2026-11-24 · building

**Not touched by this holder:** the owner's local API lane (outside the cap, `extra`; it never merges); #632 (the owner's branch); #920 X-operator-cap-60 (held, operator unfunded).

**Runner slices still running at the handover:** none; the runner's four slots were empty.

**Returning runner:** read this file first, adopt the claims above as registered slices, and delete this file only after the owner confirms (`docs/handoff.md` §5 step 5).
