# Brief: X-utp-spurious-rto (Opus, build slot 30)

**Runner:** session_01EYhgh4JX3AmHzKXPjYCoUg. **Branch:** `slice/X-utp-spurious-rto` from `origin/main`. **PR title:** `X-utp-spurious-rto: uTP spurious-timeout sample from the echoed timestamp; timeouts reset only on ACK progress`.

Your slice is its `runner/slices.yaml` entry: read it first (`grep -n "id: X-utp-spurious-rto," runner/slices.yaml`). Background: `docs/decisions/2026-11-12-X-utp-backoff.md` (#788, especially "Known limit") and `docs/decisions/2026-11-12-master-plan-r4.md`.

## Scope
- **Owned paths:** `crates/utp/src/conn.rs`, `crates/utp/src/ledbat.rs`, `crates/utp/src/conn/tests.rs`, plus `docs/decisions/2026-11-13-X-utp-spurious-rto.md` for your calls. Ask the runner before touching any other file.
- **Two changes:**
  1. **A spurious-timeout sample.** When an ACK for a resent packet arrives and the peer's echoed timestamp shows it answers the *first* transmission, treat the timeout as spurious: take an RTT sample from it and undo or limit the backoff, so the backoff stops compounding across timeouts separated by progress on lossy links. Karn's rule must still hold wherever the echo is ambiguous.
  2. **Timeouts reset only on ACK progress.** The `timeouts` count resets on progress (the oldest unacknowledged packet advancing), not on any packet heard. A peer whose packets acknowledge nothing reaches `MAX_TIMEOUTS` and the connection times out. Add a test with such a peer.
- **Must stay green:** X-utp-backoff's Karn and Sim bounds, including `a_round_trip_jump_costs_few_retransmissions` (at most 8 resends over the 1.8, 2.5, 5 and 10 s jumps). If a bound moves, say why in the decisions file.
- **The peer's timestamps are untrusted.** A forged, wrapped or future echo must not produce a negative, zero or huge sample, or a panic. Bound and saturate it. Add a property test over arbitrary echoed timestamps and ACK orderings.
- **No wire-format, setting or owner-set number changes.** A new internal constant is the runner's call (§2.26): name it in the decisions file and in the PR description.
- Read `runner/standards.md` and CLAUDE.md. Run `/security-review` on your diff before the PR.
- Tests ship with the code. No fixed sleeps: condition waits with deadlines of at least 10 s. Prefer the existing simulated clock in the conn tests.

## Checks before the push
- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p lodestar-utp`, plus any crate that depends on it, if that's quick

## Done
Follow `.claude/skills/steward/SKILL.md`. Push once per round. When CI is green, message the runner "ready <PR> <full sha>". Never merge, never push to main, never force-push. No cloud or AWS calls.
