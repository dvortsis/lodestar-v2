# B-update-6: updater follow-ups (fail-closed version check, status cost, missing tests, migrated flag)
**Model:** Opus (updater: security-sensitive, crash-safety). The runner reviews; `/security-review` is required. Runner brief, 2026-11-01. Follow-ups from the #324 (B-update-5b) review and #326 (S-updates-4).

## Items
1. **Fail closed on an unparsable version** (#324 optional a). `crates/lodestar/src/update/journal.rs` around l.172 (`kept`): a version that can't be parsed returns `true` ("kept"). Make it fail closed (`false`, or an error the caller handles safely), and show with a test that an unparsable version can't cause a snapshot or overlay to be treated as kept. Check every caller so a `false` can't strand the running version (the reviewer noted `active.version` always matches the running one; prove the running overlay is still protected).
2. **`GET /updates` cost** (#324 optional b). `engine.rs` `rollback()` → `rollback_target` runs `snapshots::list` (and sometimes a read-only store open) on every `GET /updates`. Cache the result per status computation, or with a short TTL invalidated by any install, rollback, snapshot or prune event, so a polled `GET /updates` doesn't touch the disk each time. Correctness first: the cache must never report `available: true` after the snapshot is gone. Add a test for invalidation.
3. **Missing tests** (#324 author note): `rollback.reason = not_supervised` and the restart-pending `busy` reason. Add a test environment knob or fixture for an unsupervised run and for a pending restart, without changing production behaviour.
4. **Did the update migrate the store?** (#326): expose on `GET /updates` → `rollback` whether rolling back restores a store snapshot taken before a schema migration (e.g. `rollback.restores_store: bool`, optional in the schema), so the Roll back dialog can say exactly what's restored. Derive it from data you already have (the snapshot's schema version vs the current one, or the journal's record). Regenerate `openapi.json` and `schema.d.ts` with the repo's tooling. **Don't change the web UI in this slice**; a later web slice will use the field.

## Read first
- `docs/architecture.md` §12.2 (snapshots, rollback journal, revocations), `docs/decisions/2026-11-01-B-update-5b.md`, `docs/decisions/2026-11-01-S-updates-4.md`.
- `crates/lodestar/src/update/{journal.rs,engine.rs,overlay.rs,snapshots*}`, `crates/api/src/{updates.rs,dto/updates.rs}`.

## Rules
- Crash safety unchanged; every step idempotent. No fixed sleeps; condition waits ≥ 10 s.
- `/security-review` before the PR. Don't run formatters over files you didn't change.

## Own
`crates/lodestar/src/update/**`, `crates/api/src/{updates.rs,dto/updates.rs}`, `crates/api/openapi.json`, `web/src/lib/api/schema.d.ts` (regenerated only), their tests and goldens, `docs/decisions/2026-11-01-B-update-6.md`.
**Don't edit** web feature code, `crates/dht`, store schema files, or `vite.config.*`.

## Size
≲ 600 changed lines (goldens included).

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, the openapi drift check; in `web/`: `npm run check`. Label the PR `full-ci`.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
