# X-operator-wake-outcome decisions (#870)

These change how the bridge (`infra/operator/wake.py`, `watch`) reads the result of an operator session. They refine the stall and error bullets of `2026-11-11-OP-4.md` and keep everything else there. The owner asked for the diagnosis and the fix on 2026-11-14 (in a session of their own, about 05:40Z); the runner was told the same hour.

## What went wrong on the first live wake
- **The first live wake (`ping-1`, approved 2026-11-14T04:29Z) reported "the session ended with an error" and archived its session.** The API key, the vault, the App token and the session create all worked. No reply came from the operator.
- **The bridge read a field the API doesn't have.** `watch` took the stop reason from `GET /v1/sessions/{id}`. That object carries `status` and no `stop_reason`; the reason is only on the session's `session.status_idle` event. So the reason was always empty, and every idle session fell through to "ended with an error":
  - a clean finish (`end_turn`);
  - a call paused for approval (`requires_action`), which the bridge is meant to deny so the session can go on;
  - a budget stop.
- **A terminated session read as "done".** The API ends a session as `terminated` on an unrecoverable error or an archive. A session that finishes its work goes idle. So the one status the bridge counted as success was a failure.
- **The offline suite couldn't see it.** The test fakes put `stop_reason` on the session object too, so 265 tests agreed with the bridge and not with the API.
- **The session itself ended its turn cleanly, having done nothing.** The owner read it in the Console (2026-11-14, about 05:57Z): the `auto` permission policy had denied the operator's `bash` call outright, so it couldn't fetch the helper, read the channel or post its report. It said so in its last message and stopped. The logs showed none of this; after this change the bridge reports it (below).

## Reading the outcome
- **The status comes from the session, the reason from its events.** Once the session is idle or terminated, the bridge lists its events and takes the stop reason of the last status event (`session.status_running`, `_idle`, `_rescheduled`, `_terminated`), and only if that event is a `session.status_idle`. Thread-level status events don't count.
- **`end_turn` is the only success.** `budget_reached` and `requires_action` keep their OP-4 handling. `retries_exhausted`, and any reason the bridge doesn't know, escalate as an error. `refusal` (the model's reply was refused) escalates with its own line.
- **`terminated` always escalates,** as "the session was terminated". The bridge archives only after `watch` returns, so a terminated session seen while watching is an error or an archive from outside, never the bridge's own.
- **An idle the events don't explain yet is polled again, not guessed.** The status and the event list are written separately. If the last status event isn't an idle, or carries no reason, the bridge waits and reads again, for at most six polls in a row (a minute, `STALL_POLLS`, the same bound as a stall). Then it sends an interrupt (nothing happens on an idle session; it stops one whose status lagged behind a resume) and escalates: "the session went idle and its events show no stop reason". This fails closed: nothing is treated as done without an `end_turn`. A stall ends with the same interrupt.
- **An event list that can't be read fails the wake.** A reply with no list, or more than 50 pages, raises; the bridge then interrupts, escalates and archives, as for any other failure mid-session.
- **The stall count now covers consecutive idle polls.** A poll that finds the session running resets it. Before, only a denial reset it; a session can't leave a pause without one, so the bound is the same in practice. The 30-minute limit still covers everything.

## Naming an error
- **An error escalation names the error's type and retry status,** from the session's last `session.error` event: for example "the session ended with an error (billing_error, exhausted)". Without this the owner had to open the Console to learn why a wake failed, and each wake costs an approval.
- **Only values on a fixed list are printed.** The type must be one of the API's fifteen `session.error` types and the retry status one of `retrying`, `exhausted`, `terminal`. Anything else prints as "an unlisted error type" or "an unlisted retry status". The error's free-text `message` is never read. This keeps the rule that logs and public comments carry fixed lines only (plan §3, "Nothing private reaches a public log").
- **A recovered error isn't reported.** A session that retried and then finished cleanly answers "done".

## A finish with denied calls
- **A clean finish is escalated when the permission policy refused any of the session's tool calls.** After `end_turn` the bridge reads the events once more and counts the tool calls whose `evaluated_permission` is `deny`. If there are any, it posts "the session finished, but the permission policy denied N of its tool calls (reason); the operator may have posted no report". With the stop reason fixed alone, the first wake would have read as done and the runner would have waited for a report that couldn't come.
- **The reason is the policy's `reason_code`, from a fixed list:** `high_risk` (judged high-risk) or `indeterminate` (no judgement reached and the call not held for approval, which the API reference ties to an organization out of credits or at a spend limit). Anything else prints as "an unlisted reason". The call's name and input are never read.
- **Calls the bridge denied itself aren't counted.** A call paused for approval stays `ask` in the events; the deny message already tells the operator to escalate.
- **The wake's result doesn't change.** The job still exits 0 and the session is archived as before. `watch` still answers "done", so the injection harness's verdicts are untouched.

## What doesn't change
- **No limit, gate or permission moves.** The budgets, the rate limits, the $200 total, the lock, the token's scope, the `auto` permission policy and the deny message are as they were. The bridge makes one more read of an endpoint it already used (the session's events).
- **The injection harness uses the same `watch`,** so check 8 gets the same fix. Its outcome codes are unchanged: the new lines map to `error`, as an unknown outcome did.
- **The test fakes now serve what the API serves:** a session object with a status only, and the stop reason on a status event. A regression test hands `watch` a session object with a made-up `stop_reason` and checks it's ignored.

## Open, for the runner and the owner
- **The spend ledger refuses a session with no `list_cost`.** The API documents `usage.list_cost` as absent "until cost tracking is available for the session". If the archived `ping-1` session has none, the next wake answers "no session started: the spend ledger is unreadable". Reading a missing cost as zero would loosen a spend check, so this PR leaves it alone. If `ping-2` hits it, the choice is the runner's to put to the owner.
- **The `auto` policy denies the operator's `bash` calls, so the operator can't work yet.** This PR makes the bridge report it; it doesn't change the policy. The fix depends on the `reason_code` of the denied call, which the owner is reading from the Console: `indeterminate` points at credits or a spend limit; `high_risk` points at what the system prompt asks for (download two scripts, then run them). Mounting the helper files into the session, as the injection harness already does, would remove the download. Moving `bash` off `auto` would loosen an OP-4 security decision, so that choice is the owner's.
