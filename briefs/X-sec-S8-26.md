# X-sec-S8-26: enrichment runtime fix (S8-26)

**Runner brief, 2026-11-27.** Core, Opus, `/security-review` required. A held finding (important, latent): the detail and the chosen fix direction come by message only (`docs/security/v1/README.md` §5). Nothing in this brief, the PR, commits, code comments, test names or the decisions file describes how the old behaviour could be used. Target < 800 changed lines.
- **Branch:** `slice/X-sec-S8-26` from fresh `origin/main`. **PR title:** `X-sec-S8-26: enrichment runtime fix (S8-26)`.
- Add this slice's `runner/slices.yaml` entry (kind core, model opus, the owns below, notes "held finding S8-26 (important, latent; surface enrichment runtimes); runner audit 2026-11-27; any slice that wires an enrichment runtime into the binary, ENR-13b included, depends on it") and add `X-sec-S8-26` to ENR-13b's `deps`. Add a placement line in `runner/dashboard.yaml` if no rule covers it. Run `cd tools/status-connector && npm test`.

## Owns
`crates/pipeline/**` and `docs/decisions/2026-11-27-X-sec-S8-26.md`. If the fix needs a change in `crates/sources/**` (another slice holds it now), stop and message the runner first.

## Rules
- It's a tightening of the owner's content-safety rules (owner-decisions §2.37 and §2.47): never loosen one.
- Tests assert the general invariant (what is checked, and when, on every path) without a specific triggering sequence (README §5 step 5). Condition waits ≥ 10 s, no fixed sleeps; no tokio test-util on the lodestar crate.
- Workspace fmt, clippy `-D warnings`, `cargo test --workspace`. `unsafe_code = "forbid"`.
- Follow `.claude/skills/steward/SKILL.md`. Run `/security-review` before ready. Message the runner "ready <PR> <full sha>". The runner merges. No model identifiers; UTC only.
