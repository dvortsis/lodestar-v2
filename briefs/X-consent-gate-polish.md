# X-consent-gate-polish: Consent gate follow-ups before the v0.5.2 cut
**Model:** Opus, with `/security-review`. This is consent and content-safety gating. A fresh Opus reviewer follows. Runner brief, 2026-11-06T22:05Z. It launches into the slot WK-7 freed (#549 merged), within owner §2.42's four slots.

## Spec
- `runner/slices.yaml`, entry `X-consent-gate-polish`, and its notes. This is the list of what to do.
- `docs/decisions/2026-11-06-X-consent-gate-052.md` (#537): what the gate is, where it runs and its tests.
- `docs/decisions/2026-11-06-runner-sources-split.md` (#550): why the gate stays closed in v0.5.2, why Account closes now, and the split with X-consent-gate-lift.
- Owner §2.49 in `docs/owner-decisions.md`: a class is let through only once its dialog shows the approved text.
- The v0.5.2 manifest `docs/plan/2026-11-06-v052-feature-manifest.md`: §3.4, the torrent-cache row and Q4. No consent source can be turned on in v0.5.2.

## Do
1. **Close Account in `control::consent_text_approved`,** so every class except None waits for its approved dialog. Today Account passes once its credentials are stored.
   - Do the same at start (`switch_off_unapproved`).
   - Update the X-consent-gate-052 tests that assert an Account source turns on or stays on: `crates/pipeline/tests/control.rs` and `crates/lodestar/src/ops/tests.rs`.
   - Keep their other parts, including credentials still being required. Don't delete any test.
   - No Account plugin is registered in `crates/lodestar/src/services.rs`. Check that and say so in the decisions file: this changes nothing for users.
2. **Give the start switch-off log event a reason.** When `switch_off_unapproved` or the existing condition check turns a source off at start, the event carries a reason: `consent_text_unapproved` or `consent_lapsed`, using the names in the notes.
   - Regenerate the logs golden (`LODESTAR_BLESS=1`, not by hand).
   - Regenerate the web event names (`npm run event-names`).
   - Add en and zh labels.
3. **The OpenAPI wording** for `consent_required` and `SourcePatch.consent` (`crates/api/src/dto/ops.rs`):
   - say that this build refuses enabling a consent-class source until a later version ships approved text;
   - say what the field will do then.
   - Regenerate `openapi.json` and `schema.d.ts` with the repo's tooling (`cargo run -p lodestar --bin openapi`, then `npm run api:types`).
4. **`crates/lodestar/tests/consent_gate.rs`:** the restart leg asserts `needs == null` where the notes say.
5. **v0.5.2 release notes:** the notes say the torrent cache can't be turned on yet. `docs/releases/v0.5.2.md` isn't in your owns, so check first whether it already says this.
   - If it doesn't, ask the runner before editing. Use the `user-docs` skill.
6. **Decisions file:** `docs/decisions/2026-11-06-X-consent-gate-polish.md`. Note for X-consent-gate-lift which assertions to bring back when each class opens.

## Own
- The entry's owns in `runner/slices.yaml`.
- Your decisions file.
- Ask the runner before touching anything else.

**Running beside it:**
- **SRC-15b:** `web/src/lib/features/sources/**` and `web/.size-limit.mjs`. It shows the gate-refused path from the API's message, so if you change `CONSENT_GATE_REASON` or the error shape, tell the runner first.
- **ENR-9b (#551):** `crates/pipeline/src/unverified.rs`, `writer.rs` (promote).
- **X-fetch-pool-bytes:** `crates/sources/src/pool.rs`, `crates/lodestar/src/metrics.rs`, `docs/metrics.md`.

## Done when
- **These pass:**
  - `cargo fmt --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test` for pipeline, api and lodestar (per crate if the disk is short)
  - in `web/`: `npm run check` and `npx vitest run` (event names and messages)
  - `cd tools/status-connector && npm test`
  - `/security-review`
- **No fixed sleeps.**
- **The PR:** branch `slice/X-consent-gate-polish`, titled "X-consent-gate-polish: <short imperative title>".
- **How you work:**
  - follow `.claude/skills/steward/SKILL.md`;
  - never merge;
  - push once per round;
  - message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) with the PR number and head SHA, using the claude-code-remote `send_message` tool.
