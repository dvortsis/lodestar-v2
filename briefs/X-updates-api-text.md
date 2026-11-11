# X-updates-api-text (Sonnet helper): Updates API docs and the web rollback wording

Assigned by the runner on 2026-11-11.

## Read first
The `X-updates-api-text` entry in `runner/slices.yaml` (binding). PK-win-upd-1/2/3 decisions files for the codes.

## Owns
`crates/api/src/updates.rs`, `crates/api/src/dto/updates.rs`, `web/src/lib/api/openapi.json`, `web/src/lib/api/schema.d.ts`, `web/src/lib/features/settings/updates/messages/en.json`, `.../zh.json`.

## Build
- Doc comments: `updates_unavailable` reasons include `not_built_yet`; document #640's string codes `install_kind_unknown` and `data_folder_unchecked` (`capability.reason`), the notice `install_failed`, and that `installing` is set during the download. Check each code against the code that emits it, including what #644 added.
- Regenerate `openapi.json` and `schema.d.ts` with the repo's generator; never hand-edit or hand-merge them. Nobody else running now edits them; if `main` moves them, regenerate after bringing `main` in.
- The web Updates screen's rollback wording for `not_built_yet` (it falls back to unknown): one string in en.json and zh.json, if that's all it takes.

## Checks
`cargo fmt --check`, clippy `-D warnings`, `cargo test -p lodestar-api` (or the api crate's name), the openapi drift check, and in `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`.

## Process
- Follow `.claude/skills/steward/SKILL.md` from first push to merge-ready. Branch `slice/X-updates-api-text` from the latest `origin/main`; PR title `X-updates-api-text: <title>`.
- Before review: mutation-check every guard you add (the last reviews were mostly about guards with no test). No fixed sleeps in tests; condition waits with deadlines of at least 10 s.
- Push once per round of fixes. Keep the PR under ~1,500 changed lines (generated files aside); if it grows past that, tell the runner before splitting.
- Anything beyond this brief (a new crate, a `Cargo.toml` change, a loosening of security or content safety, an owner-set number) goes to the runner first.
- When ready: message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `X-updates-api-text: PR #<n> ready for review, head <sha>` with CI green. Blocked: `X-updates-api-text: blocked on <what>`. The runner reviews and merges; you never merge.
