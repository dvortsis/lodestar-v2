//! Per-family route checks (CR-4). A route is "down" only on local
//! evidence: a UDP `connect` to the peer, which sends nothing and fails
//! only when the local routing table has no route (or the family has no
//! socket at all). Peer timeouts, refusals, resets and a remote router's
//! ICMP never count. The probe's answer is cached per family for
//! [`PROBE_EVERY`], so a route that comes back (a VPN reconnect) is seen
//! within seconds. Nothing here runs unless a switch asks for it.

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long a probe's answer stands before the next fetch probes again.
pub const PROBE_EVERY: Duration = Duration::from_secs(2);

/// 0 for IPv4 (and v4-mapped), 1 for IPv6: the `family` label's index.
pub fn family_index(addr: SocketAddr) -> usize {
    match addr {
        SocketAddr::V4(_) => 0,
        SocketAddr::V6(v6) if v6.ip().to_ipv4_mapped().is_some() => 0,
        SocketAddr::V6(_) => 1,
    }
}

pub const FAMILIES: [&str; 2] = ["ipv4", "ipv6"];

/// A connect error that says "network unreachable". It's counted only;
/// it can come from a remote router's ICMP for one prefix, so it never
/// decides anything on its own.
pub fn is_unreachable(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::NetworkUnreachable
}

/// Whether the local routing table has a route to `to`.
pub fn local_route(to: SocketAddr) -> bool {
    let any: SocketAddr = match to {
        SocketAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
        SocketAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    match std::net::UdpSocket::bind(any) {
        // Only "unreachable" means no route; any other connect error
        // (there are hardly any for UDP) doesn't hold dials back.
        Ok(udp) => !udp
            .connect(to)
            .is_err_and(|e| e.kind() == io::ErrorKind::NetworkUnreachable),
        // No socket of the family at all (IPv6 disabled gives
        // EAFNOSUPPORT, which std leaves uncategorised): no route. A
        // transient bind failure holds the family back for one probe
        // interval at most.
        Err(_) => false,
    }
}

/// The last probe per family: when, and whether it found a route.
type Probe = Option<(Instant, bool)>;

/// Route evidence per family. Probes run only when a switch calls
/// [`routed`](Self::routed); the counters always count.
#[derive(Debug, Default)]
pub struct Routes {
    probe: [Mutex<Probe>; 2],
    unreachable: [AtomicU64; 2],
    skipped: [AtomicU64; 2],
    probes_down: [AtomicU64; 2],
}

impl Routes {
    /// A TCP connect failed as network unreachable (counted only).
    pub(crate) fn unreachable(&self, family: usize) {
        self.unreachable[family].fetch_add(1, Ordering::Relaxed);
    }

    /// Whether `addr`'s family has a local route, probing at most once
    /// per [`PROBE_EVERY`] per family.
    pub(crate) fn routed(&self, addr: SocketAddr, now: Instant) -> bool {
        self.routed_with(addr, now, local_route)
    }

    pub(crate) fn routed_with(
        &self,
        addr: SocketAddr,
        now: Instant,
        probe: impl FnOnce(SocketAddr) -> bool,
    ) -> bool {
        let family = family_index(addr);
        let mut last = lock(&self.probe[family]);
        if let Some((at, routed)) = *last
            && now.saturating_duration_since(at) < PROBE_EVERY
        {
            return routed;
        }
        let routed = probe(addr);
        if !routed {
            self.probes_down[family].fetch_add(1, Ordering::Relaxed);
        }
        *last = Some((now, routed));
        routed
    }

    /// A fetch not dialled because its family has no route.
    pub(crate) fn skipped(&self, family: usize) {
        self.skipped[family].fetch_add(1, Ordering::Relaxed);
    }

    /// Whether the family's last probe found no route (the gauge; 0 when
    /// nothing has probed).
    pub fn down(&self, family: usize) -> bool {
        lock(&self.probe[family]).is_some_and(|(_, routed)| !routed)
    }

    pub fn unreachables(&self, family: usize) -> u64 {
        self.unreachable[family].load(Ordering::Relaxed)
    }

    pub fn skips(&self, family: usize) -> u64 {
        self.skipped[family].load(Ordering::Relaxed)
    }

    /// Probes that found no route.
    pub fn probes_down(&self, family: usize) -> u64 {
        self.probes_down[family].load(Ordering::Relaxed)
    }
}

fn lock(m: &Mutex<Probe>) -> std::sync::MutexGuard<'_, Probe> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_and_the_local_probe() {
        assert_eq!(family_index("1.2.3.4:1".parse().unwrap()), 0);
        assert_eq!(family_index("[::ffff:1.2.3.4]:1".parse().unwrap()), 0);
        assert_eq!(family_index("[2001:db8::1]:1".parse().unwrap()), 1);
        assert!(is_unreachable(&io::ErrorKind::NetworkUnreachable.into()));
        assert!(!is_unreachable(&io::ErrorKind::HostUnreachable.into()));
        // Loopback always has a local route.
        assert!(local_route("127.0.0.1:9".parse().unwrap()));
    }

    #[test]
    fn a_route_that_comes_back_is_seen_within_one_probe_interval() {
        let r = Routes::default();
        let v6: SocketAddr = "[2001:db8::1]:1".parse().unwrap();
        let v4: SocketAddr = "192.0.2.1:1".parse().unwrap();
        let t0 = Instant::now();
        assert!(!r.down(1), "nothing probed yet");
        assert!(!r.routed_with(v6, t0, |_| false));
        assert!(r.down(1));
        // Cached: no probe inside the interval, even if the route is back.
        let probe_ran = std::cell::Cell::new(false);
        let probe = |_| {
            probe_ran.set(true);
            true
        };
        assert!(!r.routed_with(v6, t0 + Duration::from_millis(1999), probe));
        assert!(!probe_ran.get());
        // The next interval probes again and sees the route.
        assert!(r.routed_with(v6, t0 + PROBE_EVERY, |_| true));
        assert!(!r.down(1));
        assert!(r.routed_with(v4, t0, |_| true), "families are independent");
        assert_eq!((r.probes_down(1), r.probes_down(0)), (1, 0));
    }
}
