# Runner delta self-check: PR #778 (X-test-blockers-1112), 3d75e57d -> 4502d2de

VERDICT: MERGE (on green ci-ok, web-ok, e2e-ok; test (windows-latest) must pass the portable restart test, I1)

Delta: a clean merge of origin/main (19f2a5cc), then 4502d2de (3 files, +16/-4):
- **B1 fixed:** `Fixture::new` runs `ANALYZE pg_catalog.pg_database` right after CREATE DATABASE, with a comment giving the reason. `quiet()` and `QUIET_SQL` are unchanged; this is a test-only change.
- **Decisions file:** records both triggers, the fixture tables and the shared catalog, with the measured runs (0/80 with both fixes).
- **Optional:** the leak-test.sh header describes `skip` as a check that can't be judged, never a pass.
- **I1:** confirmed from this head's Windows run, at merge.
