# X-flake-companions-loopback decisions (#847)

The flaky detection test seen on #839's Windows run. Brief: runner, 2026-11-13Z. Test-only; no architecture section changes.

## The race
- **Two tests in `crates/companions/tests/detect.rs` used to bind `127.0.0.1:0`, drop the listener, then probe the port**: `nothing_listening_is_not_found_and_unreachable` and `nothing_on_a_loopback_port_is_not_found`.
- **Between the drop and the probe, another test in the same binary could be handed the same ephemeral port.** On #839's Windows run the qBittorrent probe reached a real listener and got `Protocol("HTTP exchange")` instead of "not found".

## The fix: hold the port, don't re-check it
- **Of the brief's two shapes, this takes the second:** a port held closed for the whole test (`ClosedPort` in `detect.rs`).
- **Why not re-check and retry:** a check right before the assertion still leaves a window between the check and the probe, so it can only make the race rarer.
- **How the port is held:** a socket bound to it and never put into listening.
  - It's bound with no `SO_REUSEADDR` (tokio's `TcpSocket::new_v4` sets none), so no other socket can bind the port, and the system won't hand it out as an ephemeral port.
  - With nothing listening, a connection gets a reset, which the client reads as "refused". That holds on Linux, macOS and Windows.
- **On `::1` too.** The loopback probe falls back to `::1`, and `loopback_falls_back_to_ipv6` in the same file listens on `::1` ephemeral ports. So the same port is held on `::1` wherever the runner has IPv6 loopback.
  - If that port is already taken on `::1`, a new pair is chosen, up to 64 times. Both sockets are held from the moment they're bound, so the retry leaves no window; it fails loudly if every attempt is taken.
  - A runner without IPv6 loopback holds only IPv4, since the probe can't fall back there.
- **Tested where CI runs.** `a_closed_port_refuses_and_cant_be_taken` asserts both properties on every CI target (Linux and Windows): a connection is refused (`ConnectionRefused`), and a listener can't be bound on the port while it's held. macOS isn't a CI target; a bound socket with no listener answers with a reset there too.
- **No skips, early returns or sleeps were added.**
- **Remaining Windows gap, accepted as negligible:** the held sockets don't set `SO_EXCLUSIVEADDRUSE`. So a foreign process using `SO_REUSEADDR` could still bind the exact port, but only by naming it deliberately; a port-0 bind is never handed a held port.

## Elsewhere
- **The other tests in `crates/companions/tests/` keep their listeners open while they probe.** None has the pattern.
- **Follow-up outside this slice's paths:** `crates/tracker-scrape/tests/scrape.rs:158-160` (`unreachable_udp_tracker_fails_fast_or_times_out`) binds a UDP socket, drops it, then probes its port.
  - Another test's socket could take the port meanwhile.
  - It accepts a timeout as well as a refusal, so it fails only if that socket answers as a tracker.
  - The same holding approach would fit.

## Follow-up: X-companions-loopback-nits
- **A port Windows won't hand out counts as taken.** A `::1` bind that fails with `PermissionDenied` is retried like `AddrInUse`, because Windows answers that way for a port in an excluded range (Hyper-V, WinNAT). The bounded retry and the loud panic stay.
- **The held-port test checks why the second bind fails:** exactly `AddrInUse` (X-worker-839-followups: `PermissionDenied` can't arise there).
- **`loopback_falls_back_to_ipv6` no longer passes without asserting.**
  - It used to skip if another program held the port on IPv4. Now it holds 127.0.0.1 closed on the server's port itself, so the IPv4 attempt is always refused and the fallback is always asserted.
  - Without IPv6 loopback, it checks only that a held port is found closed. On CI (`CI` set) the test fails instead, since every CI target has `::1`, so a missing fallback is never counted as a pass.
