# Brief: PK-cli (Opus, core, /security-review), runner 2026-11-11

**Slice:** "lodestar settings get|set|reset on the stopped store; worker placeholder that says it needs v0.6" (`runner/slices.yaml`; spec §10.10.6, §11, §12; plan `docs/plan/2026-11-06-m6-gap-audit.md` rows 29, 32, 110, 179). On the G-m6 path; PK-ops waits for it.
**Branch:** `slice/PK-cli`. **PR title:** `PK-cli: <title>`. Follow `.claude/skills/steward/SKILL.md`; the runner merges. Auto mode.
**Owns:** `crates/lodestar/src/cli/settings.rs` (new), `crates/lodestar/src/cli/mod.rs`, `crates/lodestar/src/main.rs` (the `settings` arm and its privdrop scope only), `crates/lodestar/tests/cli_settings.rs`, `docs/decisions/2026-11-11-PK-cli.md`. X-win-upd-window (open, another session) also edits `main.rs`: keep your main.rs change to the new arm, and whichever merges later brings `main` in.

## What to build
1. `lodestar settings get <section.field>`, `set <section.field> <value>`, `reset <section.field>` and `reset --all`, on the **stopped** store, using the same Settings model, validation and defaults the API uses (no second copy of the rules). `get` without a field may list the editable fields and values.
2. **Refusals (exit 3, nothing changed),** reusing `cli/util.rs` and `cli/lock.rs`: while `serve` (or anything) has the store open; while `bin/rollback.json` exists (§12.2). Run under `privdrop::Scope::DataDir` like the other offline commands.
3. **Security (plan Q2):**
   - Secrets are never on argv or stdout. A secret field (passwords, API keys, tokens, source credentials) is refused by `set`, or read only from stdin / a file with 0600 checks, and `get` prints it as set/unset only. Pick the smallest safe option and record it.
   - Security fields, and the adult and privacy opt-ins, are refused (they need the UI's confirm flows). List exactly which in the decisions file, from the code.
   - Values are validated exactly as the API does; an invalid value exits 2 with the API's message.
4. **keep_or_revert:** setting `http.bind` (or any field with a pending `system.keep_or_revert.<kind>` record) settles a pending record the way the UI's keep would, or refuses with a clear message; read `system/address.rs` and say which, and why, in the decisions file.
5. **`worker` placeholder:** `crates/lodestar/src/cli/worker.rs` already exists on main (Track W). Check what `lodestar worker` does today. If the real command is there, the placeholder is moot: say so in the decisions file and don't add one.
6. `--help` text and the AFTER_HELP exit codes stay accurate. Add `settings` to the module doc list in `cli/mod.rs`.

## Tests (`crates/lodestar/tests/cli_settings.rs`)
- get/set/reset/reset --all round trips on a temp data dir; values persist and `serve`'s settings load sees them.
- Each refusal: store open, rollback.json present, secret on argv, security/adult/privacy field, invalid value (exit codes 3/2/2 as appropriate); nothing changed after a refusal.
- No secret value in stdout/stderr in any test.
- No fixed sleeps; any wait is a condition with a deadline of at least 10 s.

## Before ready
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; `/security-review` on the diff; a fresh self-review. Reply to the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with "ready", the PR number and the full head SHA when CI is green. Never put model identifiers in commits or the PR.
