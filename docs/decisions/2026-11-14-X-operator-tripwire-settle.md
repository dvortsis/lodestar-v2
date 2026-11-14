# X-operator-tripwire-settle decisions (PR to follow)

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
- **Completed runs come from one `status=completed` list,** created from 180 minutes before the lookback (`COMPLETED_EXTRA_MINUTES`).
  - A `skipped` run, or one last updated before the lookback, is passed over without reading its jobs.
  - Any other conclusion, a new one included, has its jobs read, so it fails closed.
  - This replaces eight conclusion lists over 30 days. So more than 1,000 old successful runs no longer overflow a list.
- **A completed run's jobs are read once** and kept across the attempts.
- **The cost:** a quiet pass is five requests (two counts, the completed list, two counts again). Thirty of the runner's channel comments in the lookback fit under `SESSION_REQUESTS` (40), and a test checks it.
- **Reading of the brief:** the brief said to read completed runs "only from the lookback's start". Taken literally, that misses a run created a little before the lookback whose session ran inside it. Reading from 180 minutes earlier covers a wake approved up to about two hours late.
- **Accepted residual, failing open:** a completed run approved later than that, whose session ended inside the lookback, isn't read.
  - **Why it's accepted:** its own end-of-wake sweep ran after the session ended, and it disables if that sweep can't finish.
  - **What's left uncovered:** only a later hourly sweep that can't finish misses it.
  - While such a run is still in flight, it's read whatever its age.

## A run in flight with no jobs listed yet
- **Kept failing closed; recorded as an accepted residual.** A run in flight whose jobs, or whose `gate` job, aren't listed within the re-reads (five, two seconds apart) counts as a session, whoever started it.
- **Why not handle it:** GitHub lists a comment's skipped gate as soon as the run exists, as real runs on this repository show, so this needs a backlogged Actions queue. Telling a stranger's run from the runner's without its gate would need the comment's author. The sweep jobs don't receive the runner's ID, and passing it in would be a workflow change.

## Unchanged
- The gate-success rule and the exact workflow-ref match for the end-of-wake sweep.
- The escalation posted before the check, and a failed post with nothing disabled failing the job.
- The time budget's 60-second margin (the budget test now also counts the settle waits).
- No workflow, permission or prompt change, so `operator-apply.yml` needs no re-dispatch.
