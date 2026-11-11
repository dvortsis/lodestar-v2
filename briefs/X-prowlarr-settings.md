# X-prowlarr-settings (Opus, /security-review): the Prowlarr plugin becomes configurable and registered

Assigned by the runner on 2026-11-11. CMP-5g-2 (#653) hands over qBittorrent only, because the Prowlarr plugin isn't `config_editable` or registered. This slice makes it so; the Prowlarr companion hand-over follows in CMP-5g-3 or a later slice.

## Read first
- `crates/sources-plugins/src/prowlarr.rs`, and a plugin that is already editable for the pattern (`torrent_cache.rs:~986` `config_editable`, its `check_config`/`config_json`).
- `crates/lodestar/src/services.rs` (where plugins are registered), the consent gate (`docs/owner-decisions.md` §2.49 and the SRC-4 decisions file), and SRC-7's decisions file.
- `docs/plan/2026-11-06-v06-companions.md` (the Prowlarr hand-over), `docs/decisions/2026-11-09-*CMP-5g-2*` (or the CMP-5g-2 decisions file) for what the hand-over will write.

## Owns
- `crates/sources-plugins/src/prowlarr.rs` and its tests; `docs/decisions/2026-11-11-X-prowlarr-settings.md`.
- Shared, small edits (approved): one registration in `crates/lodestar/src/services.rs`; `crates/sources-plugins/src/lib.rs` only if a re-export is needed. CMP-5g-3 is running at the same time and may touch `services.rs`; whichever lands second brings `main` in.

## Build
1. `config_editable` = true, with `check_config` / `config_json`: the address is egress-validated (the same validator and connect-time DNS/no-redirect rules other sources use), the API key is a secret field (never logged, never echoed, never in errors or OpenAPI examples), bounded inputs.
2. Register Prowlarr in `services.rs` **behind the consent gate** (it stays off until consent is given, as other consent-gated sources).
3. Tests: valid and hostile configs (userinfo, fragment, non-http(s), private-range addresses where the policy refuses them, oversize), the key never appearing in any output, the gate keeping it off.

## Rules
CMP-8b is unchanged. No new crate or dependency. Regenerate `openapi.json`/`schema.d.ts` only if a DTO changes, never hand-merge them.

## Process
- Follow `.claude/skills/steward/SKILL.md` from first push to merge-ready. Branch `slice/X-prowlarr-settings` from the latest `origin/main`; PR title `X-prowlarr-settings: <title>`.
- Before review: mutation-check every guard you add (the last reviews were mostly about guards with no test). No fixed sleeps in tests; condition waits with deadlines of at least 10 s.
- Push once per round of fixes. Keep the PR under ~1,500 changed lines (generated files aside); if it grows past that, tell the runner before splitting.
- Anything beyond this brief (a new crate, a `Cargo.toml` change, a loosening of security or content safety, an owner-set number) goes to the runner first.
- When ready: message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `X-prowlarr-settings: PR #<n> ready for review, head <sha>` with CI green. Blocked: `X-prowlarr-settings: blocked on <what>`. The runner reviews and merges; you never merge.
