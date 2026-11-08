# CMP-6 (Opus core, /security-review): the Companions API

Launched by the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) on 2026-11-08. Follow `.claude/skills/steward/SKILL.md`. Branch `slice/CMP-6` from main (20818fea or later). PR title `CMP-6: <title>`.

## Read first
- `docs/plan/2026-11-06-v06-companions.md`: the CMP-6 row, Q5, and the shared-file and ordering notes. Also architecture §10.10.13, §12 and §18.10.
- `docs/owner-decisions.md` §2.47, the owner's answer to Q5 on credential reveal. Build exactly that, no more.
- The merged CMP-5 work (#620, #621, #619): `crates/companions` (`manager.rs`, `layout.rs`, `state.rs`) and `crates/lodestar/src/companions.rs`, which holds `start()`, `Unwired` and `StoreRows`. Also `docs/decisions/2026-11-08-CMP-5.md`, including the CMP-5g list.
- CMP-7 (compose and templates), CMP-8b and CMP-10 as merged.

## Owns
- `crates/api/src/companions.rs`, `crates/api/src/dto/companions.rs`, `crates/api/tests/companions.rs`;
- `crates/lodestar/src/system.rs`, `crates/api/src/dto/system.rs`;
- `docs/decisions/2026-11-08-CMP-6.md`.

**Shared, one line each:** the router in `crates/api/src/lib.rs` and `crates/api/src/dto/mod.rs`. `web/src/lib/api/openapi.json` and `schema.d.ts` are regenerated with the repo's tooling, never hand-merged.

## What
**Admin-only endpoints:**
- list and detail;
- install, update and remove, each with confirmation. Install mirrors the CLI: it shows the pinned version and SHA-256 and proceeds only when the request echoes that SHA-256. Remove lists exactly what it deletes and needs explicit confirmation;
- restart, logs, and compose/template (CMP-7);
- credential reveal, exactly per Q5;
- the system row.

**Until CMP-5g wires the manager,** serve uses `Unwired`. Endpoints that act must return its stable `not_wired` refusal before anything is downloaded, and tests cover that. Design the handlers against the manager's trait or handle, so that CMP-5g only swaps the wiring.

**Rules:**
- Every route sits behind admin auth (`auth::protected` plus the admin check), tested for an unauthenticated caller and a non-admin.
- Error codes are stable; no paths or secrets in responses or logs.
- Credential reveal is audit-logged (event names only, never the value).

## Tests and checks
- API tests for each route:
  - auth;
  - confirmation mismatch refused;
  - `not_wired`;
  - reveal per Q5;
  - the system row.
- No fixed sleeps.
- The regenerated openapi.json passes the drift job.
- `cargo fmt --check`, clippy with `-D warnings`, and `cargo test -p lodestar-api -p lodestar`.

## Size
About 800 hand-written lines plus about 400 regenerated. State the reason in the PR if it goes past 1,500.

## Done
Message the runner `CMP-6: PR #<n> ready for review, head <sha>` when CI is green.
