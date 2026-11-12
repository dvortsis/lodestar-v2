# X-companion-pins-polish (Opus; same session as CMP-13 / X-cmp13-installer-pins)

Small follow-ups named in #720 (merged as 65ce6ce4) as outside its paths. Branch `slice/X-companion-pins-polish` from current `main`. PR title `X-companion-pins-polish: <title>`.

## Do
1. `crates/companions/README.md`: the `native.release` row mentions the installer's own `release` folder (`INSTALLER_SOURCES`, pin checked against that folder only, never read at run time).
2. `.github/workflows/companion-pins.yml`:
   - the `artifacts` input description no longer says "every one but installers";
   - a `concurrency` group per companion (for example `companion-pins-${{ inputs.companion }}`), `cancel-in-progress: false`.
   - No change to `permissions:`, secrets, triggers or environments. If any seems needed, stop and ask the runner (owner's call).
3. Decisions: one short file `docs/decisions/2026-11-12-X-companion-pins-polish.md`.

## Checks
`actionlint` if available, the tool's unittest, `cargo test -p lodestar-companions` if the README is doc-tested. Steward skill; message the runner "ready <PR> <sha>" when CI is green.
