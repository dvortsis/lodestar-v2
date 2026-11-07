# LEGAL-3a: the legal notice's facts checked in CI

Runner brief, 2026-11-07T12:40Z. Model: Sonnet (facts are read-only; no parser, format or concurrency work). Reviewed by a fresh Opus reviewer before merge.

## Spec
`docs/decisions/2026-11-06-X-legal-docs-r2.md`, section "LEGAL-3", bullet LEGAL-3a. Read it in full; it is the spec. Summary:
- New `crates/lodestar/tests/legal_facts.rs`: one table of claims the notice makes (`docs/legal/legal-notice.md`, identical to `crates/api/src/legal/notice.md`), each checked against the code it describes. On failure, the message names the notice section to update.
- Each claim is matched by a short canonical phrase the notice must contain (wording stays the writer's). The test also checks that the two notice copies are byte-identical.
- Claims: registered source plugins and consent classes vs section 2; peer TTL vs the 30-minute sentence; backup file list vs section 5; `SecretStore::KEY_FILE` and the seal key; notify channel modules vs the outbound list; the PRIVATE_SOURCE-clearing rule (a store test: private then public sighting, read the flag); default `file_folder` and `restart_witness_dir`; the torrent cache (`Consent::External`) and webhooks (`Consent::None`); the Windows temp log (`windows/temp_log.rs`, `#[cfg(windows)]` where needed). Not the Windows Event Log.
- Two small exports: the peer table TTL as `lodestar_dht::PEER_TTL` (from `crates/dht/src/node/peers.rs`, re-exported in `crates/dht/src/lib.rs`), and the backup file list extracted to a constant in `crates/lodestar/src/cli/backup.rs`. No behaviour change.
- New `docs/legal/README.md` listing every checked phrase and what to do when the test fails (a person decides whether the notice version rises, §2.33; never edit the notice automatically).

## Owns
`crates/dht/src/node/peers.rs`, `crates/dht/src/lib.rs` (the export only), `crates/lodestar/src/cli/backup.rs` (the constant only), `crates/lodestar/tests/legal_facts.rs`, `docs/legal/README.md`, `docs/decisions/2026-11-07-LEGAL-3a.md`.
**Do not edit** the notice text itself (`docs/legal/legal-notice.md`, `crates/api/src/legal/notice.md`). If a claim doesn't hold against the code, stop and message the runner with the mismatch; don't change either side.

## Rules
- No fixed sleeps. Tests run on Linux, macOS and Windows CI.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (at least `-p lodestar -p lodestar-dht`) before pushing. Check `df -h` first; disk is limited.
- Branch `slice/LEGAL-3a` from fresh origin/main; PR title "LEGAL-3a: Check the legal notice's facts against the code in CI". Under 600 lines.
- Use the steward skill. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and full head SHA once CI is green.
