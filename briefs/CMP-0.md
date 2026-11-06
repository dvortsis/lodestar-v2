# CMP-0: Crate lodestar-companions and the catalogue: format, compiled-in v0.6 entries, validation, trust root, adapter traits, closed template input types
**Model:** Opus, with `/security-review`. The runner reviews the result with a fresh Opus reviewer. The plan marks this slice "2R", so a second, independent review follows before merge. Runner brief, 2026-11-06T17:15Z. It launches into the slot X-consent-gate-052 freed (#537 merged), within owner §2.42's four slots. It's the root of the companions track.

## What to build
- **The plan:** `docs/plan/2026-11-06-v06-companions.md` §4, the CMP-0 row (about line 58), its slice notes and Q1/Q15. It's the spec; read the whole plan. The architecture sections it cites are §13, §12, §18.10 and `docs/owner-decisions.md` §1.7.2.
- **Its `runner/slices.yaml` entry (`CMP-0`), notes included:**
  - `/security-review`;
  - an unanchored second read;
  - the catalogue override is allowed only under `cfg(test)` or the `testing` feature;
  - a one-line shared edit to `[workspace.dependencies]` in the workspace `Cargo.toml`.
- **Owner decisions that bind it:**
  - **§2.46, cq1 (a):** the catalogue is compiled into the binary, so the release signature covers it. The plan's caveat about first installs stands.
  - **§2.46, cq2:** the SHA-256 pin is the run-time gate. OpenPGP is checked at pin time by CMP-13; that's not this slice.
  - **§2.47, cq6:** whose binaries run: userdocs static `qbittorrent-nox`; Prowlarr from upstream; linuxserver.io images over hotio; gluetun and FlareSolverr from their own projects. All are pinned.
  - **§2.47, cq3/cq4/Q21:** FlareSolverr is opt-in only and not installed by default.

  Where a plan row decided a question, the plan row is the decision.
- **The catalogue is untrusted structure even though it's compiled in.**
  - Validate everything: hosts against the allowlist, hash formats, versions, and closed input types for templates.
  - No panics.
  - A test that a malformed catalogue fails loudly at build or test time.

## Own
- `crates/companions/Cargo.toml`, `README.md`, `src/lib.rs`, `src/log_events.rs`, `src/catalogue.rs`, `src/adapter.rs`, `src/inputs.rs`, `data/catalogue.json`, `tests/catalogue.rs`.
- The workspace `Cargo.toml` members line and one `[workspace.dependencies]` line, plus the `Cargo.lock` entries cargo generates for the new crate. Never edit the lock file by hand.
- A decisions file, `docs/decisions/2026-11-06-CMP-0.md`, and its own `runner/slices.yaml` entry (`owns` only).

**Running beside it:** WK-0, WK-6 and ENR-9, with disjoint `owns`. If you need a file outside your `owns`, message the runner before touching it.

## Done when
- These pass:
  - `cargo fmt --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test` for the new crate and anything that depends on it;
  - `cargo deny`, or the repo's licence/advisory check, if CI runs one for new dependencies;
  - `cd tools/status-connector && npm test`;
  - `/security-review`.
- No fixed sleeps.

Branch `slice/CMP-0`. Open the PR titled "CMP-0: <a short imperative title, under 60 characters>". Follow `.claude/skills/steward/SKILL.md`. Never merge. Push once per round. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the `send_message` tool of the Claude Code Remote MCP server.
