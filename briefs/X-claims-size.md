# X-claims-size: Identification answers fit the store's size budget

**Runner brief, 2026-11-27T12:51Z.** Core, Opus (an on-disk format, the claims blob, and the store's schema). Target < 900 changed lines. A store migration is allowed only after the runner agrees it (step 2). `/security-review` before you report ready.
- **Branch:** `slice/X-claims-size` from fresh `origin/main` (with #1138 merged as 10ddc7ea).
- **PR title:** `X-claims-size: Identification answers fit the store's size budget`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do. A fresh Opus review gates the merge.
- **Add this slice's own `runner/slices.yaml` entry** (kind core, model opus, deps [X-enr11-size-measure, X-claims-refile-rearm], your owns, notes "runner 2026-11-27T12:51Z, §2.26: shrink the claims before any budget moves") and its one placement line in `runner/dashboard.yaml` next to X-claims-refile-rearm. Run `cd tools/status-connector && npm test`. Edit nothing else in either file.

## Why
`docs/decisions/2026-11-27-X-enr11-size-measure.md` measured the store with identification answers. At the realistic 60 % share (54.7 % of the corpus), `size-report --torrents 50000 --identified 60` gives **1114.4 B/torrent against the 976 B budget**, 138 B over:
- `torrent_claims` takes 145.5 B per torrent, about 266 B per answer with page overhead;
- its three indexes take about 24 B;
- rows are zstd-compressed one at a time with no dictionary.

Database size is the owner's second priority, so the runner decided (§2.26) to shrink the data before any budget moves. The torrent-index overrun (613 B against 560) is **not** this slice's: it's in `crates/search`, which X-import-verified-no holds. The runner will plan it separately.

## Do
1. **Measure each lever first, alone and together,** with `size-report --torrents 50000 --identified 0,60,100`:
   - **(a) A trained zstd dictionary for claims blobs,** as the file trees already have. Find how the file-tree dictionary is trained, stored and versioned, and reuse that machinery.
   - **(b) Fewer stored alternative titles:** the claim keeps up to 32 and the index uses 8. Check what reads the others (the drawer? an API? ENR-10's matching?) before cutting. Never drop a title that something shows or uses.
   - **(c) The claims indexes:** `torrent_claims_source`, `torrent_claims_stored` and the third. Is each needed? Could a narrower one or `WITHOUT ROWID` do its job? Check every query that uses them, with `EXPLAIN QUERY PLAN` before and after.
   - **(d) Anything else the numbers show,** such as encoding the ids compactly.
2. **Message the runner with a table:** bytes per torrent at 0/60/100 % for each lever and the combination, the write and read cost of each, and whether it needs a migration. Then **wait for the runner's choice** before you build a format change or a migration. A format change needs a version byte or flag so old blobs still read, a migration slot, and a test that a store written by `main` opens, reads and rewrites correctly.
3. **Build the chosen levers.**
   - Reads must accept every blob `main` writes.
   - Writes produce the new form.
   - Decide whether a background rewrite of old rows is worth doing, and justify it in the decisions file.
4. **CI's gate:** if the store at 60 % is now within 976 B, move CI's `size-report --gate` to the 60 % share (`ci.yml`, one flag). Otherwise leave it at 0 % and report what's left; the runner decides any budget change. Ask the runner before touching `ci.yml`.
5. **Tests:**
   - Round-trip every claim shape through the new encoding.
   - Old blobs still decode.
   - A dictionary-version mismatch fails closed, never decoding with the wrong dictionary.
   - The claims indexes' queries keep their plans.
   - A property test or fuzz target for the decoder, since it parses stored bytes. Add a seed to `fuzz/` if one exists for claims.
   - No fixed sleeps.

## Owns
`crates/store/src/claims.rs` and its tests, `crates/store/src/unverified.rs` (only where it writes or reads claims blobs), the store's dictionary module (name it in the PR), `crates/store/src/schema.rs` (only if the runner agrees a migration), `crates/store/tests/**` claims tests, `bench/src/bin/size-report.rs` and `bench/src/synth.rs` (fixes for the #1136 nits: route answers through `Identification::into_claims`; `TERMS_REDISTRIBUTE` from `Terms{redistribute:true}.record()`), `docs/decisions/2026-11-27-X-enr11-size-measure.md` (only the "default share" headings, reworded to "realistic share"), `fuzz/**` for a claims target, `docs/decisions/*-X-claims-size.md`, and this slice's yaml lines.

**Not yours:** `crates/search/**` and `crates/api/**` (X-import-verified-no), `crates/dht/**`, `crates/lodestar/src/config.rs`, `metrics.rs` and `docs/metrics.md` (CR-0w), `crates/lodestar/src/watch.rs` (X-watch-open-fstat), and `ci.yml` until the runner says.

## Done
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test -p lodestar-store -p lodestar-pipeline -p lodestar-bench` (or the workspace if the disk allows; say which). Put the size table in the PR description. Report `ready <n> <full sha>` to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) once ci-ok, web-ok and e2e-ok are green. Push once per round of fixes.

No AWS or cloud calls, no new accounts, no spending, no force-push. Held security detail stays off every branch.
