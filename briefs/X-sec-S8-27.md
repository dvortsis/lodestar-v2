# X-sec-S8-27: dht restart snapshot fix (S8-27)

**Runner brief, 2026-11-27.** Core, Opus, `/security-review` required. This fixes a held finding (low). The detail and the chosen fix direction come by message only (`docs/security/v1/README.md` §5). Nothing in this brief, the PR, the commits, code comments, test names or the decisions file may describe how the old behaviour could be used: describe only what the code now does. Target: under 600 changed lines.
- **Branch:** `slice/X-sec-S8-27` from fresh `origin/main`. **PR title:** `X-sec-S8-27: dht restart snapshot fix (S8-27)`.
- **slices.yaml entry:** add one for this slice with kind core, model opus, the owns below, and notes "held finding S8-27 (low; surface dht restart snapshot); runner audit 2026-11-27; /security-review".
- **dashboard.yaml:** add a placement line if no rule covers this slice.
- Run `cd tools/status-connector && npm test`.

## Owns
- `crates/dht/**`
- `fuzz/fuzz_targets/dht_restart.rs` and `fuzz/seeds/dht_restart/**`
- `docs/decisions/2026-11-27-X-sec-S8-27.md`

If the fix needs a change outside these, stop and message the runner first.

## Rules
- **Commit messages** describe only what the code now does. The PR body does too.
- **Tests** assert the general invariant, not a specific triggering sequence (README §5 step 5).
- **No reproducer** goes into `fuzz/seeds/`.
- **Waits:** condition waits with deadlines of at least 10 s, and no fixed sleeps.
- **Budgets:** stay within the restart memory and disk budgets in `docs/decisions/2026-11-24-X-split-resume-gate.md`. If the fix needs more, stop and message the runner with the numbers.
- **The restart snapshot's format and its compatibility rules are an on-disk format.**
  - Keep the existing version rules: v1 bodies still decode, and v2 bodies decode or refuse as they do today.
  - Any format change gets a new version, and the fuzz target and its seeds must cover it.
- **Before you push:** workspace fmt, clippy `-D warnings` and `cargo test --workspace`. `unsafe_code = "forbid"`.
- **Process:** follow `.claude/skills/steward/SKILL.md`, and run `/security-review` before ready. Then message the runner "ready <PR> <full sha>". The runner merges. No model identifiers; UTC only.
