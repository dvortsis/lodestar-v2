# X-restart-required-creds (Opus, core: pipeline control): a changed credential shows `restart_required`

Assigned by the runner on 2026-11-11, after X-prowlarr-settings (#658, 40945e35) merged. It closes self-review I1 from that PR.

## Read first
- `docs/decisions/2026-11-11-X-prowlarr-settings.md`, the item "`restart_required` doesn't show a changed key", and `docs/decisions/2026-11-09-CMP-5g.md`, which promises that "after a hand-over the Prowlarr source shows `restart_required`".
- `crates/pipeline/src/control.rs`: `config_pending` (around lines 515 and 1169–1205) and how a source's state is built.
- How credentials are stored and read: SRC-4's sealed credentials and `PATCH /sources/{id}/credentials`. Also `crates/sources-plugins/src/prowlarr.rs` `KeyAtStart` and `at_start`, which show the pinned-key pattern.

## The problem
`config_pending` compares only the saved settings. A plugin that pins its credential at start, which today means Prowlarr, keeps using the old key until a restart. When the key changes, including a hand-over to the same address, the source still shows `restart_required: false`, and only its health error says to restart.

## Build
- **`restart_required`:** make the existing `restart_required` / `config_pending` also be true when a credential has changed since start, for a plugin that pins its credential. **Don't change the API's shape.** No new DTO field, no OpenAPI change: X-natural-wording owns `openapi.json` right now. If you find a new field truly can't be avoided, stop and tell the runner.
- **Comparison:** compare by a credential version or a keyed fingerprint recorded at start. Never compare by the secret itself. Never log, return or serialize the secret or any unkeyed hash of it. A monotonic per-source credential revision counter is the preferred design if the store has one, or can add one without a migration. A store migration is a hard stop: ask the runner first.
- **Which plugins:** a plugin that reads its credential fresh each time doesn't need this. Make it opt-in, through a trait method or a flag on the plugin, so only pinning plugins report it.
- **Clearing:** after a restart on the new credential, `restart_required` clears.

## Tests
- A key changed to a new value shows `restart_required`.
- A key changed to the same value: decide and document. The preferred answer is that it doesn't show, if a fingerprint is used. With a counter it does show; say so.
- A hand-over to the same address with a new key shows it.
- A restart clears it.
- A non-pinning plugin never shows it.
- No test output, Debug string or response contains the key; plant a fake key.
- Mutation-check the new comparison.
- No fixed sleeps.

## Owns
- `crates/pipeline/src/control.rs`, `crates/pipeline/tests/control.rs`.
- The smallest needed edit to the plugin trait, wherever it lives (`crates/sources*`).
- `crates/sources-plugins/src/prowlarr.rs`: only to opt in.
- `docs/decisions/2026-11-11-X-restart-required-creds.md`.
- Also update the CMP-5g decisions line if it's now true.
- Anything else goes to the runner first.

## Process
- Follow `.claude/skills/steward/SKILL.md`. Branch `slice/X-restart-required-creds` from the latest `origin/main`. PR title: `X-restart-required-creds: <title>`.
- Run a fresh Opus self-review and `/security-review` before saying ready. Push once per round of fixes.
- When ready, message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg): `X-restart-required-creds: PR #<n> ready for review, head <sha>`, with CI green. If blocked: `X-restart-required-creds: blocked on <what>`.
