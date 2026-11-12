# UP-0: .torrent upload through one raw-body route (Opus, core: untrusted input, /security-review)

**Why:** the first live upload surface for v0.7. UP-1 (folder watch) and UP-2 build on it. Spec:
- `docs/plan/2026-11-05-v07-m7.md` §5, "The `.torrent` upload exception", and §4 "UP-0";
- `docs/architecture.md` §3 (hard truths), §12 and §18.7;
- owner §2.37: the exception goes with the runner's recommendation. If what's built differs from it, it goes back to the owner.

## Step 1, before any code: restate the exception and wait for the runner
Write `docs/decisions/2026-11-12-UP-0.md`, restating the plan's §5 terms as you'll build them:
1. **One route,** `POST /api/v1/torrents/upload`, outside the JSON extractor. **Raw body, not multipart:** `Content-Type: application/x-bittorrent` exactly, else 415. No multipart parser, filenames or form fields.
2. **The same guard as JSON writes,** whatever authenticates a JSON write on that install, plus the same-origin rule (`same_origin_json`, `extract.rs` ~194). **API keys can't upload;** no scope has it.
3. **Bounded:**
   - a body cap of 8 MiB, read with a length limit before parsing;
   - `TorrentFile::parse_with`'s limits;
   - one file per request;
   - **its own rate-limit bucket,** not the login limiter's. State the number; it's a §2.26 number.
4. **Nothing stored but what a BEP 9 fetch would store,** plus the out-of-info-dict claims. The raw file is dropped unless the info-dict archive setting is on.
5. **The same judgement as any metadata:** SRC-13b's verified-file path, floor hash list first, then the full pipeline. A held verdict is never stored.

Commit it on your branch, push, and message the runner "UP-0 step 1 <sha>". **Write no code until the runner confirms.** Name any difference from §5 plainly: those go to the owner.

## Build, after confirmation
- **Parse** with `metainfo::TorrentFile::parse_with`, bounded. Derive the info hash from the raw info-dict bytes.
- **Judge** exactly as SRC-13b's verified file: `pipeline/src/torrent_cache.rs` `Feed` → `judge_sealed` → the writer's lease-free entry. The provenance is "upload".
- **Out-of-info-dict keys become claims:**
  - `announce` and `announce-list` through X-claim-trackers' guard;
  - `creation date`, `created by` and `comment`, which the core judges like any claim text (stricter only);
  - `url-list` is dropped (§18.7: never fetched).
- **A torrent whose info dict has `private=1` is `private_source`** (BEP 27: its hash must never reach the DHT). Test that it never reaches any DHT-facing path.
- **Errors name the cause** (too large, wrong type, malformed, refused, held) **without echoing names** or other text from the file.
- **Tests** (`crates/api/tests/upload.rs`, `crates/pipeline/tests/local_torrent.rs`):
  - 415 for every other content type, multipart included;
  - the cap, refused before parsing;
  - auth and same-origin refusals: cross-site, no session, an API key;
  - the rate bucket is separate from login's;
  - v1, v2 and hybrid torrents;
  - `private=1`;
  - floor-blocked and held verdicts never stored;
  - the archive setting off and on;
  - claims kept and `url-list` dropped;
  - malformed and fuzzed bodies never panic.
  - Add a fuzz target or seeds if the parse entry point differs from metainfo's existing ones.
  - No fixed sleeps.
- **`/security-review`** on the diff before the PR.

## Owns
- `crates/api/src/upload.rs`, `crates/api/tests/upload.rs`
- `crates/pipeline/src/local_torrent.rs`, `crates/pipeline/tests/local_torrent.rs`
- its decisions file

**Shared one-line edits allowed:** the route registration and `mod` line in the api router or `lib.rs`; the `mod` line in `crates/pipeline/src/lib.rs`; `docs/metrics.md` rows for any new metric; and the OpenAPI spec entry for the route, if the repo keeps one. Ask the runner before anything else. **Never edit `extract.rs` or `writer.rs`** (plan rule).

## Checks
- `cargo fmt --check`
- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`
- `cargo test -p lodestar-api -p lodestar-pipeline` (the workspace if disk allows)

Branch `slice/UP-0`; PR title "UP-0: .torrent upload through one raw-body route". The plan's estimate is ~900 lines. Follow the steward skill, including a fresh self-review before the PR. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) "ready <pr> <sha>" when CI is green. A fresh Opus reviewer with security items reviews it before merge.
