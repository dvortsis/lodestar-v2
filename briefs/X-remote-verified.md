# X-remote-verified: A torrent a remote worker reports is indexed verified only if the coordinator checked it

**Runner brief, 2026-11-27T15:36Z.** Core, Opus (search provenance; content honesty; untrusted remote input). Target < 400 changed lines. `/security-review` before you report ready.
- **Branch:** `slice/X-remote-verified` from fresh `origin/main` (with #1139, X-import-verified-no, merged as 10db45b4, and #1140 as ce03a538).
- **PR title:** `X-remote-verified: A torrent a remote worker reports is indexed verified only if the coordinator checked it`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. A fresh Opus review gates the merge.
- **Add this slice's own `runner/slices.yaml` entry** (kind core, model opus, deps [X-import-verified-no], your owns, notes "runner 2026-11-27T15:36Z, from #1139's review") and run `cd tools/status-connector && npm test`. Edit nothing else in that file or in `runner/dashboard.yaml`.

## Why
#1139 made `verified:` honest for imports: `yes` means the info dict was fetched and hashed to the key here. `stored_verified` (`crates/search/src/doc.rs`) returns `Yes` for every row that isn't `IMPORTED`. Rows a remote worker brings in (WK-9, `crates/pipeline/src/remote.rs` about line 921) aren't `IMPORTED`. They carry `REMOTE_UNVERIFIED` instead, which today is a privacy flag: every privacy gate treats the row as private until WK-13 re-verifies it, and send and alerts already refuse it. #1139's review asked whether such a row's metadata is really verified, or only the worker's word.

## Do
1. **Step 0, report before you build.** Read the remote intake path (`crates/pipeline/src/remote.rs`, the worker wire format under `crates/pipeline/src/wire/`, and WK-9 / WK-13 in docs/architecture.md and their decisions files). Answer, with file:line:
   - Does the coordinator receive the raw info dict and hash it to the key (v1 SHA-1 and v2 SHA-256 where it applies) before storing? Or does it store fields the worker parsed?
   - If it hashes it, is that enforced for every remote record, or only when `preparer_archive_raw()` holds?
   - What does WK-13's re-verification check, and does it clear `REMOTE_UNVERIFIED`?
   Then message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with your answer and a proposal. **Wait for the runner's reply before writing code.** The likely outcomes:
   - (a) The coordinator hashes every record: `yes` is right. Pin it with a test that a record whose dict doesn't hash to its key is refused, and document the rule in `stored_verified`'s doc.
   - (b) It doesn't: rows with `REMOTE_UNVERIFIED` index as `imported` (visible, never called verified) until they're re-verified. That needs `stored_verified` to take the flag, plus tests over every flag combination, as `import_verified.rs` does.
2. **Whichever applies:** the API detail and search-hit mapping follow the index value (#1139's `is_import`). Check that a remote row's hit and detail read the same as its index value, and test it.
3. No store migration and no new flag. If you think one is needed, stop and ask.

## Owns
`crates/search/src/doc.rs`, `crates/search/tests/import_verified.rs`, `crates/api/tests/provenance.rs`, `crates/api/src/search.rs` and `crates/api/src/torrents.rs` (only the `is_import` mapping, if it must change), `crates/pipeline/tests/remote_intake.rs` (tests only), `docs/decisions/*-X-remote-verified.md`, and this slice's `runner/slices.yaml` entry. `crates/pipeline/src/remote.rs` is read-only unless the runner grants it after step 0.

**Not yours:**
- `crates/store/src/saved.rs`, `crates/lodestar/src/services.rs` and any new rewrite module in `crates/search/src/` (X-saved-verified-rewrite);
- `crates/lodestar/src/config.rs`, `metrics.rs`, `docs/metrics.md` and `crates/dht/**` (CR-0w).

## Done
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test -p lodestar-search -p lodestar-api -p lodestar-pipeline` (targeted tests if the disk is tight; say which). Report `ready <n> <full sha>` to the runner once ci-ok, web-ok and e2e-ok are green. Push once per round of fixes.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
