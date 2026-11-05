# RP-2: pack-rule overrides (action, extra unless), bound to base version, weakening-gated
**Model:** Opus (content safety; filter engine). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05.

## Spec
`docs/plan/2026-11-04-v06-rulepacks.md` §4 "RP-2" (the authoritative text, including its **Gates**), §3 (storage: one `settings` row per overridden rule, `filter.pack_override:<RULE-ID>` → a small versioned encoding of base version, action, optional `unless` text; in `RuleSet` so `rule_set_prev`/Undo snapshots it), §5 Q8 and Q9 (no migration), and architecture §20.7, §20.8, §20.11. RP-DEF (#454, merged dd6ce6e9) just changed `load.rs`, `upgrade.rs` and the store's pack seeding; read its decisions file (`docs/decisions/2026-11-05-RP-DEF.md`) and build on it.

## In short
- An override changes a pack rule's **action** (hide, demote, flag, drop) and may add an **`unless`**; it never edits matching code.
- Stored with the base rule version it was made against. When an accepted version changes the rule: a **strengthening** override (heavier action) keeps applying; a **weakening** one (lighter action, or any `unless`) is **paused** until re-confirmed against the new version with warning + password (re-confirming is RP-3's API op; RP-2 provides the engine/store state and a paused flag).
- Weakening needs the effect warning and the password (428 without); strengthening doesn't.
- An override row that doesn't read **fails closed** per #357: a stronger action is kept, an unreadable `unless` is dropped; both listed in `Engine::unreadable` and `filter.stand_ins` (#367).
- In `RuleSet`, so Undo covers it. No author, time or reason stored or logged.
- Precedence: inside the pack layer (§20.1, §20.6 "as built"): CSX-5 Override, exceptions and *Always allow* still win as today; the locked floor is never affected.

## Gates (all required)
API tests that every weakening override is refused without the password (428) and every strengthening one isn't; a base change pauses weakening overrides and keeps strengthening ones; the `changed_since_preview` race (§20.7); Undo restores the previous overrides; the fail-closed load cases; no new column or log line records who or when; the filter `budget` test still holds (run it in release).

## Own
`crates/filter/src/packs/overrides.rs` (new), `crates/filter/src/engine.rs`, `crates/filter/src/load.rs`, `crates/store/src/rules.rs`, `crates/api/src/filters.rs`, `crates/api/src/dto/filters.rs`, `crates/filter/tests/pack_overrides.rs`, one `mod` line in `crates/filter/src/packs/mod.rs`, regenerated `openapi.json`/`schema.d.ts` if the API changes, and `docs/decisions/2026-11-05-RP-2.md`. RP-1b and RP-UC wait for you (they edit `packs/mod.rs`).

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, the filter budget test in release, `/security-review`. PR "RP-2: …" against `main` from `slice/RP-2`. Follow `.claude/skills/steward/SKILL.md`; message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) via the claude-code-remote `send_message` tool. Never merge. Push once per round. No fixed sleeps.
