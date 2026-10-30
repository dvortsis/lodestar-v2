# CR-0 aggregate checkpoint evidence

Captured through October 29, 2026 at 17:02:59 MDT / 23:02:59 UTC. See the
[interpretation and owner-confirmed disk diagnosis](../../2026-10-29-native-cr0-checkpoint.md).

- `aggregate-evidence.zip`: fixed copy of 1,347 per-minute metrics files,
  `samples.jsonl`, 8,075 disk observations, baseline-start metrics, collector
  events, status, and reduced run provenance. Metrics labels were checked against
  fixed enums and numeric tracker IDs. The private data path is omitted from run
  settings; no application logs, credentials, database or executable are included.
- `manifest.json`: SHA-256 and byte size for every archive member, archive SHA-256,
  source-run identity, cutoff and observation counts.
- `analysis.json` and `analysis.md`: full offline analyzer output for this fixed
  snapshot, including all phases, per-hour counters, failure reasons and bounds.
- `checkpoint.json`: compact resource, coverage and yield summary.

To reproduce, extract the archive into a new temporary directory outside the live
run and invoke `python docs/soak/tools/analyze_cr0.py <extracted-directory>`.
Use `--format json` for structured output. Observation freshness will change when
rerun later; counters and coverage describe the saved snapshot. No `result.json`
is included because collection was still active. Never extract into a live run.

The earlier 16:55 assessment and read-only directory-size measurements are recorded
in the report. All original local evidence remains preserved and continues growing.
