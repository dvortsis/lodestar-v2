# Durable partial-upgrade scheduling contract

Status: **specified** (DUR-10); DUR-11 fixture scenarios specified, executable
fixtures pending. CR-3 stays behind baseline review and stable ownership/outcome
interfaces. No migration, scheduler change or production data repair is performed
by this document.

## Existing limitation

`pipeline::start` currently asks `Store::partial_hashes(PARTIAL_REFETCH)` at startup
and adds those hashes to the restored fetch queue. The query orders by torrent ID
and limits the result. A new process can repeatedly favor the same unavailable
early rows while later partial rows remain unserved. Fetch snapshots preserve
attempts and source metadata but cannot express durable next eligibility or prove
that an upgrade was committed.

Keep the existing pool, single writer, SQLite transaction discipline, source DRR,
coordinator lease/fencing, consent and filter checks. A maintenance origin bypasses
only the known-stored-hash shortcut; it is not a new discovery, new source or a
permission to publish private work.

## Minimal durable state

Add one row per partial torrent in an indexed eligibility table. Proposed fields:
`torrent_id` as primary/foreign key, `next_due_at` as bounded integer UTC seconds,
`failures` as a saturating small integer, and `generation` as a monotonically
advanced token scoped to that row. Index `(next_due_at, torrent_id)`. Do not store
peers, hints, temporary ownership, names, duplicate source bitsets or a second copy
of the torrent's immutable identity. Generation overflow is an explicit terminal
error requiring review; never wrap into an old token.

Eligibility is initialized for existing partial rows in a bounded, resumable
migration/backfill whose progress is durable. A partially initialized migration
must resume without selecting only the first block forever. New partial imports
insert eligibility in the same transaction as the partial row. Complete imports
do not create it. Preserve the database's existing schema-version/migration gates.

Claims use the existing lease owner/epoch and add the eligibility generation to
the writer outcome token. There is no new distributed ownership framework or
unfenced background writer. On claim, atomically check that the torrent remains
partial and eligible, advance generation and set `next_due_at` to the bounded
existing lease expiry. This reservation survives a crash; expiration makes work
eligible again. A stale owner cannot clear, delay or complete a newer generation.

Use UTC only for durable due times; in-process waits use monotonic time. Clamp
restored future eligibility to the maximum supported retry/lease horizon, with
an aggregate anomaly report, so a clock jump cannot defer work indefinitely.
Do not move every row earlier merely because the clock changed.

## Due-work producer and service

Retrieve due work in bounded keyset pages ordered by `(next_due_at, torrent_id)`.
Recheck eligibility at claim, not just at query time. Claimed or deferred rows
move to a later due time; unavailable early rows cannot occupy every subsequent
page. If a page changes while being traversed, restart its bounded cursor from
the last inspected key and rely on conditional claims to prevent duplicate work.
Never use a fixed ascending-ID prefix as the whole maintenance population.

Use a bounded producer channel and the existing pool cap. Admission coordination
must reserve a small work-conserving maintenance service opportunity even during
continuous discovery. Reuse unused discovery capacity; do not reserve idle sockets
for maintenance without peers. Carry maintenance origin through lookup, fetch,
preparation, writer and restart. The exact service share and retry schedule are
experiment parameters chosen and recorded before CR-3 execution from the stable
CR-2 interface and baseline evidence; **they remain an implementation gate**.
Do not silently select numeric tuning defaults from this design document.

If the pool already owns the hash, merge the maintenance obligation and its token
into that lifetime, without resetting age, attempts, discovery provenance or
private-source restrictions. A real concurrent sighting still contributes its own
discovery evidence once. Maintenance cannot manufacture a discovery event, source
bit or cohort. The obligation persists until a matching writer outcome, explicit
ineligibility or a bounded defer outcome releases it.

## Outcome and transaction contract

| Outcome | Durable action | Attempt/failure accounting |
| --- | --- | --- |
| Pool/finder admission rejected | Reschedule bounded admission defer under matching generation | Neither network attempt nor failure |
| Evicted before terminal work | Release/defer matching obligation; preserve eligibility | Eviction, not an additional network failure |
| Accepted lookup/fetch attempted | Preserve claim until terminal outcome | Count the actual operation once |
| Terminal no-peer/network/verification failure | Increment bounded failure count once and compute next due | Failure only if applicable attempted work occurred |
| Pause/unsupported finder/consent unavailable | Defer without discarding row | No synthetic network failure |
| Verified result prepared | No success acknowledgment yet | Fetch success differs from committed upgrade |
| Writer commits partial upgrade | Replace canonical data; preserve provenance/corrections; clear partial and eligibility; append reindex work in one transaction | Commit success only after transaction succeeds |
| Writer sees already complete | Clear matching stale eligibility atomically if still applicable | Already-complete, not another stored torrent |
| Deleted/quarantined/filter denied | Respect current writer policy; invalidate matching obligation | No resurrection; distinguish ineligible from failed fetch |
| Transaction rollback/disk error | Leave durable claim/eligibility recoverable; no success | Explicit writer failure; retry must be idempotent |
| Stale token after new claim | Ignore state mutation and report stale outcome | No completion of the new generation |

Deletion cascades or explicitly removes eligibility in its transaction. Quarantine
must not admit new public work; release rechecks policy and eligibility instead of
blindly replaying a stale claim. A late verified response cannot recreate a deleted
row or defeat a tombstone. Reindex acknowledgment and database commit remain
separate: index lag is not a reason to repeat a successful network upgrade.

## Prepared failure matrix

Each fixture uses disposable data and an explicit fake clock/writer boundary.
No execution against live or original soak data is allowed.

- Seed 100,001 partial rows, make the first 50,000 unavailable, and demonstrate
  eligible later rows receive claims under continuous discovery. Measure maximum
  producer backlog and database growth; merely counting selected rows is not a
  fairness proof.
- Crash after page read, after claim commit, after pool admission, after verified
  preparation, before writer commit, and after commit before acknowledgment.
  Restart must recover due work or observe completed work without double success.
- Reject pool/finder admission repeatedly, then free capacity. Attempts and network
  failures stay unchanged until actual admitted work; eligible maintenance resumes.
- Evict a maintenance entry, rediscover its hash, then deliver the old outcome.
  Preserve the new token, provenance and due state; no stale success or delay.
- Race deletion/quarantine/release with claim and writer commit in both orders.
  Assert tombstone/filter policy and no resurrection.
- Merge public and private sightings with maintenance; preserve original source
  bits/cohort and public-routing eligibility. Maintenance counters stay independent
  of discovery conservation.
- Inject migration, WAL, claim and writer write failures. Verify recovery on a copy,
  foreign keys, resumable initialization and zero acknowledgment before commit.
- Move the clock forward/backward, exhaust the failure counter and exercise token
  overflow. Assert bounded eligibility horizons and no generation reuse.
- Drain all producer, pool and writer ownership after cancellation/shutdown. A
  bounded steady-state queue alone does not establish leak-free cleanup.

Before this becomes **source prepared**, settle the service share/retry parameters,
map the existing lease API and all writer outcomes to concrete types, review the
migration/storage estimate and prepare executable fixtures. Before **accepted**,
run those fixtures, full platform gates and an independently measured CR-3 trial.
