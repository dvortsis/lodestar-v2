# CMP-5g (Opus, security-sensitive): guided and detected companions, end to end

Assigned by the runner, session_01EYhgh4JX3AmHzKXPjYCoUg, on 2026-11-09 at about 00:20Z. It continues in CMP-6's session, which merged as #639 (e1df4995). Follow `.claude/skills/steward/SKILL.md`. Branch `slice/CMP-5g` from main e1df4995 or later. PR title `CMP-5g: <title>`. The runner gives it a fresh Opus review and a /security-review.

## Read first
- `docs/architecture.md` §18.7 and §18.10, and `docs/owner-decisions.md` §1.7.2.
- The companions plan's Q12 and Q19.
- The CMP-5, CMP-6 and CMP-8b decisions files, especially CMP-6's lines 19–20: the two things CMP-5g must do.
- The `CMP-5g` entry in `runner/slices.yaml`.

## Owns
- `crates/companions/src/guided.rs`, `crates/companions/src/watch.rs`, `crates/companions/tests/guided.rs`;
- `crates/lodestar/src/companion_wiring.rs`;
- `crates/api/src/companions_guided.rs`, `crates/api/src/dto/companions_guided.rs`, `crates/api/tests/companions_guided.rs`;
- `docs/decisions/2026-11-09-CMP-5g.md`;
- regenerated `web/src/lib/api/openapi.json` and `schema.d.ts`, never hand-merged;
- **shared edits:**
  - the api router;
  - in `crates/lodestar/src/companions.rs`: the wiring line, plus the carry-overs below;
  - `crates/companions/src/manager.rs`: making `field_of` public only.

## What it is
- **Guided companions:**
  - add one;
  - paste its key, which is stored, never logged, and goes out in no response but the reveal;
  - confirm the configuration endpoints.
- **Detected companions:** unsupervised detection and health checks (`watch.rs`).
- **Adapter wiring:** the regenerate, restart and logs routes stop returning `not_wired` where this slice wires them.

## Carried over from CMP-6 (#639 decisions file and its delta review)
1. **Move `Service::installed` onto the journal** (`crates/lodestar/src/companions.rs`, around line 570, which today keys on `version.is_some()`).
2. **Never write a pasted credential into an installed companion's row** without a provenance marker that says it was pasted, not generated.
3. **Important: test the positive path of the data-folder guard.** It's around companions.rs:604-611; its test was lost when the B1 fix landed. Add a unit test that `s.input()` puts `DataFolder::from_running(dir)` in `input.data`, so a guard that resolves the folder but never passes it in fails.
4. **Make the manager's `field_of` public** and delete the `own_field` copy in companions.rs.
5. **I1 end to end:** the template-error field naming, tested through the real generators once a catalogue pin lands. If no pin lands in this slice, say so in the decisions file.

## Rules
- Pasted secrets never reach a log, an error or a response other than the reveal.
- No fixed sleeps (condition waits with deadlines of 10 s or more). No skipped tests.
- **Never add tokio `test-util` or a `testing` feature to the `lodestar` package**, not even as a dev-dependency. CI's "testing seams stay out of the binary" guard refuses it. Tests that need the testing seams go in `crates/companions/tests/`.
- No model identifiers.
- Size counts the regenerated openapi.json and schema.d.ts. If the slice heads past ~3k changed lines, tell the runner before pushing.
