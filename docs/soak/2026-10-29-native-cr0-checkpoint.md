# Native CR-0 checkpoint — October 29, 2026

Status: **running; partial baseline; acceptance pending**. This report records the
17:02:59 MDT snapshot (23:02:59 UTC), plus the earlier 16:55 diagnostic assessment.
The owner requested preservation and publication of results, diagnostics and all
pending source changes. Publication does not establish release or soak acceptance.

## Run and evidence

- Source: `744b84d68c7609d8220430ffc9938c05c6d6a527`, frozen r21 package.
- Executable SHA-256: `24CB1EF43D86AD8C0F9F80C41E9D2D661F3FD6FE5573E72C13A87285E11E1475`.
- Run: `target/soak/cr0-native-2026-10-28-744b84d6-r2` from the repository root.
- Crawler PID 49888 and collector PID 98244 were both alive at the diagnostic check.
- Started October 28 at 18:23:56 MDT; suppression ended October 29 at 00:24:56;
  measured baseline started at 00:55:11 MDT.
- Expected end: October 30 at 00:55:11 MDT, subject to actual collector evidence.
- [Launch and preserved first-attempt details](2026-10-28-native-cr0-launch.md).
- [Checkpoint evidence and reproduction](evidence/2026-10-29-cr0-checkpoint/README.md),
  [full analysis](evidence/2026-10-29-cr0-checkpoint/analysis.md) and
  [machine-readable analysis](evidence/2026-10-29-cr0-checkpoint/analysis.json).

The snapshot contains 1,347 samples (sequence 0–1,346), 8,075 disk observations,
and every corresponding aggregate metrics file. Baseline coverage is 16.130 hours,
67.21% of the required 24 hours. No completion result or clean shutdown exists yet.
The analyzer's `completion_unknown` is an intentional acceptance limitation;
the saved status and live process observation establish that this run was active.

## Observed results

| Observation | Result |
| --- | --- |
| Health / required metrics failures | 0 / 0 |
| Collection issues / disk read failures | 0 / 0 |
| Sampled capacity violations | 0 across peer requests, sampler states, frontier, RPCs and pool lookups |
| Writer / search index errors | 0 / 0 |
| Stored torrents, whole run | 12,171 |
| Stored during baseline | 11,574; 717.54/hour |
| Baseline verified metadata fetches / attempts | 11,606 / 270,342; approximately 4.29% |
| Baseline completed peer lookups | 74,880; 4,642.27/hour |
| Baseline actor admission rejections | 63,697,589; 63,697,567 capacity, 22 caller closed |
| Baseline command-channel full events | 32,754,009 |
| Baseline unaccepted / retry-unaccepted events | 96,451,059 / 92,838,608 |
| Baseline pool evictions | 2,209,749 |
| Evictions before accepted lookup | 2,185,357; 98.90% of evictions |
| Other eviction stages | lookup 4,828; peers 0; fetch 19,564 |
| Baseline fetch failures by kind | timeout 192,613; unreachable 37,087; refused 28,965; invalid 74; oversize 6; encryption 0 |
| Current / peak working set | 156.64 / 535.99 MiB |
| Current / peak private bytes | 514.76 / 613.43 MiB |
| Free disk at checkpoint | 61.26 GiB |

At the earlier 16:55 check, 12,101 torrents were stored, the baseline rate was
719.2/hour and the last-hour rate was 558.2/hour. The preceding six-hour rate was
741.4/hour. These windows show variability; one slower hour is not evidence of a
regression. Private memory was approximately 513 MiB at baseline start and
525 MiB at that check. Working set fluctuated; there is no obvious runaway trend,
but endpoint differences cannot establish the absence of a leak.

Admission and retry counters count repeated internal events, not unique hashes or
network requests. The eviction stage records the furthest observed service before
each eviction; a hash can return later. Discovery is outrunning admission and most
evictions occur before service. This supports investigating capacity-aware
admission, retry pacing and retention fairness. It does not prove a particular
fix, unique-hash starvation, or the benefit of increasing queue limits.

The sampler and frontier reached their configured limits without exceeding them.
Capacity equality is expected backpressure, not an ownership violation. Stored
counts are not independently verified searchable yield. The earlier metric sample
at 16:53 had 12,091 stored and 12,090 indexed, but no cohort latency claim follows.

## Disk diagnosis and owner correction

Free space fell from approximately 116 GiB at launch to 61 GiB. The collector
recorded projected-reserve warnings between 09:53:46 and 10:03:32 MDT, based on
the recent depletion rate. The minimum observed reserve was about 60.51 GiB;
the configured stop reserve is 10 GiB. No disk stop occurred at this checkpoint.

Read-only file-size inspection around 16:56 MDT found 43,040,256 bytes in the
private executable directory, 89,293,203 bytes across application data, and
29,063,797 bytes of metrics, plus small root evidence files: approximately
157 MiB for the whole run. Application logs were tiny; the SQLite database and
WAL were approximately 27.6 MB and 22.4 MB. This does not explain the host's
roughly 55 GiB reserve decrease.

**The owner confirmed that another application's download caused the disk-space
change and that the download process has been discontinued.** This supersedes
the earlier unknown-cause note. It does not retroactively identify the cause of
the separate October 27 disk-exhaustion incident. No cleanup, crawler tuning or
restart was performed. No sustained post-download disk trend has yet been measured.
The download is also a possible host/network contention confounder when comparing
throughput; its exact start/stop and resource impact were not measured here.

## Follow-up and limits

Keep the frozen crawler, collector, settings, data and ports unchanged through
completion. Finish with endpoint and clean-shutdown evidence before acceptance.
Prepare admission/retry and fairness work separately; defer heavy builds and
experiments on this host until the run completes. Compare later candidates under
matched configuration and account for shared-host activity. Current evidence
does not prove a speedup, per-request latency, exactly-once cleanup, or Linux,
Docker or Unraid behavior. Existing deterministic and cross-platform gates remain.

Raw application logs, credentials, databases and executable files remain private.
Only aggregate diagnostic evidence is included in the published checkpoint.
