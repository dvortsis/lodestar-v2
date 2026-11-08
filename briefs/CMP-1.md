# CMP-1: Companion fetch and verify (pinned download, host allowlist, byte cap, streaming SHA-256, upstream signature)

**Model:** Opus. **Review:** a fresh Opus reviewer, plus `/security-review` and an unanchored second read (2R). The slice fetches untrusted bytes from the network. **Branch:** `slice/CMP-1`, cut from `origin/main`. **PR title:** `CMP-1: <title>`. **Steward:** follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Read first
- `docs/plan/2026-11-06-v06-companions.md`:
  - §4's CMP-1 bullet;
  - §5 Q2: the SHA-256 pin is the run-time gate; OpenPGP is checked at pin time by CMP-13, not here; minisign upstream signatures, if any, are checked at run time with `lodestar_update::minisign`;
  - §5 Q16: downloads use Lodestar's own route through the egress settings and the SSRF guard, never direct. The cap is the pin's size, plus a hard 512 MB.
- `docs/owner-decisions.md`, the companions answers (cq1, cq2) and §2.23.
- `docs/architecture.md` §12.2 ("Checking", "Downloading") and §18.10.
- **`crates/lodestar/src/update/download.rs` is the model to follow:** its rules, its limits (`OVERALL`, `IDLE`), the writer thread, `.part` handling, and its tests. Also `check::hop_allowed` and `MAX_HOPS`, and `lodestar_integrations::net::{GuardedClient, NetPolicy}`.
- `crates/companions/` as it is on main: `catalogue.rs` (per platform `{url, size, sha256, archive, upstream_sig?}` and the per-entry host allowlist), `credentials.rs`/`config_files.rs` (CMP-3's file-handling patterns) and `inputs.rs`.
- `runner/standards.md` is the bar.

## Owns
- `crates/companions/src/fetch.rs`
- `crates/companions/src/verify.rs`
- `crates/companions/tests/fetch.rs`

**Shared edits the runner allows:**
- one `mod` line each in `crates/companions/src/lib.rs` (plus `pub use` if needed), and new events in `crates/companions/src/log_events.rs`. An event carries no URL query string and no redirect URL. If an event needs web message keys, use none and return error codes instead (CMP-2 did the same);
- `crates/companions/Cargo.toml`: dependencies already in the workspace (`reqwest` via `lodestar-integrations`, `sha2`, `lodestar-update` for minisign, `tokio`, and `rustix` if you want `O_NOFOLLOW`; it's a workspace dependency at 1.1.5). No new crate outside the lockfile without asking the runner;
- `Cargo.lock`;
- one `docs/decisions/2026-11-08-CMP-1.md`.

CMP-2 (#600) also adds lines to `crates/companions/Cargo.toml`, `lib.rs` and `Cargo.lock`. Whichever PR merges later brings `main` in; that's a trivial conflict, not an owns overlap.

If you find the right way is to reuse code from `crates/lodestar/src/update/` (for example, moving `hop_allowed` into a shared crate), tell the runner first. `crates/lodestar` isn't yours, and `crates/companions` must not depend on `crates/lodestar`.

## What to build
1. **Fetch (`fetch.rs`).**
   - **Where from:** the catalogue pin's URL, `https` only.
   - **Redirects:** every redirect hop must be `https` and on the entry's host allowlist, at most `MAX_HOPS` (the same number as the updater). Never log a redirect's URL; GitHub's signed asset URLs carry a query.
   - **Route:** take a `GuardedClient` (or the same `NetPolicy` and egress route the updater uses) handed in by the caller. Never build a direct client. The SSRF guard applies to every hop.
   - **Request:** `User-Agent: Lodestar-Companions`. No cookies, no referer, nothing that identifies the installation.
   - **Where to:** `<data>/companions/downloads/<id>-<version>.part` (or similar under `<data>/companions/`), created new with no-follow and 0600.
     - Stale `.part` files for that id are removed at the next download.
     - One download per companion at a time.
   - **Limits:**
     - bytes are counted as they arrive; one byte past the pin's size, or past the hard 512 MB, stops the download;
     - an `OVERALL` and an `IDLE` timeout, as in `download.rs`, sized for 512 MB;
     - hashed (SHA-256) as it streams.
   - **On completion:** fsync, then the size and the SHA-256 must equal the pin. Anything else removes the file and returns a typed error.
   - **On success:** return a handle to the verified file for CMP-2's extractor (CMP-5 wires the two together).
2. **Verify (`verify.rs`).**
   - The SHA-256 and size check, with constant-time comparison where it matters. Hashes aren't secret, but don't leak timing needlessly.
   - **Upstream minisign** (Q2): if a pin carries a minisign `upstream_sig` with a pinned public key, verify it at run time with `lodestar_update::minisign`. An entry with OpenPGP-only upstream signatures is covered at pin time (CMP-13), and nothing is checked here beyond the SHA-256.
   - Record in the decisions file what the catalogue's `upstream_sig` field needs to carry for this, in line with CMP-0's format. If the format can't express a minisign key, tell the runner rather than change `catalogue.rs`.
3. **Errors** are typed, carry no URL query, and contain no `unwrap` or `expect` outside tests.

## Tests and gates
- **A local TLS test server** (as `download.rs`'s tests do). Cases:
  - wrong hash;
  - wrong size, short and long;
  - one extra byte;
  - a redirect off the allowlist;
  - a redirect to `http`;
  - too many hops;
  - a slow drip that hits `IDLE`;
  - an overall timeout;
  - a `.part` path planted as a symlink;
  - a stale `.part` removed;
  - success.
- **Minisign:** a good signature, a bad signature, and a wrong key.
- **Request contents:** a test that the request headers carry nothing identifying.
- **No fixed sleeps.** Waits are conditions with ≥ 10 s deadlines. Tests run on Windows CI too.
- **Before pushing:**
  - `cargo fmt --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test --workspace`, at minimum `-p lodestar-companions`.
- **Size:** about 600 lines per the plan; stay under 1,500.

## Decisions you may make (§2.33; record each in the decisions file)
- the download folder name;
- the timeouts sized for 512 MB;
- the API shape for CMP-5;
- how the `GuardedClient` and route are handed in.

Anything that loosens a rule above, or changes a Q16 number, goes to the runner.

When CI is green, Windows included, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA, using the Claude Code Remote MCP `send_message` tool.
