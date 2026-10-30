# X-ci-health r2 decisions (refused_addr on macOS)

Supersedes the `refused_addr()` fix in `2026-10-30-X-ci-health.md`, which hung on macOS (runner, 2026-10-30: #184's macOS job, `aria2.rs` `nothing_listening_is_unreachable` got `Timeout`, not `Unreachable`).

- **What each OS does with a connection to a port held by a socket that is bound but doesn't listen:** Linux and Windows refuse it at once. macOS matches the SYN to that socket and drops it, so the client waits for its timeout. That is why #181's CI was green: macOS runs only on `main` or with `full-ci`.
- **The fix holds the port with an open connection instead.** `refused_addr()` binds a listener, connects to it, accepts, and closes the listener; the returned `Held` keeps both ends open for the test's lifetime.
  - A new connection finds no listener and no matching connection, so every OS answers with a reset (the same closed-port answer the original bind-and-drop got): `Unreachable`, never `Timeout`.
  - The port stays in use, so no socket bound to port 0 is given it, on any OS. Every `ReplayServer` binds port 0, so none can take it and answer, which was #181's race. Verified on Linux: with the ephemeral range narrowed to 32 ports, filling it never handed out the held port.
- **Why not a port registry:** recording handed-out ports and having `ReplayServer` skip them works only inside one test binary. With four copies of a binary in parallel, the cross-talk came back (16 failures in 600 runs). The open-connection hold passed 600 of 600 in the same setup, and 300 runs under a narrowed range showed no cross-talk.
- **Tests aren't loosened:** `Unreachable` against `Timeout` is still what each adapter's `nothing_listening_is_unreachable` checks. The call sites are unchanged.
