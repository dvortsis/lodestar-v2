# X-wk10d-unknown decisions (#628)

An unknown check is busy, and waits at the back.

#615's review notes, collected in their own PR (runner, 2026-11-08T17:20Z).

## A claim's `Unknown` is busy, not stored
- A claim now takes the per-key answer (`StoredKeys::key_checks`, formerly `regrant_check`; it serves both paths now):
  - it grants only a `Free` key;
  - it denies a `Stored` or `Held` key as stored;
  - it denies an `Unknown` key (its store read failed) as **busy**.
- **The protocol's answer is now right:** `DenyReason::Busy` means "claim again later" (`proto.rs`, whose doc now names the store check too), where `Stored` says the key needs no fetch. A failed read still never grants a key.
- **Today's worker doesn't act on the difference yet.** Its claim loop (`worker/runtime.rs`) takes only grants and drops every denied key, whatever the reason. Such a key comes back only when the DHT finds it again. So a transient read error can still cost a key until then, as before this PR.
- **Follow-up (the runner queues it):** a worker-side retry of a `Busy` denial with back-off, so the protocol's "again later" is honoured.
- A check that dies or comes back short still denies the whole batch as busy, as before.
- `StoredKeys::stored` stays for the remote intake, where any non-free key reads as stored.

## An unknown re-grant waits at the back
- `LeaseAuthority::requeue_back` puts a key back behind the rest of the waiting keys. The checked re-grant uses it for an `Unknown` key, so a key whose read keeps failing doesn't stand first in every re-grant and hold up those behind it.
- A delivery that failed (`requeue`) still goes back in front, as before.

## A save skipped for a removed worker is logged
- `Workers::save_floor_pause` logs `workers.pause_not_saved` with `code="unknown_worker"` when the worker was removed before the save ran. Before, it returned silently.
- The event's doc names the three codes (`prefs_full`, `failed`, `unknown_worker`). Its labels are unchanged.

## Files
All within the runner's grant for this PR (2026-11-08T17:20Z):
- `crates/cluster/src/server.rs`;
- `crates/cluster/src/leases.rs`;
- `crates/cluster/tests/transport.rs`;
- `crates/lodestar/src/cluster/ops.rs`, `serve.rs` (the trait method's rename) and `logging/events.rs` (a doc comment);
- the decisions files.

## Tests
- `a_claim_denies_an_unknown_key_as_busy` (transport): free granted, stored and held denied as stored, unknown denied as busy, then granted once its check answers.
- `an_unknown_key_waits_behind_the_rest` (leases): an unknown key goes behind, a failed delivery goes in front, and a stale token takes nothing back.
- `the_floor_pause_save_follows_a_resume_or_a_revoke` (ops) now checks the `unknown_worker` code.
- The re-grant tests set their checks after the claims, since a claim now asks the same check.
