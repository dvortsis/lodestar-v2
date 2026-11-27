# X-claims-refile-rearm: A refile or terms change takes a held-back title out of the record promptly

**Runner brief, 2026-11-27T10:52Z.** Core, Opus (store and pipeline, concurrency). Target < 400 changed lines. `/security-review` before you report ready.
- **Branch:** `slice/X-claims-refile-rearm` from fresh `origin/main` (with #1135, X-purge-index-shapes, merged as 3bfe7246).
- **PR title:** `X-claims-refile-rearm: A refile or terms change takes a held-back title out of the record promptly`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. A fresh Opus review gates the merge.
- **Add this slice's own `runner/slices.yaml` entry** (kind core, model opus, deps [X-purge-index-shapes], your owns, notes "runner 2026-11-27T10:52Z, from #1135's review") and run `cd tools/status-connector && npm test`. Edit nothing else in that file or in `runner/dashboard.yaml`.

## Why
#1135 made every path that removes a served identification answer log the torrent for the index in the same transaction, so ids and alternative titles leave search at once. Its review found three remaining gaps where a held-back answer's **canonical title** (ENR-10: the effective title stored in the record) can stay searchable:
1. **A refile doesn't re-arm the writer.** `ClaimsRefiled::rederive` is ignored at `crates/pipeline/src/unverified.rs:1462` and `:1231`. If the backfill already passed that id, the title stays in the record until the next restart, purge or version change. The fix is about one line: `if done.rederive { reenrich.store(true) }`, at both sites if both apply.
2. **`drop_one`** (retention expiry and the "no match" refresh, `crates/store/src/unverified.rs`) **and `mark_claims_unenriched`** (a terms change that stops redistribution) **don't bump `class_gen`.** A backfill batch on another connection that read the old answer can write its canonical title back over the mark.
3. **`mark_claims_unenriched` doesn't log `reindex_log`**, and it doesn't re-arm the backfill it relies on.

Both store paths are inert today, since no host calls `record_terms` or `sweep_recorded` yet. Fix them now, before something wires them.

## Do
1. Re-arm the writer when a refile reports `rederive`.
2. Bump `class_gen` in `drop_one` and `mark_claims_unenriched`, in the same transaction as their change, the way the purge and refile paths do.
3. Have `mark_claims_unenriched` log the affected torrents to `reindex_log` in its transaction, through `shaped_or_unreadable`. Re-arming the backfill is the caller's job: name the caller and do it there, or return what the caller needs.
4. **Tests.**
   - A refile with `rederive` re-arms, and the canonical title leaves the record and the index without a restart.
   - A backfill batch that read an answer before `drop_one` or `mark_claims_unenriched` is rejected by `class_gen` and can't write the title back. Use an interleaving test on two connections, waiting on conditions with deadlines of at least 10 s; no fixed sleeps.
   - `mark_claims_unenriched` logs, and a failed log rolls back.
   - Mutate each fix and show a test fails.
5. Also fix #1135's nits: the decisions heading `# X-purge-index-shapes decisions (#1135)` (one line in `docs/decisions/2026-11-27-X-purge-index-shapes.md`), and `identify_sweep.rs:153`, which decodes the details twice.

## Owns
`crates/store/src/unverified.rs`, `crates/store/src/claims.rs`, `crates/store/src/identify_sweep.rs`, `crates/store/tests/unverified.rs`, `crates/pipeline/src/unverified.rs` and its tests, `crates/pipeline/src/control.rs` (only if the caller is there), `crates/search/tests/enrichment.rs` (for an end-to-end index check), `docs/decisions/2026-11-27-X-purge-index-shapes.md` (heading only), `docs/decisions/*-X-claims-refile-rearm.md`, and this slice's `runner/slices.yaml` entry.

**Not yours:**
- `crates/search/src/**` and `crates/api/**` (X-import-verified-no holds them);
- `crates/dht/**`, `crates/lodestar/src/config.rs`, `metrics.rs` and `docs/metrics.md` (CR-0w);
- `crates/integrations/**`.

## Done
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test -p lodestar-store -p lodestar-pipeline -p lodestar-search`, or the workspace if the disk allows. Say which you ran. Report `ready <n> <full sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) once ci-ok, web-ok and e2e-ok are green. Push once per round of fixes.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
