# X-flake-scrape-udp-port decisions

Date: 2026-11-13Z. Slice: X-flake-scrape-udp-port. Test-only; no product change.

- **The race:** `unreachable_udp_tracker_fails_fast_or_times_out` bound a UDP port, dropped the socket, then scraped that address. Another test in the same binary starts a fake UDP tracker, which can be given the freed port and answer, so the scrape succeeded. The same pattern was fixed in the companions tests by holding the port (#847).
- **The fix:** the socket is held for the whole test (`silent_udp_tracker_times_out`). A bound socket nobody reads accepts datagrams and never answers, so the scrape can only time out, and the assertion is now `Err(Error::Timeout)` alone. It is not widened: with nothing listening, a refusal (`Io`) happens on some systems and silence on others, and the old match accepted either. Holding the port is one outcome on every system, and the fast refusal path is no longer exercised here.
- **Rest of the tests:** the only drop-then-probe in `crates/tracker-scrape/tests/` was this one (`scrape.rs`, `full.rs`, `full_memory.rs` and `props.rs` bind no other sockets of their own).
