# B-update-5b: manual Roll back from the API, the fake-source binary test, and the 5a follow-ups
**Model:** Opus (updater: security-sensitive, crash-safety, on-disk journal). A fresh Opus reviewer runs `/security-review`. Runner brief, 2026-11-01. It finishes B-update part 5 (`docs/decisions/2026-11-01-B-update-5.md`, "Follow-ups").

## Why
5a (#312) made Install now work. §12.2 also requires **manual rollback**: "**Roll back** in Settings → Updates (the previous version, while its snapshot is kept), and `lodestar rollback` (§12). Both go through the same journal." The API has no rollback route, and 5a's follow-ups are open.

## Read first
- `docs/architecture.md` §12.2 (around l.1500–1580: probation, the journaled crash-safe rollback, `rollback.json`, the "no process opens the store mid-rollback" rule, revocations survive any rollback), §12 (`lodestar rollback`).
- `docs/decisions/2026-11-01-B-update-5.md`, `2026-10-30-F-updater.md`, `2026-10-23-B-update-r4.md`.
- `crates/lodestar/src/update/**` (engine, install, overlay, the existing rollback journal and the CLI `lodestar rollback` path), `crates/api/src/{updates.rs,dto/updates.rs}`, `crates/api/openapi.json`.

## Do
1. **`POST /updates/rollback`** (body: `{confirm: true}` or the repo's existing confirm convention). It goes through **the same journal and code path** as `lodestar rollback`. No second implementation.
   - Allowed only when a previous version and its snapshot are kept. Otherwise a stable `409` code with a plain message (e.g. `rollback_unavailable` with a structured reason).
   - Refused while an install or a check holds the one-check lock (`409 … busy`), on image mode, and on Windows/fixtures as the other routes are (`updates_unavailable`).
   - The response is `202` with the status; the restart follows the existing graceful-restart path.
   - **Revocations survive:** a rollback never restores an older `trust.json` (§12.2: "no rollback can undo a revocation"). Add the test "trust survives a rollback".
   - `GET /updates` gains a `can_roll_back` (and the reason when false) so the UI can show Roll back truthfully. Optional in the schema, like `installing` was.
2. **A structured reason in the error envelope** for `install_unavailable` (and the new `rollback_unavailable`): add a `reason` field to the envelope (`crates/api` error type) while keeping the message unchanged. Then update `web/src/lib/features/settings/updates/store.svelte.ts` to read `e.reason` (add it to `ApiError` in `web/src/lib/api/client.ts` the way `section`/`assessment` are carried), and delete the regex that parses "(reason)" out of the message. Keep the existing vitest passing and add one for the structured path.
3. **The 5a reviewer's note:** when the overall download timeout fires, the download writer isn't awaited before the `.part` is removed. Make the timeout path await (or abort and join) the writer before cleanup, so no write can land after removal. Add a test.
4. **The binary-level fake-source test** (5a follow-up): a local fake release source serving a wrong hash, a wrong size, an oversized body and another target's artifact. Each fails with its stable code and leaves no file in `<data>/bin`. Use a loopback server in the test, never the network.
5. **A log capture over a whole install** (5a follow-up): one test that runs an install end to end against the fake source and asserts the log events (names from the logging catalogue, no secrets or URLs with credentials).
6. **Out of scope:** the Settings → Updates Roll back button (a follow-up web slice, S-updates-4, uses `can_roll_back`), WiX/MSI rollback (M6), anything in `crates/dht`, `crates/store` schema or the crawler.

## Rules
- Crash safety: every new step is idempotent and journaled like the existing rollback. Add crash-at-each-step tests for the new route's entry into the journal.
- No fixed sleeps; condition waits with ≥ 10 s deadlines.
- Regenerate `openapi.json`, `schema.d.ts` and any goldens with the repo's tooling, never by hand.
- `/security-review` before the PR; the reviewer runs it again.

## Own
`crates/lodestar/src/update/**`, `crates/api/src/{updates.rs,dto/updates.rs}`, the API error envelope type (only to add `reason`), `crates/api/openapi.json`, `web/src/lib/api/{client.ts,schema.d.ts}` (the `reason` field only), `web/src/lib/features/settings/updates/store.svelte.ts` and its test (the regex removal only), your decisions file `docs/decisions/2026-11-01-B-update-5b.md`.
**Don't edit** other web features, `crates/dht`, `crates/store` schema files, or `vite.config.*`.

## Size
≲ 1,500 changed lines (generated files and goldens included). Split if it grows past that and say where.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, the openapi drift check.
- In `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`.
- Label the PR `full-ci` so macOS runs before merge.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Use the `send_message` tool of the Claude Code Remote MCP server. Never merge. Work frugally. Push once per round of fixes.
