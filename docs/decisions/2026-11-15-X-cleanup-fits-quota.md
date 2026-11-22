# X-cleanup-fits-quota decisions

The cleanup stack (`infra/aws/fleet-control/cleanup.json`) gave both broker functions `ReservedConcurrentExecutions: 1`. AWS keeps at least 10 units of account concurrency unreserved, so on an account whose Lambda concurrency limit is 10, any reservation fails and the stack can't deploy. The owner's quota increase was denied (owner, 2026-11-15). Fleet-control and pia-delivery are deployed; cleanup waited on this change.

## Same bound, different mechanism
- **The bound is unchanged: at most one run of each broker (expiration, emergency) at a time.** Only the mechanism changes, from a Lambda reservation to a lease in S3. The runner flags this to the owner (`docs/owner-decisions.md` §2.19).
- **Lease:** one object per mode in the control bucket the stack already writes to, `locks/cleanup-expiration.json` and `locks/cleanup-emergency.json`. It records `version`, `mode`, `holder` (the Lambda request id) and `expires_at`.
- **Acquire before anything else.** The handler checks its execution account, then acquires the lease, before it reads the authorization, the credential or Linode. `prepare` and `dry_run` calls take the lease too, so the rule has no exceptions.
  - First a conditional create, `PutObject` with `If-None-Match: *`.
  - If the object exists (412, or 409 while a racing conditional write is in flight), read it. A live lease means busy. An expired one is taken over only by `PutObject` with `If-Match` on the ETag just read, so of two runs racing for a stale lease exactly one wins.
- **Lease length: 600 s.** It's longer than the 300 s function timeout, so a run can never outlive its own lease. It's shorter than the 15-minute schedule, so a run that crashes holding the lease delays the next scheduled run by at most one interval.
- **Release by conditional overwrite, not delete.** At the end, success or failure, the run overwrites its own lease version (`If-Match` on the ETag it wrote) with one that has already expired. A 412 means the run lost its lease, so it raises and the Errors alarm fires. Overwriting instead of deleting means the Executor role gets no `s3:DeleteObject`, which is narrower than the brief's ceiling.
- **The runtime's SDK must accept `IfMatch` and `IfNoneMatch` on `PutObject`.** Takeover and release both depend on it, and conditional deletes would add one more dependency. The Lambda Python 3.13 runtime's bundled botocore is expected to support them, but nothing here proves it. If it doesn't, every run fails closed at `lease`: safe, but cleanup is dead. So the runbook's post-deploy test runs the dry run twice on each function and stops on `busy` or `cleanup_lease_release_failed`.
- **IAM:** the Executor role gains one statement: `s3:GetObject` and `s3:PutObject` on exactly the two lease keys in the control bucket, with the same-owner condition (`s3:ResourceAccount`) kept. No wildcard key, no delete, no other bucket. Every lease call also sends `ExpectedBucketOwner`.

## A busy run is visible
- **Decision: a busy run returns normally and an alarm notifies.** It returns `{"status": "busy", "event": "cleanup_busy", "mode": ...}` and logs that one line, at warning level. It makes no other call.
- **Two new metric filters**, one per log group (`ExpirationBusy`, `EmergencyBusy`), count `cleanup_busy` lines into `LodestarCleanup/<stack>-busy`.
- **Term patterns, not JSON selectors.** Both the busy filter and the existing heartbeat filter now match the quoted term (`"cleanup_busy"`, `"cleanup_complete"`), not `{ $.event = … }`. Lambda's default text log format puts a level, time and request id before each JSON line, and nothing here proves a JSON selector matches such a line. A term matches either way, and neither term appears in any other log line. The runbook confirms the heartbeat once expiration is on: its alarm must return to OK. A new `BusyAlarm` (sum ≥ 1 in 5 minutes) notifies the same SNS topic as the other alarms.
- **Why not raise:** raising would show up as a function error and make Lambda retry an asynchronous expiration invocation for no reason. A busy run means the bound held, not that cleanup failed. It's still worth one email, because overlap only happens after a crash, a slow run or a duplicate invoke.
- **The emergency path also shows it:** `emergency_cleanup.py` treats any status but `complete` as a failure, so an owner dispatch that meets a busy broker ends red in GitHub Actions.
- **The heartbeat is unaffected:** it counts only `cleanup_complete`, so a busy run never counts as a successful one.

