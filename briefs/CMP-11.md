# CMP-11: Windows qBittorrent per owner Q4 (cq4): catalogue entry, guided install path, then detection and configuration
**Model:** Opus, with `/security-review` (companions: third-party installer, credentials, config written to another program). A fresh Opus reviewer follows.

Runner brief, 2026-11-11T14:09Z. Slot 1. Epic E-v06-companions, on the path to CMP-G. Deps merged: CMP-5g (5g-1/2/3), CMP-8a, CMP-8b, X-cmp-native-handover; `ext:owner-cq4` holds.

## The owner's answer (cq4)
`docs/owner-decisions.md` ~530: **a guided install for qBittorrent on Windows only, as the one exception to §1.7.2's fully automatic installs.** Lodestar never runs the qBittorrent installer itself. It shows the official download, the pinned version and checksum to check, and the steps; then automatic detection and configuration take over, as for any guided companion.

## Read first
- `docs/plan/2026-11-06-v06-companions.md`: the CMP-11 row (~75), §CMP-5g (~161), Q4 in the questions table; `docs/decisions/2026-11-06-runner-planning-wave-answers.md` (~33).
- `docs/architecture.md` §18.10; `docs/owner-decisions.md` §1.7.2.
- Decisions files: `2026-11-08-CMP-5.md` (~30, `guided_only`), `2026-11-08-CMP-8a.md` (~15, entered-URL policy, loopback), `2026-11-08-CMP-8b.md` (~53: "Windows paths for CMP-11's native qBittorrent aren't accepted yet. CMP-11 adds them"), CMP-5g's files, and `2026-11-11-X-cmp-native-handover.md`.
- Code: `crates/companions/src/manager.rs` (~25, ~113), `crates/companions/src/detect/mod.rs` (~259), `crates/companions/src/guided.rs`, `crates/companions/src/configure/`, `crates/companions/README.md` (~83).

## Do
1. **Catalogue entry** for Windows qBittorrent (shared edit to `crates/companions/data/catalogue.json`): `guided_only`, the official upstream download page and pinned version with its SHA-256, no Lodestar-run installer. Keep `tests/catalogue.rs` green; if CMP-13's pin tool format is needed, match the existing entries.
2. **The guided path on Windows** (`windows_qbittorrent.rs`): the steps shown, then detection of a local qBittorrent (WebUI on loopback, default `http://localhost:8080`), with its version checked against the pin's range.
3. **Configuration** through CMP-8b's adapter: accept the Windows paths CMP-8b refused (drive-letter and UNC rules spelled out, no path traversal, no device names such as `CON`/`NUL`, no `\\?\` tricks), and the hand-over to `integrations.clients` as X-cmp-native-handover does for native installs.
4. **Credentials:** the WebUI password the user sets or pastes is sealed (CMP-3) and never echoed, logged or put in argv.
5. **Decisions file** `docs/decisions/2026-11-11-CMP-11.md`, with any refinement flagged (§2.19).

## Owns
- `crates/companions/src/windows_qbittorrent.rs`, `crates/companions/tests/windows_qbittorrent.rs`, your decisions file.
- **Shared edits (approved):** the catalogue.json entry; the `mod` line in `crates/companions/src/lib.rs`; the detect and configure registrations (one entry each); the Windows-path acceptance in CMP-8b's configure code, limited to the path check; the README line at ~83.
- **Ask the runner first** for anything else. **Don't touch:** `crates/companions/src/manager.rs`, `crates/companions/tests/manager.rs`, `crates/lodestar/src/companion*.rs`, `crates/api/src/companions.rs` (X-cmp-handover-followups, #692, under review), `web/**` (CMP-9b), `crates/lodestar/src/main.rs` and `cli/**` (PK-cli), `crates/lodestar/src/update/**`.

## Done when
- **Gates:** Lodestar never downloads or runs the Windows installer; a detected qBittorrent with a version outside the pin is refused with a plain message; Windows path rules are tested with hostile inputs (traversal, device names, `\\?\`, mixed separators); the password never reaches logs or `Debug`; the hand-over makes it the managed download client.
- **Tests:** waits on conditions with deadlines of at least 10 s, never fixed sleeps; Windows-only code compiles and is tested on windows-latest in CI.
- **Checks:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (per-package runs fine on limited disk; say so).
- **Size:** about 350 lines (plan). Past ~1,000, tell the runner first.
- **Process:** `.claude/skills/steward/SKILL.md`, push once per round, never merge. Branch `slice/CMP-11`; PR title `CMP-11: Windows qBittorrent: guided install, detection and configuration`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA once CI is green, Windows included, using the Claude Code Remote MCP `send_message` tool.
