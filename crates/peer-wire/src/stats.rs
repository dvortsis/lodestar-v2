//! Fetch outcomes per transport, so the uTP gain is measurable (§6, §15).

use std::fmt::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};

use lodestar_utp::StatsSnapshot;

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
}

impl FetchStats {
    pub(crate) fn attempted(&self, t: Transport) {
        self.attempts[t.index()].fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn succeeded(&self, t: Transport) {
        self.successes[t.index()].fetch_add(1, Ordering::Relaxed);
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
        s.attempted(Transport::Utp);
        s.attempted(Transport::Utp);
        s.succeeded(Transport::Utp);
        s.attempted(Transport::Tcp);
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
