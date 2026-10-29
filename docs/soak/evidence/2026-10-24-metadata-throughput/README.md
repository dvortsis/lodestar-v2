# Metadata throughput evidence, 2026-10-24

Supports the [investigation report](../../2026-10-24-v0.5.0-metadata-throughput.md). The running soak was left unchanged. These files are diagnostic records, not application changes or a test-suite addition.

| File | Meaning |
|---|---|
| `h00.prom` through `h13.prom` | Copies of the supervisor's existing hourly snapshots; `h00` is approximately the first minute. |
| `samples.csv` | Copy of the existing supervisor samples through 22:08:06 MDT; 802 rows. |
| `summary-events.log` | Only aggregate `dht.harvest_summary` and `fetch.failed_summary` lines from the existing stderr log at capture. No startup token or identifying peer/torrent details. |
| `live-start.prom`, `live-end.prom` | Read-only `/metrics` captures at the times in matching `*-time.txt` files (UTC). |
| `analysis.json` | Recomputed historical totals/deltas, process sample statistics and live-window deltas. |
| `analyze.py` | Reads these saved files only; prints JSON. Python standard library, no dependencies. |
| `lookup-progress-probe.rs` | Includes the actual current `Lookup` source; simulates immediate send failures using the same transitions as the actor. No sockets or application process access. |
| `lookup-progress-output.txt` | Successful diagnostic run demonstrating the no-progress state. |

Recalculate the analysis from the repository root:

```powershell
python docs/soak/evidence/2026-10-24-metadata-throughput/analyze.py
```

The diagnostic was compiled using already-present release libraries, without running Cargo or changing application files. Exact command from the repository root:

```powershell
$deps = (Resolve-Path target/release/deps).Path
$probeOut = Join-Path $env:TEMP 'lodestar-soak-lookup-probe.exe'
rustc --edition=2024 -L "dependency=$deps" --extern "lodestar_dht=$deps\liblodestar_dht-051ddf7b0bb2ba08.rlib" --extern "lodestar_metainfo=$deps\liblodestar_metainfo-0790112b7f5c69e3.rlib" docs/soak/evidence/2026-10-24-metadata-throughput/lookup-progress-probe.rs -o $probeOut
& $probeOut
```

Artifact hashes in these library filenames are specific to the existing local build. The included `lookup.rs` is read from the repository at compile time. Output confirms zero outstanding requests and fresh unvisited candidates after all three sends fail; a manual second drive finds more work. The actor's lack of an automatic second drive/deadline was established by source inspection. This is **not** a full actor regression test, an attachment to the live process, or proof of its exact stalled-slot count.

Process identification and source provenance are documented in the report. The supervisor launch script was inspected but not executed or changed. Authenticated API access was not available; no credentials were read or modified. The existing private log directory was not readable; aggregate stderr and Prometheus captures were sufficient for the recorded findings.
