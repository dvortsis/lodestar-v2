# CMP-5g-3 (Opus, /security-review): supervised companions at serve

Assigned by the runner on 2026-11-11. CMP-5g-1 (#645) and CMP-5g-2 (#653, 29fd001d) are merged. This is the last CMP-5g slice; CMP-9a follows.

## Read first
- `runner-state:briefs/CMP-5g.md` (the CMP-5g brief, the split, persistence and rules), and the `CMP-5g-3` entry in `runner/slices.yaml` (binding notes: plan Q12, Q19, Q18).
- The CMP-5g-1 and CMP-5g-2 decisions files and the runner's reviews `reviews/rv-645-*.md`, `reviews/rv-653-*.md` on runner-state.

## Owns
- `crates/lodestar/src/companions.rs`, `crates/companions/src/manager.rs` (shared edits approved, as the slices.yaml entry says), plus one settings-registry entry for `companions.<id>.egress` if needed; their tests; `docs/decisions/2026-11-11-CMP-5g-3.md`.
- X-prowlarr-settings runs at the same time and adds one line to `crates/lodestar/src/services.rs`; whichever lands second brings `main` in.

## Build
Supervised at serve behind `manager::monitored`; the Q18 `companions.<id>.egress` setting; real restart, logs and regenerate.

## Carried from #653's final review (do all)
- **L4:** regenerate must never rotate a guided companion's credential without an apply and a Sink save in the same step; test it.
- **N7:** the `target_changed` message (`companions_guided.rs:~99`) says to reveal the password before forgetting.
- **N8:** `guided.rs:5` still says "three kinds" of rows.
- **N9:** the CMP-5g-2 decisions line (~137) says the URL is stored "as you typed it"; it is normalised.
- Record for CMP-9a: the forget screen must warn and offer reveal first.

## Rules
Logs shown to the user are scrubbed of secrets. Restart is bounded and refuses during an install. Regenerate OpenAPI files only by the generator.

## Process
- Follow `.claude/skills/steward/SKILL.md` from first push to merge-ready. Branch `slice/CMP-5g-3` from the latest `origin/main`; PR title `CMP-5g-3: <title>`.
- Before review: mutation-check every guard you add (the last reviews were mostly about guards with no test). No fixed sleeps in tests; condition waits with deadlines of at least 10 s.
- Push once per round of fixes. Keep the PR under ~1,500 changed lines (generated files aside); if it grows past that, tell the runner before splitting.
- Anything beyond this brief (a new crate, a `Cargo.toml` change, a loosening of security or content safety, an owner-set number) goes to the runner first.
- When ready: message the runner (session_01EYhgh4JX3AmHzKXPjYCoUg) `CMP-5g-3: PR #<n> ready for review, head <sha>` with CI green. Blocked: `CMP-5g-3: blocked on <what>`. The runner reviews and merges; you never merge.
