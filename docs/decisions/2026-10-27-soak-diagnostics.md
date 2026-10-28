# Hardened Windows baseline diagnostics

The owner approved a fresh integrated CR-0 baseline and bounded observability on
October 27. Admission, retries, sampling, transport and all resource caps retain
their existing behavior. New counters are observations, never scheduler inputs.

## Admission boundaries

`dht_peer_rejected_total` retains its previous boundary. Its new exclusive
`dht_peer_rejection_reasons_total{reason}` partitions are `caller_closed`, then
`paused`, then `capacity`. If acknowledgment delivery fails after the initial
closed check, `dht_peer_ack_failed_total` increments separately; neither admitted
nor the legacy rejected total changes. Command-channel failures are counted by
`dht_peer_command_full_total` and `dht_peer_command_closed_total` before actor
admission. These boundaries must not be added together as network attempts.

Pool finder-channel full/closed counters describe an earlier handoff.
`fetch_lookup_unaccepted_total` counts requests with no finder channels accepted,
all replies finished without observed ownership, or a timed-out request with no
observed ownership. Cancellation/eviction is not labeled an admission rejection.
`fetch_retry_unaccepted_total` counts a second or later actual lookup request in
the current entry lifetime while accepted-attempt count remains zero. Waiting
for a pool slot does not count as a request. Private-source deferrals are included
in the pool-level no-finder case and must not be attributed to DHT rejection.

## Eviction boundaries

`fetch_evicted_service_total{stage}` partitions eviction by furthest known service:
`fetch`, then `peers`, then `lookup`, otherwise `unaccepted`. New discoveries lost
before insertion count as unaccepted. Fetch-start and request-start flags are
transient booleans, bounded by the existing entry cap; no new map, task, timer,
label or durable field is added. Restored entries retain persisted attempt count,
but historical peers/fetch flags are unknown and reset. Thus the metric is not a
complete historical service classification across restarts.

## Isolated cost contract

The two entry flags own no heap allocation. The layout check compares the actual
target's `Entry` with the exact pre-diagnostic field types at `5e1a53c0`, including
compiler padding, and limits growth to one entry-alignment unit. At the unchanged
200,000-entry cap, report the measured byte delta multiplied by that cap. The
pool adds eight atomic counters (64 payload bytes); each DHT actor adds six
(48 payload bytes), plus six scalar fields in each copied stats snapshot.
These are per-pool/per-actor costs, not per-discovery labels or retained records.

Before inclusion, run the ignored release `diagnostic_atomic_cost_report` alone,
after builds and browser suites finish. It alternates nine one-million-event
trials with the old aggregate increment and aggregate-plus-category increments,
checks exact final accounting and reports median and range. The preselected
microcheck ceiling is less than 100 ns of added median time per event. This is
an instrumentation sanity check, not a product latency or throughput budget.
Uncontended atomic cost does not measure scheduler behavior, concurrent readers,
metrics serialization, network efficiency or end-to-end crawl performance; those
remain part of the integrated baseline review. Do not describe this microcheck
as a measured crawler speedup.

## Harness and reporting

Version 2 samples carry sequence and process identity. The analyzer remains
compatible with version 1. Missing or invalid termination evidence means
completion unknown, independently of observation freshness. Freshness never proves
the process is alive. Reports export only fixed reason enums, not arbitrary labels.
Hourly counters assign each valid interval to its ending hour and report its
covered duration; they do not estimate events within that interval.

Stored throughput is not searchable throughput; pool events are not unique hashes.
First-peer latency, cold-cohort age, kernel drops and byte efficiency remain unknown
unless measured separately. No lookup-leg/logical-request percentage is derived.

Launch defaults are 50 GiB free, 20 GiB warning, 10 GiB orderly-stop reserve,
10-second storage checks and a 1 GiB combined redirected-log cap. They are harness
safeguards, not performance budgets. No automatic cleanup is performed. Disk
exhaustion can still prevent final evidence from being durable; cleanup and
independent final writes are attempted, and failure is not reported as completion.
