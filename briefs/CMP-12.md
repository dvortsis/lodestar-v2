# CMP-12: FlareSolverr, opt-in only (owner Q21)

**Runner brief, 2026-11-12 (UTC).** Opus, core, `/security-review`. About 400 lines with tests.
- **Branch:** `slice/CMP-12` from fresh `origin/main`.
- **PR title:** `CMP-12: FlareSolverr per owner Q21, opt-in only: catalogue entry, detection, Prowlarr proxy setting`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## The decision
The owner answered Q21 yes, opt-in only (`docs/owner-decisions.md` §2.47; plan row Q21 in `docs/plan/2026-11-06-v06-companions.md` §5, which is the decision in full). From the plan, line 168:
- FlareSolverr's catalogue entry is marked opt-in.
- Its detection and health adapter.
- Lodestar sets it as Prowlarr's indexer proxy only after the user switches it on for Prowlarr.
- It is **never installed, detected into use or wired by default.**

## Read first
- The companions plan: §4's CMP-12 line, §5 Q21, and the shared-file notes at lines 133 and 149.
- `docs/decisions/2026-11-11-X-cmp-egress-prelaunch.md` lines 38–43: a FlareSolverr pairing leaves directly, not through the route. Keep that note true, and surface it wherever this slice shows the pairing.
- The existing adapters as the pattern: `crates/companions/src/detect/prowlarr.rs`, `configure/prowlarr.rs`, `detect/mod.rs`, `configure/mod.rs`, and the catalogue loader `catalogue.rs`.
- architecture §18.10 and §18.9; `runner/standards.md`.

## What to build
1. **The catalogue entry** in `crates/companions/data/catalogue.json`, marked opt-in.
   - It's pinned and checksum-verified like the others. Use CMP-13's pin tool, or put a placeholder pin with a clear `TODO(CMP-13)`, never an unverified download.
   - Listed under Prowlarr's options, never offered in the default install set.
2. **Detection and health** (`flaresolverr.rs`):
   - find a running FlareSolverr, whether ours or an existing one, by its health endpoint;
   - bounded timeouts and a response size cap;
   - its reply is untrusted input: parse strictly and never panic.
3. **Wiring:**
   - The Prowlarr indexer-proxy setting is written only when the user has switched FlareSolverr on for Prowlarr.
   - It's undone when they switch it off.
   - It's idempotent, and it never touches a proxy the user made themselves (same name or tag rule as the Prowlarr adapter uses).
4. **Safety:**
   - FlareSolverr's port is bound to localhost or the companion network only;
   - no credentials in logs;
   - the egress note above shown where the pairing is described (a reason code; no new UI text in this slice beyond codes).

## Owns
- `crates/companions/src/flaresolverr.rs` and `crates/companions/tests/flaresolverr.rs`
- `crates/companions/fixtures/flaresolverr/**`
- `docs/decisions/2026-11-12-CMP-12.md`

**Shared, small:** the `catalogue.json` entry, the registrations in `detect/mod.rs` and `configure/mod.rs`, and one `mod` line in `lib.rs`. Ask the runner before anything else.

## Tests
- Off by default: a fresh install set has no FlareSolverr, and Prowlarr gets no proxy.
- Switched on: the proxy is written once.
- Switched off: the proxy is removed. A proxy the user made is untouched.
- Detection:
  - healthy;
  - unhealthy;
  - timeout;
  - oversized and malformed replies, plus a property or fuzz-style test over random bytes (no panic, bounded memory);
  - the catalogue entry loads and verifies;
  - the localhost binding holds.
- No fixed sleeps (waits are conditions with ≥ 10 s deadlines). No tokio `test-util` or `testing` feature on `lodestar`.

## Done
- `cargo fmt --check`;
- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`;
- `cargo test -p lodestar-companions`.

Name your refinements in the PR description. Send the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green. A fresh Opus `/security-review` follows.

**Session window (owner, 2026-11-12):** sessions are cut back at 00:00Z on 2026-11-13. If the runner keeps this session after that, carry on. If not, push your work in progress to the branch, open the PR as a draft, and report its state.
