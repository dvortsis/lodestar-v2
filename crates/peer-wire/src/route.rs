//! Per-family route backoff (CR-4). Only route-level evidence moves it: a
//! TCP connect that the local stack refuses with "network unreachable"
//! (no route to that family at all). Peer timeouts, refusals and resets
//! say nothing about the route and never count.

use std::io;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The first backoff; each consecutive no-route probe doubles it.
pub const ROUTE_BACKOFF_MIN: Duration = Duration::from_secs(30);
pub const ROUTE_BACKOFF_MAX: Duration = Duration::from_secs(15 * 60);

/// 0 for IPv4 (and v4-mapped), 1 for IPv6: the `family` label's index.
pub fn family_index(addr: SocketAddr) -> usize {
    match addr {
        SocketAddr::V4(_) => 0,
        SocketAddr::V6(v6) if v6.ip().to_ipv4_mapped().is_some() => 0,
        SocketAddr::V6(_) => 1,
    }
}

pub const FAMILIES: [&str; 2] = ["ipv4", "ipv6"];

/// Whether a connect error is route-level evidence. `HostUnreachable` is
/// left out: a remote router sends it for one host, not the family.
pub fn is_no_route(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::NetworkUnreachable
}

#[derive(Debug, Default, Clone, Copy)]
struct State {
    /// Dials to this family are skipped until then.
    until: Option<Instant>,
    /// Consecutive no-route probes, for the doubling.
    strikes: u32,
}

/// Route evidence per family. Its counters always count; it holds dials
/// back only when the fetcher's `route_backoff` switch is on.
#[derive(Debug, Default)]
pub struct Routes {
    state: [Mutex<State>; 2],
    no_route: [AtomicU64; 2],
    skipped: [AtomicU64; 2],
}

impl Routes {
    /// A connect failed with no route: counted, and with `back_off` (the
    /// switch) the family backs off (doubling, capped).
    pub(crate) fn no_route(&self, family: usize, now: Instant, back_off: bool) {
        self.no_route[family].fetch_add(1, Ordering::Relaxed);
        if !back_off {
            return;
        }
        let mut s = lock(&self.state[family]);
        let step = ROUTE_BACKOFF_MIN.saturating_mul(1 << s.strikes.min(16));
        s.until = Some(now + step.min(ROUTE_BACKOFF_MAX));
        s.strikes = s.strikes.saturating_add(1);
    }

    /// A connect in this family opened: the route works.
    pub(crate) fn reached(&self, family: usize) {
        let mut s = lock(&self.state[family]);
        if s.strikes != 0 || s.until.is_some() {
            *s = State::default();
        }
    }

    /// Whether a dial to this family should be skipped now (and counts it).
    /// Once the backoff ends, dials go out again as probes.
    pub(crate) fn skip(&self, family: usize, now: Instant) -> bool {
        let backed_off = lock(&self.state[family]).until.is_some_and(|t| now < t);
        if backed_off {
            self.skipped[family].fetch_add(1, Ordering::Relaxed);
        }
        backed_off
    }

    /// Whether the family is backed off at `now` (the gauge).
    pub fn backed_off(&self, family: usize, now: Instant) -> bool {
        lock(&self.state[family]).until.is_some_and(|t| now < t)
    }

    pub fn no_routes(&self, family: usize) -> u64 {
        self.no_route[family].load(Ordering::Relaxed)
    }

    pub fn skips(&self, family: usize) -> u64 {
        self.skipped[family].load(Ordering::Relaxed)
    }
}

fn lock(m: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_and_evidence() {
        assert_eq!(family_index("1.2.3.4:1".parse().unwrap()), 0);
        assert_eq!(family_index("[::ffff:1.2.3.4]:1".parse().unwrap()), 0);
        assert_eq!(family_index("[2001:db8::1]:1".parse().unwrap()), 1);
        assert!(is_no_route(&io::ErrorKind::NetworkUnreachable.into()));
        for k in [
            io::ErrorKind::HostUnreachable,
            io::ErrorKind::ConnectionRefused,
            io::ErrorKind::TimedOut,
            io::ErrorKind::AddrNotAvailable,
        ] {
            assert!(!is_no_route(&k.into()), "{k:?} isn't route-level");
        }
    }

    #[test]
    fn backs_off_doubling_to_the_cap_and_resets_on_a_connect() {
        let r = Routes::default();
        let t0 = Instant::now();
        assert!(!r.skip(1, t0));
        r.no_route(1, t0, true);
        assert!(r.skip(1, t0 + Duration::from_secs(29)));
        assert!(!r.skip(1, t0 + ROUTE_BACKOFF_MIN), "the probe goes out");
        assert!(!r.backed_off(0, t0), "families are independent");
        let t1 = t0 + ROUTE_BACKOFF_MIN;
        r.no_route(1, t1, true);
        assert!(r.skip(1, t1 + Duration::from_secs(59)));
        assert!(!r.skip(1, t1 + Duration::from_secs(60)));
        for _ in 0..20 {
            r.no_route(1, t1, true);
        }
        assert!(r.backed_off(1, t1 + ROUTE_BACKOFF_MAX - Duration::from_secs(1)));
        assert!(!r.backed_off(1, t1 + ROUTE_BACKOFF_MAX), "capped");
        r.reached(1);
        assert!(!r.backed_off(1, t1));
        r.no_route(1, t1, true);
        assert!(
            !r.backed_off(1, t1 + ROUTE_BACKOFF_MIN),
            "back to the minimum"
        );
        r.no_route(0, t1, false);
        assert!(!r.skip(0, t1), "switch off: counted, never backed off");
        assert_eq!((r.no_routes(1), r.skips(1), r.no_routes(0)), (23, 2, 1));
    }
}
