# X-sources-api-spec: Sources API: manifest credential spec, config_schema and a purge preview count
**Model:** Opus, with `/security-review`. It covers credentials metadata, a new API surface and a dry-run over user data. A fresh Opus reviewer follows. Runner brief, 2026-11-07T00:25Z. It launches into the slot R-legal-status freed (#558 merged), within owner §2.42's four slots.

## Why
SRC-15b (#552) shipped the External, Restricted and Overlay consent dialogs and the purge flow. It couldn't build the Account dialog, generated config forms or a purge count shown before you commit, because the API doesn't expose what they need. SRC-15c builds that UI on this slice. X-consent-gate-lift, which comes after the v0.5.2 cut, also depends on it.

## Spec
- `docs/decisions/2026-11-06-runner-sources-split.md` (#550): the split, why the gate stays closed in v0.5.2, and why purge asks for the admin password.
- `docs/decisions/2026-11-06-SRC-15b.md` and `docs/decisions/2026-11-06-X-consent-gate-polish.md` (#553).
- `docs/decisions/2026-11-07-runner-followups.md` (#557, may still be open): the `patchSource` wording note.
- Architecture §18.7, §18.8 and §10.10.7.
- `runner/slices.yaml`: the `X-sources-api-spec` entry and its notes.
- Code: `crates/sources/src/{manifest.rs,consent.rs}`, `crates/api/src/{ops.rs,jobs.rs}`, `crates/api/src/dto/{ops.rs,jobs.rs}` and `crates/lodestar/src/ops.rs`.

## Do
1. **A declared credential spec per plugin** in the manifest: name, label, kind (`secret` | `username`) and required. Check it against what `consent::need` and the plugin's credential handling actually accept: a test fails if a plugin declares a credential it can't take, or takes one it doesn't declare.
   - **A new `Manifest` field touches every plugin's manifest in `crates/sources-plugins`.** Give it a default, so plugins without credentials need no edit. Ask the runner before editing any plugin file.
2. **Expose the credential spec and the manifest's `config_schema` read-only** in `GET /sources`, with no secret values.
3. **A purge dry-run count** (§10.10.7 shows the count before you commit). It is read-only, behind the same admin authentication as purge, and changes nothing.
4. **Regenerate the OpenAPI** (`web/src/lib/api/openapi.json` and `schema.d.ts`) with the repo's tooling, never by hand.
5. **Fix the `patchSource` description** in `crates/api/src/ops.rs` (about lines 693–701). It still says an Account source gets 409 `credentials_required`, and describes the consent flow as live. **The gate stays closed in v0.5.2** (#550); describe what the code does today.
6. **Tests:** `crates/api/tests/sources_spec.rs`, covering:
   - the spec and `config_schema` appear for each plugin;
   - no secret value is ever returned;
   - the dry-run count matches what a real purge then removes, and the dry run changes nothing;
   - unauthenticated and API-key calls are refused.
7. **The decisions file** `docs/decisions/2026-11-07-X-sources-api-spec.md`, with notes for SRC-15c and X-consent-gate-lift.

## Done when
- **Owns:** only the slices.yaml `owns`, plus your decisions file and any plugin edit the runner approved.
- **Checks:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` and, in `web/`, `npm run check` (for the regenerated schema) all pass.
- **No fixed sleeps.**
- **Process:** follow `.claude/skills/steward/SKILL.md`, push once per round, and never merge. Title the PR `X-sources-api-spec: Sources API credential spec, config_schema and purge preview`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA when CI is green, using the claude-code-remote `send_message` tool.
