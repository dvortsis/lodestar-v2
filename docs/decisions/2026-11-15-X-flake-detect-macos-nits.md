# X-flake-detect-macos-nits decisions (#923)

The runner's nits from the review of #899 (X-flake-detect-macos), in `crates/companions/tests/detect.rs`. Tests only.

**Supersedes** one bullet of `2026-11-14-X-flake-detect-macos.md`:

- **"Each process starts at its own place in 20000–29999"** (its line 13): a process starts at 20000 + (pid % 40) × 250, so starts run from 20000 to 29750. Its counter then climbs to 32767, the last port below the Linux ephemeral floor; past that the test fails rather than take an ephemeral port. A port another program holds is still skipped.

## The nits

- **Unique within one process only.** `own_port`'s doc now says so. Two test binaries whose process ids fall in the same bucket (`pid % 40`) walk the same ports, and either may find one held by the other; a held port is skipped, as any other program's is.
- **The closed port never connects on ::1 either.** `a_closed_port_refuses_and_is_never_handed_out` now requires the ::1 connect to fail, refused or with no IPv6 on the host. Before, a connection that succeeded there passed unnoticed.
