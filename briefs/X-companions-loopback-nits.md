# X-companions-loopback-nits (Opus; follow-up to #847)

The runner decided this slice, from #847's review nits.

## Do
1. **`crates/companions/tests/detect.rs` ~:418 (`ClosedPort`).** A `::1` bind that fails with `PermissionDenied` (WSAEACCES on Windows, for a port in a Hyper-V or WinNAT excluded range) retries like `AddrInUse`. Retrying opens no window, and the bounded count and the loud panic stay.
2. **~:441.** Assert that the second bind fails with `ErrorKind::AddrInUse` specifically, not just `is_err()`. If Windows reports `PermissionDenied` there, accept either one and say why in a comment.
3. **`loopback_falls_back_to_ipv6` (~:487)** has early returns that pass without asserting (they predate #847). Each one either asserts what it can, or records why the environment can't run the check in a way the test output shows. Never a silent pass. If IPv6 is unavailable on a CI target, say so, and don't count that as a pass of the fallback.
4. **The decisions file** `docs/decisions/2026-11-13-X-flake-companions-loopback.md`:
   - head it `# X-flake-companions-loopback decisions (#847)`;
   - add a sentence on the remaining Windows gap: with no `SO_EXCLUSIVEADDRUSE`, a foreign process using `SO_REUSEADDR` could bind the exact port only by naming it deliberately. Accepted as negligible.
   - add a line for this follow-up.

## Owns
`crates/companions/tests/detect.rs`, that decisions file, and your own placement line in `runner/dashboard.yaml`.

## Rules
- No fixed sleeps. Any wait is a condition with a deadline of at least 10 s.
- No new skips.
- ≲ 120 lines.
- Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test -p` for the companions crate, several times.
- Follow the steward skill, push once per round, and send "ready <PR> <sha>".
