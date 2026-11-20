# X-operator-tripwire-settle decisions (#865)

This supersedes the session check's window and retry bullets in `2026-11-14-X-operator-hardening.md` (OPERATOR-01). These change the tripwire's wake-session check (`infra/operator/tripwire.py`, `_wake_session`), which decides whether an incomplete sweep disables `operator-wake.yml` under the owner's decision of 2026-11-14 (`2026-11-14-X-operator-hardening.md`). They come from the reviewer session's third delta on #859, at component level. The rule itself is unchanged: an incomplete sweep disables when a wake session ran inside its lookback, or that can't be read.

## Settling counts
- **Counts that don't add up are read again after a wait.** The check makes up to three attempts, with `SETTLE_SECONDS` (4) between them, through the injected wait function and inside its 45-second deadline. Before, the attempts followed each other at once, so a comment's run still moving from in flight to skipped could use them all up.
- **A last mismatch made only by others' runs is no session.** It counts as no session when both hold:
  - every run listed in flight had a `gate` job that finished without success, so none can hold a session;
  - the latest count leaves no run in flight unlisted.
- **Any other last mismatch still counts as a session (fail closed).** That covers a run in a status no list covers, and the runner's own run still moving.
- **Stricter than the brief:** the brief asked only for the first condition. The second keeps a run in an unknown in-flight status from passing as "no session".

## The request budget and the window
- **Runs in flight** (`in_progress`, `waiting`, `queued`, `pending`, `requested`) are still read from the last 30 days, the longest a job can wait for approval.
  - Their number is all runs minus `completed` ones, from two counts.
  - The status lists stop once they've found that many. With nothing in flight, which is usual, no list is read.
- **Completed runs are listed by conclusion,** created from 72 hours before the lookback (`COMPLETED_EXTRA_MINUTES`).
  - **Never `skipped`:** a comment alone makes a skipped run, so any number of comments costs nothing.
  - **The common conclusions first:** `success`, `failure` and `cancelled`.
  - **The rare ones only when needed:** they're listed only while completed minus skipped runs are still unread.
  - **Still unread after that** (a conclusion GitHub adds later, or runs still completing): the counts don't add up, so it fails closed. A count below zero, from an index that contradicts itself, is never settled on either.
  - **Passed over without reading its jobs:** a listed run last updated before the lookback.
  - **No overflow from old runs:** this replaces eight conclusion lists over 30 days. More than 1,000 completed runs that aren't skipped within 72 hours would overflow, and that fails closed.
  - **Counting order:** every count that's subtracted is read first. A run that moves between the two reads can then only make the check look for more runs, never fewer.
- **A completed run's jobs are read once** and kept across the attempts.
- **The cost:**
  - **A clean pass** is nine requests plus a page for every 100 completed runs past the first, plus a jobs read for each run updated inside the lookback.
  - **One settle retry** repeats the counts and lists, but not the jobs reads.
  - **What fits `SESSION_REQUESTS` (40):** with 300 completed runs in the 72-hour window and one settle retry, about 14 of the runner's channel comments in the lookback; a test checks 12. On a clean pass it's about 28. Past that, it counts as a session (fails closed).
- **A run that just settled:** an in-flight list whose copy of a run already shows it completed and skipped passes over that run, and the counts decide. Before, it made a mismatch that could never count as settling.

## No session outside the window (the runner's call)
- **A wake whose run is too old refuses to start a session.** `wake.py` reads its own run's creation time with the job's token (`actions: read`, which it already holds). It refuses when the run is older than `WAKE_RUN_MAX_AGE_MINUTES`: `COMPLETED_EXTRA_MINUTES` less `WAKE_JOB_MINUTES` less `WAKE_AGE_MARGIN_MINUTES`, that is 72 hours less 45 minutes less 15.
  - **Why the margin:** the 15 minutes cover the setup before the check, a cancellation's overrun and clock skew between the runner and GitHub.
  - **Where it runs:** before the lock, so a refused run holds nothing, and so before the token, the vault write and the session.
  - **What the refusal does:** it posts the usual escalation, "no session started: the instruction was approved too late …; post it again", and starts no session. The runner posts the instruction again.
  - **Fail closed:** a creation time that can't be read refuses too.
  - **Why this closes it:** a session ends within the wake job's 45 minutes. Every sweep whose lookback reaches that end reads completed runs from `LOOKBACK_MINUTES` + `COMPLETED_EXTRA_MINUTES` back, so it finds the run with the margin to spare.
  - **In sync by construction:** `tripwire.wake_run_too_old` holds the cut-off, and `wake.py` calls it, so the two can't drift apart. A test checks the boundary against the window, and `test_workflows` pins the wake job's timeout to `WAKE_JOB_MINUTES`.
- **This closes the late-approval residual,** which failed open. The runner decided to close it rather than ask the owner to accept it. It makes the check stricter: an instruction approved more than about 72 hours late is refused instead of run. The runner flags it to the owner (`docs/owner-decisions.md` §2.19).
- **The same holds for a late approval completing during the check.** Its run can't be older than the window, so the completed list finds it.
- **No workflow, permission or prompt change.** The agent's prompt and config are unchanged, so `operator-apply.yml` needs no re-dispatch. `wake.py` runs from `main`.

## Other accepted residuals, failing closed
- **A burst of others' runs in flight.** Each run in flight costs a jobs read, so a timed burst of comments in flight at the moment of the check can pass the request cap, which counts as a session. It was the same before this change.
- **If GitHub caps totals at 1,000.** Then a window whose counts sit at the cap is split, which costs more requests and can reach the cap. The cost figures above assume no cap; the first run verifies it.

## A run in flight with no jobs listed yet
- **Kept failing closed; recorded as an accepted residual.** A run in flight whose jobs, or whose `gate` job, aren't listed within the re-reads (five, two seconds apart) counts as a session, whoever started it.
- **Why not handle it:** GitHub lists a comment's skipped gate as soon as the run exists, as real runs on this repository show, so this needs a backlogged Actions queue. Telling a stranger's run from the runner's without its gate would need the comment's author. The sweep jobs don't receive the runner's ID, and passing it in would be a workflow change.

## Unchanged
- The gate-success rule and the exact workflow-ref match for the end-of-wake sweep.
- The escalation posted before the check, and a failed post with nothing disabled failing the job.
- The time budget's 60-second margin (the budget test now also counts the settle waits and a last wait before the deadline).
- No workflow, permission or prompt change, so `operator-apply.yml` needs no re-dispatch.
