# RP-3: packs API (versions, changelogs, per-rule state, override diffs, pending versions previewed and accepted, corpus results)
**Model:** Opus (content safety: deciding what is weakening). `/security-review` and a fresh Opus reviewer follow. Runner brief, 2026-11-05T06:55Z.

## Why
RP-0 (pack versions and shadow results), RP-1 (the corpora) and RP-2 (#465, overrides) have merged. Without an API, a pack version a build ships stays pending forever: MW-6 sits in shadow on every upgraded store, and a paused override can't be confirmed again. RP-5 and RP-6 build on this.

## The spec
`docs/plan/2026-11-04-v06-rulepacks.md`, in the "RP-3" bullet, is exact; follow it. Also read architecture §20.7, §20.8 and §20.10, and the decisions files for RP-0, RP-1, RP-2 (`docs/decisions/2026-11-05-RP-2.md`), RP-DEF and RP-UC (#469; read it on its branch if it hasn't merged).

## Do (branch `slice/RP-3` from `origin/main`)
1. **`GET /filters`** adds:
   - per pack: its version and changelog;
   - per rule: its version, action, base action, override (RP-2) and `base_changed`;
   - pending versions with their changelog entries.

   Keep the field name for RP-2's override clearly apart from the CSX-5 "Override" list. The #465 review raised this; name it, for example, `pack_override`.
2. **`GET /filters/fixtures`** runs the pack corpora too, and reports counts and disagreements per rule.
3. **`POST /filters/preview`** gains an `accept_pack_versions` op, which returns the impact from the shadow hits and the review queue. `apply` accepts them.
   - An acceptance is strengthening only when every accepted version's recorded direction is `widens` or `action-heavier` **and** the shadow results agree. Anything else needs the password, with `changed_since_preview`: `narrows`, `action-lighter`, `mixed`, no record, or a shadow disagreement.
   - Accepting a pending MW-6 (`widens`, on a store whose malware pack is on) must be strengthening when shadow agrees.
4. **Re-confirming a paused override** (RP-2) is an op too. It is weakening whenever the override is weakening, so it needs the password.
5. **The review queue** shows the holds that acceptance left for review, each with its rule.
6. **Tests:**
   - acceptance is refused without the password when it's weakening, for each weakening direction plus a shadow disagreement;
   - a strengthening acceptance needs no password;
   - a stale preview gives `changed_since_preview`;
   - re-confirming an override is gated;
   - existing filter routes are unchanged; the goldens are regenerated, never hand-edited;
   - no fixed sleeps.
7. **OpenAPI:** regenerate `openapi.json` and `schema.d.ts` with the repo tooling, and check that the web types compile (`npm run check`).
8. **Decisions file:** `docs/decisions/2026-11-05-RP-3.md`.

## Own
`crates/api/src/filters.rs`, `crates/api/src/dto/filters.rs`, `crates/api/tests/filters_packs.rs`, their goldens, `openapi.json` and `schema.d.ts`, and the decisions file.

**Overlaps, and who merges in whom:**
- RP-UC (#469) edits two lines of the `PackSwitch` warning in `crates/api/src/filters.rs` and is about to merge. Bring `main` in after it lands.
- X-source-adult-2 will later need the `/classify/correct` handler in the same file. It waits for you.
- Don't touch `crates/filter/**` or `crates/pipeline/**`. If the engine needs a new read accessor, keep it to one small public function and list it in the PR.

## Done when
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (per crate if the disk won't allow the full run; say so), and `npm run check` in `web/` all pass. Then run `/security-review`.

Open the PR against `main`, titled "RP-3: Packs API: versions, changelogs, per-rule state, override diffs, pending versions previewed and accepted, corpus results". Follow `.claude/skills/steward/SKILL.md`. Message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head, using the Claude Code Remote MCP `send_message` tool. Never merge. Push once per round.
