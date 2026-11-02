//! Fetch outcomes per transport, so the uTP gain is measurable (§6, §15).

use std::fmt::{self, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use lodestar_utp::{StatsSnapshot, UtpSocket};

use crate::route::{FAMILIES, Routes};

/// The transport a fetch runs over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Transport {
    #[default]
    Tcp,
    /// uTP (BEP 29) over UDP.
    Utp,
}

impl Transport {
    pub const ALL: [Transport; 2] = [Transport::Tcp, Transport::Utp];

    /// The metrics label.
    pub fn as_str(self) -> &'static str {
        match self {
            Transport::Tcp => "tcp",
            Transport::Utp => "utp",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Counters per transport. A fetch that falls back from uTP to TCP counts
/// one attempt on each; the MSE → plaintext retry within one transport
/// doesn't count again.
#[derive(Debug, Default)]
pub struct FetchStats {
    attempts: [AtomicU64; 2],
    successes: [AtomicU64; 2],
    connect_timeouts: [AtomicU64; 2],
    /// The same attempts and successes by address family (CR-4), indexed
    /// by [`crate::route::family_index`].
    family_attempts: [AtomicU64; 2],
    family_successes: [AtomicU64; 2],
    /// Route-level evidence and the backoff it drives (CR-4).
    pub routes: Routes,
    /// The shared uTP socket, for its counters (`dht.utp` only).
    utp: OnceLock<UtpSocket>,
}

impl FetchStats {
    pub(crate) fn attempted(&self, t: Transport, family: usize) {
        self.attempts[t.index()].fetch_add(1, Ordering::Relaxed);
        self.family_attempts[family].fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn succeeded(&self, t: Transport, family: usize) {
        self.successes[t.index()].fetch_add(1, Ordering::Relaxed);
        self.family_successes[family].fetch_add(1, Ordering::Relaxed);
    }

    /// Attempts by family: 0 IPv4, 1 IPv6.
    pub fn family_attempts(&self, family: usize) -> u64 {
        self.family_attempts[family].load(Ordering::Relaxed)
    }

    pub fn family_successes(&self, family: usize) -> u64 {
        self.family_successes[family].load(Ordering::Relaxed)
    }

    /// Exports `socket`'s counters with these. The first call wins.
    pub fn attach_utp(&self, socket: UtpSocket) {
        let _ = self.utp.set(socket);
    }

    /// The attached uTP socket's counters, if any.
    pub fn utp_stats(&self) -> Option<StatsSnapshot> {
        self.utp.get().map(UtpSocket::stats)
    }

    pub(crate) fn connect_timed_out(&self, t: Transport) {
        self.connect_timeouts[t.index()].fetch_add(1, Ordering::Relaxed);
    }

    pub fn attempts(&self, t: Transport) -> u64 {
        self.attempts[t.index()].load(Ordering::Relaxed)
    }

    pub fn successes(&self, t: Transport) -> u64 {
        self.successes[t.index()].load(Ordering::Relaxed)
    }

    /// Connections that didn't open within their timeout.
    pub fn connect_timeouts(&self, t: Transport) -> u64 {
        self.connect_timeouts[t.index()].load(Ordering::Relaxed)
    }

    /// Appends these counters, and the uTP socket's if given, in the
    /// Prometheus text format.
    ///
    /// `lodestar_fetch_success_total{transport}` replaces the pool's
    /// unlabelled series. It counts every verified info dict, so it can
    /// run slightly ahead of the pool's `fetched`, which drops a result
    /// whose key left the pool while the fetch ran.
    pub fn write_prometheus(&self, utp: Option<&StatsSnapshot>, out: &mut String) {
        let mut family = |name: &str, help: &str, value: &dyn Fn(Transport) -> u64| {
            let _ = writeln!(out, "# HELP lodestar_{name} {help}");
            let _ = writeln!(out, "# TYPE lodestar_{name} counter");
            for t in Transport::ALL {
                let _ = writeln!(out, "lodestar_{name}{{transport=\"{t}\"}} {}", value(t));
            }
        };
        family(
            "fetch_attempts_total",
            "Metadata fetches attempted, by transport.",
            &|t| self.attempts(t),
        );
        family(
            "fetch_success_total",
            "Info dictionaries fetched and verified, by transport.",
            &|t| self.successes(t),
        );
        family(
            "fetch_connect_timeouts_total",
            "Peer connections that didn't open in time, by transport.",
            &|t| self.connect_timeouts(t),
        );
        let mut by_family = |name: &str, kind: &str, help: &str, value: &dyn Fn(usize) -> u64| {
            let _ = writeln!(out, "# HELP lodestar_{name} {help}");
            let _ = writeln!(out, "# TYPE lodestar_{name} {kind}");
            for (i, f) in FAMILIES.iter().enumerate() {
                let _ = writeln!(out, "lodestar_{name}{{family=\"{f}\"}} {}", value(i));
            }
        };
        by_family(
            "fetch_family_attempts_total",
            "counter",
            "Metadata fetches attempted, by peer address family.",
            &|f| self.family_attempts(f),
        );
        by_family(
            "fetch_family_success_total",
            "counter",
            "Info dictionaries fetched and verified, by peer address family.",
            &|f| self.family_successes(f),
        );
        by_family(
            "fetch_no_route_total",
            "counter",
            "Peer connections the local network stack refused as unreachable (no route), by family.",
            &|f| self.routes.no_routes(f),
        );
        by_family(
            "fetch_route_skipped_total",
            "counter",
            "Fetches not dialled because their family's route is backed off, by family.",
            &|f| self.routes.skips(f),
        );
        let now = Instant::now();
        by_family(
            "fetch_route_backoff",
            "gauge",
            "1 while dials to the family are held back after a no-route error.",
            &|f| u64::from(self.routes.backed_off(f, now)),
        );
        if let Some(u) = utp {
            let mut one = |name: &str, help: &str, v: u64| {
                let _ = writeln!(out, "# HELP lodestar_{name} {help}");
                let _ = writeln!(out, "# TYPE lodestar_{name} counter");
                let _ = writeln!(out, "lodestar_{name} {v}");
            };
            one(
                "utp_retransmits_total",
                "uTP packets sent again after loss or a timeout.",
                u.retransmits,
            );
            one(
                "utp_connection_timeouts_total",
                "uTP connections dropped because the peer stopped answering.",
                u.timeouts,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_per_transport_counters() {
        let s = FetchStats::default();
        s.attempted(Transport::Utp, 0);
        s.attempted(Transport::Utp, 1);
        s.succeeded(Transport::Utp, 1);
        s.attempted(Transport::Tcp, 1);
        s.connect_timed_out(Transport::Utp);
        let utp = StatsSnapshot {
            retransmits: 7,
            timeouts: 2,
            ..StatsSnapshot::default()
        };
        let mut out = String::new();
        s.write_prometheus(Some(&utp), &mut out);
        for line in [
            "# TYPE lodestar_fetch_attempts_total counter",
            "lodestar_fetch_attempts_total{transport=\"tcp\"} 1",
            "lodestar_fetch_attempts_total{transport=\"utp\"} 2",
            "lodestar_fetch_success_total{transport=\"tcp\"} 0",
            "lodestar_fetch_success_total{transport=\"utp\"} 1",
            "lodestar_fetch_connect_timeouts_total{transport=\"utp\"} 1",
            "lodestar_fetch_family_attempts_total{family=\"ipv4\"} 1",
            "lodestar_fetch_family_attempts_total{family=\"ipv6\"} 2",
            "lodestar_fetch_family_success_total{family=\"ipv6\"} 1",
            "lodestar_fetch_no_route_total{family=\"ipv6\"} 0",
            "lodestar_fetch_route_skipped_total{family=\"ipv4\"} 0",
            "# TYPE lodestar_fetch_route_backoff gauge",
            "lodestar_fetch_route_backoff{family=\"ipv6\"} 0",
            "lodestar_utp_retransmits_total 7",
            "lodestar_utp_connection_timeouts_total 2",
        ] {
            assert!(out.lines().any(|l| l == line), "missing {line:?} in\n{out}");
        }
        let mut without = String::new();
        s.write_prometheus(None, &mut without);
        assert!(!without.contains("utp_retransmits"));
    }
}
