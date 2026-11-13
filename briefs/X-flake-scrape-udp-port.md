# X-flake-scrape-udp-port (Opus)

Runner brief, 2026-11-13Z. Branch `slice/X-flake-scrape-udp-port` from current main. PR title `X-flake-scrape-udp-port: <short title>`.

## Problem
`crates/tracker-scrape/tests/scrape.rs`, `unreachable_udp_tracker_fails_fast_or_times_out` (~:155): binds a UDP socket on 127.0.0.1:0, drops it, then scrapes that address expecting `Io` or `Timeout`. Between the drop and the scrape another test in the binary (the fake UDP tracker) can be given the same port and answer, so the test fails. Found during #847 (same pattern in companions, fixed there by holding the port).

## Do
1. Hold the port for the test's duration instead of dropping it (a bound UDP socket nobody reads answers nothing, so the scrape times out). Keep the assertion meaningful: if holding the socket means only `Timeout` can occur, assert what actually happens and say so in a comment; do not widen the match to make it pass.
2. Keep the test fast: use the existing `udp_cfg()` timeout; no fixed sleeps.
3. Check the rest of `crates/tracker-scrape/tests/` for the same drop-then-probe pattern and fix it the same way.
4. Decisions file `docs/decisions/2026-11-13-X-flake-scrape-udp-port.md` (README format, short).

## Owns
`crates/tracker-scrape/tests/**`, the decisions file, your placement line in `runner/dashboard.yaml`.

## Checks
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, the crate's tests several times. No cloud or AWS calls. Follow `.claude/skills/steward/SKILL.md`. Push once; reply `ready <PR> <full 40-char sha>`.
