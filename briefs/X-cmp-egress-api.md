# X-cmp-egress-api: a companion's route in the API, and a PUT that changes it and restarts the companion

**Runner brief, 2026-11-12 (UTC).** Opus, core, `/security-review` (egress and the network route a companion's traffic takes). About 500 lines with tests.
- **Branch:** `slice/X-cmp-egress-api` from fresh `origin/main`.
- **PR title:** `X-cmp-egress-api: Companions: a route (egress) field and PUT /companions/{id}/egress that restarts the companion`.
- Follow `.claude/skills/steward/SKILL.md`. The runner merges; you never do.

## Why
CMP-9a (#681) found that no HTTP route reads or writes `companions.<id>.egress`, and `Companion` has no route field (`docs/decisions/2026-11-11-CMP-9a.md` line 8). So the UI can't show or change which route a companion uses, and a change to the source's route never reaches a companion that has already started. CMP-9a's call 4 also wants a "where it runs" field.

## Read first
- `docs/decisions/2026-11-11-CMP-9a.md`, `2026-11-11-X-cmp-egress-prelaunch.md` (especially lines 38–43, the SOCKS5 notes) and `2026-11-11-X-cmp-handover-followups.md`.
- `crates/api/src/companions.rs` (routes at ~222) and `crates/lodestar/src/companions.rs` (`egress_setting`, how a companion's saved row is read at start).
- architecture §18.10 (companions) and the egress and kill-switch rules in §18.7 and §19; `runner/standards.md`.

## What to build
1. **`Companion` gains:**
   - its route, read from the companion's saved row: the same value `egress_setting` uses at start, and the same type and names the sources' route uses;
   - "where it runs", per CMP-9a call 4.

   Both are read-only in GET.
2. **`PUT /companions/{id}/egress`:**
   - Validates the route the same way the sources' route setting does: same allowed kinds, same SSRF and kill-switch rules, and no route that bypasses the kill switch.
   - Saves it to the companion's row, then restarts the companion through the existing restart path so the new route takes effect.
   - It sits behind the same auth and CSRF as the other companion writes. If the other companion writes need the admin password confirmation, this one does too.
   - It refuses with the existing refusal codes for: an unknown id; a companion that takes no route; a companion that isn't installed; and a restart already running.
   - If the restart fails, the saved route stays, and the response says it was saved but not yet applied. It never reports applied when it wasn't.
3. **No secrets in the response or logs:** a SOCKS5 password or proxy credential is never echoed back. Use the reveal pattern if the UI needs it later. Log the route kind only.
4. **openapi.json and schema.d.ts:** regenerate them with the repo's tools (`cargo run -p lodestar --bin openapi`, then `npm run api:types` in `web/`), never by hand.

## Owns
- `crates/api/src/companions.rs` and `crates/api/tests/companions_egress.rs`
- `crates/lodestar/src/companions.rs`
- `web/src/lib/api/openapi.json` and `web/src/lib/api/schema.d.ts` (regenerated only)
- `docs/decisions/2026-11-12-X-cmp-egress-api.md`

Ask the runner before touching anything else. No UI in this slice: the editor is a follow-up, so say so in the decisions file.

## Tests
- GET shows the saved route and where the companion runs.
- PUT:
  - a valid route saves and restarts;
  - each invalid kind is refused;
  - a kill-switch bypass is refused;
  - an unknown id, a companion that takes no route, and one that isn't installed are each refused;
  - without auth or CSRF the request is refused;
  - a restart failure leaves the route saved and the response says it's not applied;
  - credentials never appear in the response or the captured logs.
- No fixed sleeps (waits are conditions with ≥ 10 s deadlines). No tokio `test-util` or `testing` feature on `lodestar`.

## Done
- `cargo fmt --check`;
- `cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`;
- `cargo test -p lodestar-api -p lodestar`, at least the companion tests;
- `npm run check` in `web/`.

Name your refinements in the PR description. Send the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `ready <n> <full sha>` once ci-ok, web-ok and e2e-ok are green. A fresh Opus `/security-review` follows.

**Session window (owner, 2026-11-12):** sessions are cut back at 00:00Z on 2026-11-13. If the runner keeps this session after that, carry on. If not, push your work in progress to the branch, open the PR as a draft, and report its state.
