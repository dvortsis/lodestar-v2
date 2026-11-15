# X-flake-detect-macos decisions (#899)

Eight tests in `crates/companions/tests/detect.rs` failed on macOS. They hadn't run there on `main` before: cargo stops at the first failing test binary, and the bench dataset tests failed first until #896. Linux and Windows passed. This is the runner's call, as a test-harness fix. Lodestar doesn't ship on macOS, and the product code is unchanged.

## The cause
- **The harness kept a loopback port closed by binding a socket to it without listening,** so that no other test in the binary could be handed the port.
- **Linux and Windows refuse a connection to such a port.** macOS, like the BSDs, drops it: the kernel finds the bound socket in the CLOSED state and discards the SYN.
- **So on macOS the connect timed out (os error 60)** instead of being refused. The probes waited out their limits instead of falling back to ::1, and the helpers' own checks reported "still answers".

## What the tests do now
- **Their own ports:** the tests take ports from below every target's ephemeral range (Linux 32768 and up, macOS and Windows 49152 and up), each handed out once (`own_port`).
  - The system never hands one to another socket through `bind(.., 0)`, so a closed port can be left unbound, which refuses on every target, and still be the test's alone.
  - Each process starts at its own place in 20000–29999, and a port another program holds is skipped.
- **Every helper that held a port uses them instead:** the closed port, the server on ::1 with 127.0.0.1 closed, both loopbacks of one port, and the wait for a port to close.
- **Unchanged:** no timeout was raised and no test skipped. The test of the closed-port helper now checks that the port refuses on 127.0.0.1 (and ::1 where present), sits below the ephemeral floor, and is handed out once.

## Accepted
- **Another program could bind one of these ports between the check and the test.** A port that's held is skipped when it's picked. A program taking one in the seconds after is the same small risk as with any fixed test port.

## Also unmasked on macOS: one supervise test
- **What failed:** with the detect tests passing, cargo went on to `crates/companions/tests/supervise.rs`. There, `credentialed_requests_follow_the_run_found_ready` waited 60 s for its run to be found ready ("run 1 never found ready").
- **Why:** that's the product failing closed, as designed. A run is ready only once the listener on its port is found to be its process (`supervise::listener`, X-cmp09-fix), which can be told only on Linux and Windows. Elsewhere the owner is `Unknown`, so no run is ever ready, and no native companion is offered there.
- **The change:** the test is gated on Linux and Windows, as `only_the_childs_own_listener_counts` already is in the same file. The listener check gated that test when it landed, but missed this one, which predates it. Nothing else changes.
