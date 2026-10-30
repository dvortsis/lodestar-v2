# Streaming and responder-hint implementation contracts

Status: **specified; source implementation and runtime acceptance pending**
(DUR-05–07). These are separate experimental slices after baseline review. Neither
changes the next baseline. Streaming can ship behind its own experiment switch
without responder hints; hints must be measured separately before combining them.

## Current boundary and required replacement

`sources::PeerRequest` currently acknowledges admission with an optional one-shot
receiver whose only successful value is the final peer vector. The pool's
`FinderReply` and entry `State` couple lookup completion to starting a fetch.
Replace this boundary as a coordinated pool/plugin/actor change; adapting only
the DHT actor would leave pool ownership and cancellation incorrect.

An accepted finder returns a `LookupSession` with a bounded batch receiver and
a separate terminal one-shot receiver. Rejection remains `None`. Allocate the
session only after actor admission. The terminal result is a fixed enum:
`Complete`, `Deadline`, `Cancelled`, or `SourceFailed`; it contains no peer list.
Dropping the terminal sender means source failure, never successful completion.
The producer must resolve terminal delivery independently of batch-channel space.

Each batch belongs to the existing request generation and finder/family identity.
Use the request object identity as the receiver fence; include generation in
pool event dispatch so a stale ready event cannot attach to a recreated entry
with the same hash. Do not add infohash or endpoint labels to telemetry.

## Bounded peer delivery

Use a fixed maximum eight endpoints per batch and two queued batches per finder.
The actor may hold one pending batch while a channel is full. All vectors are
constructed with bounded capacity; no unbounded staging queue or task per peer.
These are proposed candidate bounds, to be recorded before the experiment; they
do not increase the existing pool's 32 retained peers per key or network caps.

Validate every endpoint with the existing public endpoint policy before buffering.
Normalize mapped addresses consistently and deduplicate against that session's
bounded seen set and the pool entry's retained/tried peers. The actor seen set is
also capped at 32 endpoints per finder session. Once the cap is exhausted, ignore
further endpoints for that session; do not grow the set to remember all replies.
Account for all queued, actor-pending and seen-set storage in the candidate's
memory check, multiplied by actual admitted finders across both families. The
32-per-key pool cap is not a claim that these separate buffers consume no memory.

On a full channel, merge new distinct peers into the single bounded pending batch.
If it is full, discard additional batch candidates with a fixed aggregate
backpressure-drop counter. Continue terminal handling and deadline processing;
do not await an unbounded consumer stall inside the actor. On termination, attempt
one final nonblocking flush, count any discarded pending peers, close the batch
sender and resolve terminal. This explicitly permits losing peer candidates under
backpressure, while never losing the ownership-completion signal. It is a measured
tradeoff, not equivalent conversion quality by construction.

When the pool observes terminal, drain the at-most-two already queued batches
before releasing that finder session. A terminal event selected before a ready
batch must not discard that batch. An entry may reject excess peers because its
own cap is full; report that separately from producer backpressure loss.

## Independent lifetimes

Track lookup ownership separately from fetch readiness/in-flight state. The pool
may own one logical lookup (with admitted family legs) while a socket fetch is
active. Receiving a batch makes the key socket-eligible; it does not release a
lookup slot, change logical attempt count, extend the deadline or create another
logical lookup. The first observed admitted finder still consumes one attempt.
Multiple admitted families remain one logical attempt under one shared deadline.

| Event | Lookup ownership | Fetch/entry action |
| --- | --- | --- |
| First valid batch | Retain | Queue eligible peer under existing socket scheduler |
| Fetch fails with more peers | Retain until terminal/deadline | Try next peer within existing limits |
| Fetch fails with no peers, lookup active | Retain | Wait for next batch or terminal; no duplicate lookup |
| Finder terminal, another finder active | Release only that finder | Retain other ownership and any usable peers |
| All finders terminal | Release logical lookup once | Fetch available peers, otherwise use existing backoff/terminal policy |
| Verified fetch succeeds | Cancel every finder and release receivers | Forward exactly one verified result to writer; stale batches ignored |
| Eviction/cancel/filter rejection | Cancel every finder | Drop buffers and invalidate generation; no retry from stale events |
| Source crash/channel close | Classify source failure | Other families may continue; no fabricated completion success |
| Shutdown | Cancel all sessions | Drain owned tasks/receivers within shutdown contract |

