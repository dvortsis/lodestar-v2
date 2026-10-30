//! Ports for the tests that start a server on a port they pick, or need
//! one nobody listens on. Most tests let the server bind port 0 and read
//! where it is (`common`); these can't.

/// A port for one test's server: never the same twice in this run, and
/// below the ephemeral range, so no `bind(0)` or `connect` is handed it.
/// A port picked by `bind(0)` and released isn't reserved: the child binds
/// it seconds later, and again at each restart, and in between the kernel
/// may give it to another test, whose child then holds it, and this
/// supervisor's child fails to start.
pub fn test_port() -> u16 {
    use std::sync::atomic::{AtomicU32, Ordering};
    const COUNT: u32 = 10_000;
    // Below Linux's default range (from 32768) and macOS's (from 49152);
    // above a range that starts lower, if there's room.
    #[cfg(target_os = "linux")]
    let first = match std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range") {
        Ok(range) => {
            let mut r = range.split_whitespace().map(|p| p.parse::<u32>().unwrap());
            let (low, high) = (r.next().unwrap(), r.next().unwrap());
            if low >= 20_000 + COUNT {
                20_000
            } else {
                assert!(
                    high + 1 + COUNT <= 65_536,
                    "no room outside the ephemeral range {low}-{high}"
                );
                high + 1
            }
        }
        Err(_) => 20_000,
    };
    #[cfg(not(target_os = "linux"))]
    let first = 20_000;
    static NEXT: AtomicU32 = AtomicU32::new(0);
    // Spread by process, so copies of this binary run at once (local
    // sessions) mostly use different ports; when two share a block, the
    // check below skips only the ports held at that moment.
    let base = std::process::id() % 100 * 100;
    for _ in 0..COUNT {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let port = (first + (base + n) % COUNT) as u16;
        // Something outside the tests may hold it.
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    panic!("no free port in {first}..{}", first + COUNT);
}
