# X-sec-S8-25: plugin TLS fix (S8-25)

**Runner brief, 2026-11-27.** Core, Opus, `/security-review` required. A held finding (important): the detail and the chosen fix direction come by message only (`docs/security/v1/README.md` §5). Nothing in this brief, the PR, commits, code comments, test names or the decisions file describes how the old behaviour could be used. Target < 700 changed lines.
- **Branch:** `slice/X-sec-S8-25` from fresh `origin/main`. **PR title:** `X-sec-S8-25: plugin TLS fix (S8-25)`.
- Add this slice's `runner/slices.yaml` entry (kind core, model opus, the owns below, notes "held finding S8-25 (important; surface plugin TLS); runner audit 2026-11-27") and a placement line in `runner/dashboard.yaml` if no rule covers it. Run `cd tools/status-connector && npm test`.

## Owns
`crates/sources/**`, `crates/sources-irc/**`, `Cargo.toml` and `Cargo.lock` (only if a dependency feature changes), the user-guide section on plugin network routes (per `.claude/skills/user-docs/SKILL.md`), and `docs/decisions/2026-11-27-X-sec-S8-25.md`. If the fix needs anything else, stop and message the runner.

## Rules
- It's a tightening: it never loosens a route, a kill switch or a bound.
- Docker, Unraid, Windows and macOS keep working. Say per platform in the decisions file what each now does.
- Tests assert the general invariant, never a specific triggering setup (README §5 step 5).
- Workspace fmt, clippy `-D warnings`, `cargo test --workspace`. Condition waits ≥ 10 s, no fixed sleeps. `unsafe_code = "forbid"`.
- Follow `.claude/skills/steward/SKILL.md`. Run `/security-review` before ready. Message the runner "ready <PR> <full sha>". The runner merges. No model identifiers; UTC only.