Actor cancellation must be observed when receivers close even if its batch channel
is full. Pool success and terminal completion racing must not decrement ownership
twice. Slot release must remain independent of writer success: metadata fetch
success is not durable storage acknowledgment.

## Responder hints are a separate capability

A sampling responder is a DHT UDP endpoint, **not** a peer-wire TCP endpoint. A
hint may seed a `get_peers` lookup only; never feed it directly into the fetcher.
Keep at most one qualified hint per address family on a pending key. Store only
the normalized endpoint and monotonic receipt time; discard on eviction or
terminal removal. Do not persist hints, extend them on unrelated rediscovery or
accept an externally supplied timestamp as fresh.

Use a proposed 60-second TTL and a single hinted attempt per family within the
existing logical lookup deadline. Expired, invalid, private, unsupported-family
or throttled hints immediately use ordinary routing. A send failure does not
restart the logical deadline. Keep actual XOR-distance ordering for ordinary
routing; a hint does not rewrite distance or count as verified routing knowledge.

Rate accounting uses canonical IP, across ports and both hint/fallback paths.
Allow one in-flight hinted query per IP and apply a 60-second not-before interval
from the actual permitted send. This guard has a hard 20,000-IP entry bound;
unexpired restrictions are never evicted to admit a new IP. When full, skip the
hint. Ordinary routing must consult a retained restriction before sending to that
same IP, so fallback cannot bypass it. Unrestricted ordinary-routing peers retain
their existing policy. These guard/TTL defaults belong to the candidate record
and must be reviewed before implementation, not adjusted silently during a run.

Hint queries share existing actor admission, global outstanding RPC, family and
deadline budgets. They never reserve extra sockets or increment logical attempts
separately. Check consent/private-source policy again at dispatch. Public endpoint
checks must apply before entry insertion and before actual send; reject port zero,
private/reserved endpoints and unsupported address encodings through existing
policy helpers. A hint is untrusted routing assistance, not corroboration or T2.

## Prepared regression cases

These are fixture specifications, not executed tests. Implement using scripted
finders, paused clocks and condition deadlines of at least ten seconds:

| Case | Controlled sequence | Required observation |
| --- | --- | --- |
| Early batch | Admit; batch; hold terminal | Fetch starts while lookup ownership remains one |
| Full channel | Stop consumer; fill two slots and pending batch; terminate | Terminal delivered; bounded drops counted; queued batches drained |
| Success race | Fetch verifies while terminal and batch are ready | One result; all finder ownership closes once |
| Failed first peer | Batch one peer; fail it; later batch arrives | Later peer attempted without a second lookup |
| Stale generation | Evict A; recreate A; deliver old batch/terminal | New entry untouched; old ownership released |
| Dual family | Admit v4 and v6; fail one; hold other | One logical attempt; shared deadline; cancellation closes both |
| Source failure | Drop terminal sender with queued batch | Source-failure outcome, no false completion success |
| Hint expiry | Advance exactly to and past TTL | Boundary specified as age < TTL; expired hint falls back immediately |
| IP rotation | Same IP with different ports/mapped form | One canonical restriction, no fallback bypass |
| Guard saturation | Fill with unexpired restrictions; offer new IP | No eviction of restrictions; ordinary eligible work continues |
| Private key | Public hint arrives for private-only work | No public dispatch and no hint-derived provenance promotion |
| Final drain | Cancel/complete all fixtures | Zero live sessions, pending batches and owned tasks |

Acceptance requires deterministic accounting, fixed memory bounds, no lost
completion, fresh correctness/security review, and a separately measured trial
after CR-0 acceptance. Do not merge streaming, hints, fairness, transport or
prefix targeting into one performance comparison.
