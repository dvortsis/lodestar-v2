# X-cmp13-installer-pins (Opus, /security-review): installer pins in the catalogue

Runner brief, 2026-11-11 (UTC). The CMP-13 session continues (#710 merged as 48166c60). Starts once CMP-11b (#711) is merged.

## Why
CMP-13's pin tool pins every artifact but installers. CMP-11 and CMP-11b's Windows qBittorrent path needs a pinned installer (until then it accepts CMP-8b's checked range, runner option D). Your CMP-13 notes: an artifact-level release folder for installers, kept off hosts; the loader in `crates/companions/src/catalogue.rs` (RawArtifact, validate_pin) and the pin tool learn installer pins.

## Do
1. **Catalogue format.** Add the installer's release folder at the artifact level, as you proposed, and keep it off the run-time host list. Validate it in `RawArtifact` / `validate_pin` with the same strictness as other pins. Old catalogues without it still load.
2. **The pin tool learns installers.** `tools/companion-pins` pins an installer artifact: SHA-256 and, where upstream signs, the signature against a fingerprint-pinned key. No new fetch host is allowed at run time.
3. **The qBittorrent Windows installer's source.** It's official only on SourceForge and FossHub. Run time never fetches from either (runner, 2026-11-11). Decide the pin-time source and how the pin is verified there (a signature, a hash published on the official site, or two independent sources agreeing). Write the choice and its threat model in the decisions file. If no option gives a verifiable pin, say so and stop: the runner takes it to the owner (a supply-chain loosening is the owner's call).
4. **Tests.** Loader tests for the new field (valid, missing, malformed, a host in the wrong list), and pin-tool tests with fakes for the installer path. Bring any CMP-11b test that assumes "unpinned" forward without weakening it.
5. **Decisions file** `docs/decisions/2026-11-12-X-cmp13-installer-pins.md`.

## The dry run (separate from the PR)
CMP-13's notes say the first companion-pins dispatch after the merge is a dry run. Send me the inputs you want (companion, version, tag, image_tag, artifacts) and I'll dispatch `companion-pins.yml` on main. Its report goes with the first-pin PR. Don't open a pin PR from it without telling me.

## Owns
`tools/companion-pins/**`, `crates/companions/src/catalogue.rs`, `crates/companions/data/catalogue.json`, `crates/companions/tests/catalogue.rs`, and the decisions file. Anything else: ask first.

## Bar
- runner/standards.md; `cargo fmt --check`, `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`, `cargo test -p lodestar-companions`, and the tool's unittest.
- About 300–600 lines. Branch `slice/X-cmp13-installer-pins` from fresh main, PR titled `X-cmp13-installer-pins: <title>`.
- Steward skill; self-review first; push once per round. Reply "ready <PR> <full sha>" to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) when ci-ok, web-ok and e2e-ok are green.
