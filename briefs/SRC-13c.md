# SRC-13c: write a plugin's config through the API, with exact-host consent
**Model:** Opus (consent and egress boundary; untrusted user config). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05T10:55Z. Before or with SRC-15b.

## Why
SRC-13b (#479) registered the torrent-cache plugin, with its endpoints coming from the plugin's config, but nothing can write that config: `PATCH /sources/{id}` has no config field, so the plugin is inert ("needs setup: no torrent cache is listed").

## Read first
- `docs/decisions/2026-11-05-SRC-13b.md`, especially the OPEN item (lines ~10–16) and the runner's two conditions on this slice.
- `docs/decisions/2026-11-02-SRC-4.md` (consent, `Ack`, the FNV-1a endpoint digest), `docs/decisions/2026-11-05-SRC-13.md`.
- `crates/api/src/ops.rs` and its DTO, `crates/pipeline/src/control.rs`, `crates/sources/src/plugin.rs` (`SourcePlugin::endpoints()`), `crates/pipeline/src/egress.rs`, `crates/store/src/sources.rs`.

## Do (branch `slice/SRC-13c` from `origin/main`)
1. **The write path:** a `config` field on `PATCH /sources/{id}`, validated by the plugin's own `Config::from_json` (size cap, reject unknown fields, never echo secrets back), stored through the store's sources config row; applies at the next start, as egress and schedules do.
2. **Condition 1 (blocking): exact consent compare.** Replace SRC-4's 64-bit FNV-1a endpoint digest with the sorted host list (or the full endpoint list) stored in the `Ack` and compared exactly. No new dependency. Existing acks: migrate safely — an ack whose stored form can't be compared exactly counts as not acknowledged (fail closed: the plugin turns off until re-acknowledged), and say so in the decisions file. Compiled-in manifests keep working.
3. **Condition 2 (blocking): full address templates in the consent dialog**, not just host names, since path-level consent matters on hosts with per-user paths such as `/~user/`. The API that feeds the dialog returns them; update the web consent dialog to show them (minimal, tokens only; en and zh keys).
4. A host change switches the plugin off until acknowledged again (already SRC-13b's rule — keep it and test it through the new write path).
5. openapi.json and schema.d.ts regenerated with the repo's tooling, never by hand.
6. Tests: write + validate + reject (bad JSON, unknown field, oversize, non-https template if the plugin requires it); exact-compare (a list differing by one host, order-insensitive equality, a crafted list that would have collided under FNV no longer matching); old-format ack fails closed; template display; no fixed sleeps.
7. Docs: the torrent-cache section of the user guide's sources chapter per `user-docs` (how to list a cache safely), `docs/decisions/2026-11-05-SRC-13c.md`. Add your own `runner/slices.yaml` entry (deps [SRC-13b, SRC-4], model opus) and run `cd tools/status-connector && npm test`.

## Own
`crates/api/src/ops.rs` and its DTO, `crates/pipeline/src/control.rs`, the `Ack` format and compare in `crates/sources` (consent module), the consent dialog component and its message keys in `web/src/lib/features/sources/**`, openapi.json/schema.d.ts, the guide section, the decisions file, your slices.yaml entry.
**Overlaps:** #472 (LEGAL-1) regenerates openapi.json/schema.d.ts and touches `crates/api/tests/ops.rs` — regenerate after bringing `main` in, never hand-merge. SRC-11c edits host wiring only.

## Done when
`cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; in `web/`: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, Playwright for touched specs (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium-1243/chrome-linux64/chrome`). `/security-review` on the diff. PR against `main` titled "SRC-13c: Plugin config write path with exact-host consent", label `full-ci`. Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head via the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round.
