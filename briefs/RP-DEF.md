# RP-DEF: rule packs on by default on a fresh data directory (v0.5.2)
**Model:** Opus (filters are content safety; on-disk marker in the store). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05.

## Why
Owner §2.39 (recorded in #447): every rule pack is on by default, reviewed in the setup wizard, opt-out. Owner, 2026-11-05 (§2.40 in the runner's next PR): RP-DEF and RP-UC ship in **v0.5.2**, built on `main` (the release branch has neither the wizard nor RP-0's pack versions).

## The spec
It is fixed in `docs/decisions/2026-11-04-X-legal-docs.md` (merged with #447; until then read it from `git fetch origin pull/447/head:pr447`), bullets "Slice ids and ownership", "`RP-DEF` owns", "Order" and **"Existing installs are offered, not flipped"**, plus `docs/owner-decisions.md` §2.39 and `docs/plan/2026-11-04-v06-rulepacks.md` l.~133. Build exactly that. In short:
- `load::pack_switches` keeps reading no row as **off**.
- **Fresh data directory** = a store created on this start with no prior schema. The migration that creates the store (from schema version 0) writes a settings row `packs.fresh = 1` in its own transaction. `upgrade::ensure_seeded` (called by `Pipeline::start` before the first filter read; `seed_packs` is the fallback; `start_packs` plays no part) consumes it: writes an explicit **on** row for every pack in the build, seeds every rule at its **current** version (so nothing is pending), sets `packs.defaults = 1`, clears `packs.fresh`.
- **Existing store** (no marker): on the first start of this build, write an explicit **off** row for every pack without one (including any pack added later by RP-UC) and set `packs.defaults = 1`. Nothing flips silently; filtering is unchanged.
- **Wizard:** shows every pack on; unchecking a pack goes through the weakening flow inline (effect warning plus the password just set); per-rule opt-out is in Settings. Packs that are off on an existing install are offered via the existing `POST /filters/preview` → apply flow (`applyPacks`), the §1.8.7 impact preview; turning a pack on records its current versions.
- New rules in a pack that is already on stay in shadow (RP-0's design) until RP-3/RP-6 ship accept route and UI. RP-DEF builds **no** accept route or UI.

## Tests (all required)
A fresh store started through `Pipeline::start` has every pack on with every rule applied at the first filter read; an upgraded store keeps every pack's state; turning a pack on through preview → apply records its current versions; a store created by `lodestar import` and then served is seeded fresh; a crash after create but before seed is still seeded fresh on the next start; an Undo to a pre-RP-DEF snapshot returns to that snapshot's off state; the wizard's uncheck goes through the weakening flow. No fixed sleeps.

## Own
`crates/filter/src/load.rs`, `crates/filter/src/packs/upgrade.rs`, `crates/store`'s schema/lib (the marker only), `crates/api/src/filters.rs` (seed/start only), `crates/api/tests/golden/`, `web/src/lib/features/wizard/PackStep.svelte` and the wizard's packs copy in `web/src/lib/features/wizard/messages/{en,zh}.json`, the Settings → Filters & Safety copy on defaults, the "off until chosen" sentences in `docs/user/` (filters chapter and chapter 1), and `docs/decisions/2026-11-05-RP-DEF.md` (it supersedes `docs/decisions/2026-10-31-S-wizard.md` on the "at least one pack on" marker). RP-3 also owns `filters.rs` but isn't running; RP-UC follows you.

## Done when
Rust: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. Web: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`, Playwright for the wizard spec (`PW_CHROMIUM_PATH=/opt/pw-browsers/chromium-1243/chrome-linux64/chrome`). Then `/security-review`. PR "RP-DEF: …" against `main` from `slice/RP-DEF`. Follow `.claude/skills/steward/SKILL.md`; message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with PR number and head via the claude-code-remote `send_message` tool. Never merge. Push once per round.
