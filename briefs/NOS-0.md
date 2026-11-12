# Brief: NOS-0 (Opus; owner's extra-session window)

**Runner:** session_01EYhgh4JX3AmHzKXPjYCoUg. **Branch:** `slice/NOS-0` from `origin/main`. **PR title:** `NOS-0: NIP-01 relay messages and NIP-35 events: bounded parser, event-id check, claims mapping, fuzzed`.

**Why now:** the owner opened up to 10 extra sessions until **2026-11-12T23:30Z** (2026-11-12, §2.63). Your slice is its `runner/slices.yaml` entry: read it first (`grep -n "id: NOS-0," runner/slices.yaml`).

## Hard time box
- Check the time with `date -u` as you go.
- **By 23:20Z:** push what you have and open the PR. If it isn't finished, open it as a **draft** with a checklist of what's left.
- **By 23:25Z:** message the runner (send_message to session_01EYhgh4JX3AmHzKXPjYCoUg) one of:
  - "ready <pr> <full sha>" if CI is green;
  - "pushed <pr> <full sha>" with CI still running;
  - "draft <pr> <sha>: <what's left>".
- **Then stop work.** The runner resumes the slice later in a normal slot. Don't start anything after 23:25Z.
- Prefer a small, correct, tested PR over a big unfinished one. If the slice is bigger than fits, do the smallest self-contained part well, and say what's left.

## Scope
- **Owned paths:** "crates/sources-feeds/src/nostr.rs", "crates/sources-feeds/Cargo.toml", "Cargo.lock", "fuzz/Cargo.lock", "crates/sources-feeds/tests/nostr.rs", "fuzz/fuzz_targets/nostr.rs", plus `docs/decisions/2026-11-12-NOS-0.md` for your calls. Ask the runner before any other file.
- **Notes from the plan:** /security-review; json.rs, never serde_json; adds sha2 =0.11.0 (already in the lockfile); may land early, inactive (plan §5.4)
- LEM-0 runs at the same time in `crates/sources-feeds`. Adding your `mod` line to `crates/sources-feeds/src/lib.rs` (and the fuzz target entry in `fuzz/Cargo.toml`) is allowed; whichever PR merges later brings main in.
- Read the architecture section the entry points to, `runner/standards.md` and CLAUDE.md. Run `/security-review` on your diff before the PR; untrusted input stays bounded, with no panics.
- Tests ship with the code. Parsers get property tests or fuzz targets as CLAUDE.md says. No fixed sleeps: condition waits with ≥ 10 s deadlines.

## Checks before the push
The CLAUDE.md "Before pushing" list for what you touched (Rust: fmt, clippy `-D warnings`, the affected crates' tests; web: `npm run check`, `npx vitest run`, `npm run build`, `npx size-limit`).

## Done
Follow `.claude/skills/steward/SKILL.md`. Push once per round. Never merge, never push to main, never force-push.