## Fail closed
- **Any lease error stops the run before any action:** a failed create, a failed read, a malformed, oversized or other-mode lease object, or one that expires more than `LEASE_SECONDS` + 60 s from now (no real lease ends that late, so it would otherwise read as busy forever), or a failed takeover that isn't a precondition conflict. The run then fails with stage `lease`, and the function's Errors alarm notifies.
- **A failure after acquiring still releases** the lease (best effort) and re-raises, so the next run isn't held up. If that release itself fails, it logs `cleanup_lease_release_failed` and the lease simply expires.
- **A malformed lease object blocks that mode until the owner removes it.** That's deliberate: unknown lease state never authorizes work. The runbook says how to clear it ("Clear a damaged lease").
- **The summary is logged before the release.** A run that deleted VMs and then failed to release still leaves its summary (and the heartbeat's `cleanup_complete`) in the log, then raises.
- **A 409 (`ConditionalRequestConflict`) counts as busy.** On a takeover, the run returns busy. On the very first create, the object may not exist yet, so the read is refused (no `s3:ListBucket`) and the run fails closed at `lease`. Both cases are tested.

## Template size
- **`cleanup_code()` now compacts the inlined source.** Indentation becomes one space per block level, and plain double-quoted strings get single quotes, because JSON escapes every `"`. With the lease the template came to 51,521 bytes, over CloudFormation's 51,200-byte inline body limit, because the handler is inlined into both functions. Compacted, it's 50,440 bytes. The other way out, uploading the template to S3 for packaging, is an owner-reviewed change (`render()` refuses it), so it wasn't taken.
- **Only 760 bytes of headroom are left** (51,200 is a repository rule in `render()`; the console's own upload path isn't bound by it). The next change that grows the handler goes to the runner for an owner decision on packaging, not more compaction.
- **The compaction can't change the program.** It works on tokens, not `ast.unparse`, so spelling stays stable across Python versions, and f-strings and strings containing quotes or backslashes are left alone. It refuses to build if the result's AST differs from the original's. `test_cleanup_policy.py` runs the packaged code's lease scenarios as well as its cleanup checks.

## Checks
- **`deployment.guard`** now rejects `ReservedConcurrentExecutions` on either cleanup function, as does the template audit in `test_cleanup_policy.py` (`audit()`). `infra/fleet/deployment_policy.py` is unchanged; it audits GitHub settings, not templates.
- **Tests:** the real handler is run against a fake S3 with S3's conditional semantics (412 on an `If-None-Match` or `If-Match` conflict).
  - Two overlapping runs of one mode delete once, and the second makes no Linode call.
  - A stale lease is taken over exactly once when two runs race for it.
  - Lease errors fail closed.
  - Mutants that drop any one of the three precondition headers, or stop treating 409 as a conflict, fail a scenario.
  - Policy mutations that widen the lease keys, add delete, drop the owner condition or bring back the reservation all fail the audit.

## Docs
- **`docs/operations/fleet-control-bootstrap.md` and `fleet-control-cleanup.md` were rewritten** to match what's deployed: fleet-control and pia-delivery are live, and cleanup waits on this change. Each ends with the AWS console steps for the cleanup stack (review a change set, then create), then the `fleet-cleanup` environment and the cleanup token. The older PowerShell proposal text is replaced. The deployment boundaries it described are kept as reference.

## Not done here
- **IAM doesn't require the conditional headers.** S3's `s3:if-none-match` and `s3:if-match` condition keys could force every lease write to be conditional. That would take two statements and about 300 bytes of the remaining headroom, and the brief doesn't ask for it. Left for the runner to decide.
- **`infra/aws/fleet-control/README.md` still says the cleanup stack has 13 Adds; it's now 16.** That file is outside this slice, so the runner gets a follow-up.
