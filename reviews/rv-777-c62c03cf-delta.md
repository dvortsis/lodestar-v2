# #777 M8-WIN-1: delta self-check 908a48c6..c62c03cf (runner)

**Verdict: MERGE.**

- I1 fixed: the decisions file, runbook and "what to send back" route the exe hash through the RC portable zip checked against the signed SHA256SUMS; from_image_sha256 the same against the last release's zip.
- I2 fixed: the Update pause is 1 week (the run is about 32 h plus phases) and Phase 5 resumes updates.
- I3 fixed: the producer tests use real temp files and folders (same and different binaries, empty and missing data folder); only service and process calls are stubbed. Cleanup removes only what the test created.
- O2: list-valued start ticks are JSON-encoded so they stay hashable; a non-string bound hash fails build_binding. Test added.
- O4: Save-SoakUpgrade validates a 64-hex previous hash and positive start ticks and torrent count; three rejection cases tested.
- O1 left for a follow-up (X-m8-win-1-o1).

ci-ok, web-ok, e2e-ok green on c62c03cf; merge-tree clean; no file overlap with main since the base.
