# B-update-5: the update engine's install path (download, verify, staging, POST /updates/install)
**Model:** Opus (supply-chain security, on-disk state, concurrency). **`/security-review` is required, and a fresh Opus reviewer follows.** Runner brief, 2026-11-01. Resumes the paused B-update 5/5 from the old lineage.

## Why
B-update shipped parts 1–4 (manifest, key chain, policy, check, overlay supervisor, history, API, SSE). Part 5 (download, verification, overlay staging, the install pipeline and `POST /updates/install`) was in progress when v0.5 work paused it. It was saved as a patch at the history swap. F-updater (#198) has since merged and touches the same files. Settings → Updates (S-updates, #306) is waiting on this API for **Install now**.

## Read first
- CLAUDE.md, `runner/standards.md`, and `runner/slices.yaml` → `B-update` (the long notes are binding: trust anchor, trust.json, rollback journal, the supervisor rules).
- `docs/architecture.md` §12.2, including the "B-update must resolve" checklist at its end; `docs/owner-decisions.md` §2.13, §2.21 and §2.23 (no Authenticode certificate yet: the signed manifest's SHA-256 and size pins are the trust anchor).
- The saved work: `git show origin/runner-state:ports/README.md`, `git show origin/runner-state:ports/B-update-5.commits`, and the patch, `git show origin/runner-state:ports/B-update-5.patch > /tmp/b5.patch`. It contains `notes/B-update-5-status.md` (where it stopped) and `docs/decisions/2026-10-24-B-update-r5a.md` (the 5a decisions).
- `docs/decisions/` for F-updater and the later updater changes (grep `updater`, `F-updater`, `DUR-26`).

## Do
1. **Port.** Run `git apply -3 /tmp/b5.patch` on a branch from `origin/main`. Resolve the conflicts with F-updater (`update/engine.rs`, `update/log_events.rs`, the logs golden) by keeping F-updater's behaviour and adding part 5's. Regenerate goldens, `openapi.json` and `schema.d.ts` with the repo's tooling, never by hand. Move `notes/B-update-5-status.md` content into your decisions file instead of committing a notes/ folder.
2. **Finish 5a, then 5b, per the status note.** If the result is over ~2,500 changed lines (goldens excluded), split: **PR 1 = 5a** (download with size and SHA-256 pins, verification, overlay staging) and **PR 2 = 5b** (the install pipeline and `POST /updates/install`). Ship 5a first.
3. **Security invariants (blocking):**
   - Nothing is staged or installed unless its bytes match the signed manifest's size and SHA-256; a mismatch deletes the partial file and records why.
   - Downloads are bounded (size pin, timeout, no redirects to non-https, and the network policy's outbound rules).
   - Staging and install are crash-safe: atomic renames in the same directory, fsync where the rollback journal needs it, and idempotent steps on restart.
   - Damaged trust data blocks installs (`trust_damaged`). Locked policy (`LODESTAR_UPDATES`) and the manual-steps level are honoured by `POST /updates/install` (409 or the existing error codes, as part 5's goldens define).
   - Install never runs two at once; concurrent requests get a defined answer.
   - The API never echoes local paths or secrets.
4. **Tests:** unit tests for every invariant, fault injection for crash-at-each-step, a fuzz target update if the parser changes, and API goldens. No fixed sleeps; use condition waits with ≥ 10 s deadlines. Cover the Windows paths in CI (the existing matrix).
5. **Decisions file:** `docs/decisions/<date>-B-update-5.md`, carrying the 5a decisions forward and recording what changed against F-updater.

## Own
`crates/lodestar/src/update/**`, `crates/api/src/updates.rs`, `crates/api/src/dto/updates.rs`, `crates/api/tests/{updates,api,openapi}.rs` and the updates goldens, `crates/lodestar/src/services.rs` (the hook only), the logs golden (regenerated), `web/src/lib/api/{openapi.json,schema.d.ts}` (regenerated), `fuzz/fuzz_targets/update.rs`, and your decisions file.

**Don't edit** the web UI (`web/src/lib/features/**`; S-updates part 2 is running there and will wire Install now after you land), the crawler, store schema or search.

## Done when
These pass:
- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- the API openapi drift check
- `npm run check` in `web/` (for the regenerated schema)

Add the `full-ci` label so macOS and Windows run before merge. The disk allowance is small: avoid parallel full builds, and `cargo clean -p` what you don't need.

Open the PR, follow `.claude/skills/steward/SKILL.md`, and message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head. Use the `send_message` tool of the Claude Code Remote MCP server; `SendMessage` doesn't reach the runner. Never merge. Work frugally. Push once per round of fixes.
