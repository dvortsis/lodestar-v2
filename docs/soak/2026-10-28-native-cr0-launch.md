# Native Windows CR-0 launch — October 28, 2026

Status: **running; measured baseline in progress; acceptance pending**.
The [October 29 checkpoint](2026-10-29-native-cr0-checkpoint.md) preserves the
16-hour results, diagnostics and owner-confirmed external disk consumption.
The owner explicitly authorized launch ("Ok. Go.") after reviewing readiness and
the six-hour sampling suppression. The previous interrupted run remains preserved.

## Frozen candidate and launch

- Source: `744b84d68c7609d8220430ffc9938c05c6d6a527`.
- Package: integration checkout `target/launch-candidate-r21`.
- Executable SHA-256: `24CB1EF43D86AD8C0F9F80C41E9D2D661F3FD6FE5573E72C13A87285E11E1475`.
- Evidence: repository-root `target/soak/cr0-native-2026-10-28-744b84d6-r2`.
- Launch review and supervisor identity: integration checkout
  `target/launch-supervision-r21-r2/` (private).
- Collector PID: `98244`; start ticks: `639288302350369040`.
- Crawler PID: `49888`; identity: `49888:639288302370712270`.
- Collector run start: `2026-10-28T18:23:56.142782-06:00` (America/Denver).

Package verification passed immediately before launch. The unchanged launch review
hash was `1A8FC66FAB1E298B4659F3A042B00AD7A2D9250BBF6CAB1EACD57695299376BD`.
Docker processes were absent, HTTP 8484 and UDP 6881 were free, and the run
directory did not exist. C: had 124,578,156,544 bytes free. The frozen collector
repeated its own launch checks and created fresh data and evidence directories.

First observation: health 200, required metrics readable, embedded UI verified,
stable process identity recorded, startup sampling suppression active, and
no ownership-cap exceedance. Disk monitoring wrote independent observations with
zero read failures. These startup checks do not establish sustained crawl progress
or baseline acceptance. A subsequent read at approximately 12 seconds verified
three received DHT packets and three routing nodes, establishing initial public
network replies. Raw logs, setup credentials and data remain private.

At 18:25 MDT, two metric samples were valid (zero invalid samples), eight disk
observations had no read failures, both process identities matched, and collector
stderr was empty. Public-network counters had advanced to 20 packets received,
46 sent and nine routing nodes. Latest sampled RSS was 33,361,920 bytes; free disk
was 124,534,685,696 bytes. Docker remained absent. Read-only analyzer output and
the aggregate startup check are preserved under the private supervision directory.

## Preserved launch-context correction

The first attempt at 18:21 MDT used the offline sandbox account. It recorded two
healthy HTTP/metrics samples but zero DHT replies, zero routing nodes and 15
timeouts. This was not a usable public-network baseline. The operator requested
orderly stop through `stop.request`; finalization completed at 18:23:28 MDT with
confirmed Ctrl+C, exit 0 and a 1.25-second shutdown. Evidence remains at the path
without the `-r2` suffix, with its separate supervision record. No data was removed
and no sample from that attempt contributes to the new run.

After confirmed cleanup, the authorized public-network launch was executed outside
the offline sandbox. Auto-review approved that execution. Package hashes and
environment gates were rechecked. Only the fresh evidence/data directory changed;
the executable, collector, behavior and guardrails remain identical. The adjusted
review is preserved in `target/launch-supervision-r21-r2/review.json`.

## Expected timeline and operating constraints

Approximate times in America/Denver, subject to actual observed phase boundaries:

- Sampling suppression ends October 29 around 00:24–00:25 MDT.
- After 30 minutes of warmup, measured collection begins around 00:54–00:55 MDT.
- The 24 measured hours finish October 30 around 00:54–00:56 MDT, followed by shutdown
  and evidence review. The collector's observed boundaries are authoritative.

Keep Docker stopped and the PC awake. Do not modify the frozen package, collector,
configuration, live data, network settings or ports. During collection, use source
editing, review and lightweight deterministic checks only; defer builds, browser
suites, installations, large fixtures and database maintenance.

The collector enforces the reviewed disk/log/resource/progress guards and protected
shutdown. It is a background process, not a scheduled service: a reboot or loss of
its user session may interrupt the run. No automatic restart is configured.

For an intentional early stop, create an empty `stop.request` in this run's evidence
directory. Let the collector perform shutdown and inspect `result.json` afterward;
do not kill a PID based solely on this document. An early stop is an interrupted
run, and the directory must never be reused.

Unix updater and pinned Linux visual acceptance remain separate release gates.
No performance improvement or completed baseline is claimed from this launch.
