//! Metadata-fetch peers at the level the pool sees them: a [`Fetcher`]
//! whose result and delay are a pure function of (seed, peer, hash).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_sources::{BoxFuture, Fetcher};

use crate::log::Log;
use crate::rng::{hash, word};
use crate::scenario::FetchConfig;

pub struct SimFetcher {
    pub seed: u64,
    pub cfg: FetchConfig,
    pub log: Arc<Mutex<Log>>,
    pub started: AtomicU64,
}

fn addr_word(a: SocketAddr) -> u64 {
    match a {
        SocketAddr::V4(v4) => (u64::from(u32::from(*v4.ip())) << 16) | u64::from(v4.port()),
        SocketAddr::V6(v6) => word(&v6.ip().octets()) ^ u64::from(v6.port()),
    }
}

impl SimFetcher {
    /// The scripted outcome; independent of when or in what order it's asked.
    pub fn outcome(&self, peer: SocketAddr, key: InfoHash) -> (Duration, Result<(), FailureKind>) {
        let c = &self.cfg;
        let p = addr_word(peer);
        if hash(&[self.seed, 1, p]) % 1000 < u64::from(c.unreachable_permille) {
            return (
                Duration::from_millis(c.connect_timeout_ms),
                Err(FailureKind::Unreachable),
            );
        }
        let timed_out = (
            Duration::from_millis(c.timeout_ms),
            Err(FailureKind::TimedOut),
        );
        let roll = (hash(&[self.seed, 2, p, word(&key.0)]) % 1000) as u32;
        let ms = |base: u64| {
            Duration::from_millis(base + hash(&[self.seed, 3, p, word(&key.0)]) % (base / 2 + 1))
        };
        let success = |d: Duration| {
            if d >= timed_out.0 {
                timed_out
            } else {
                (d, Ok(()))
            }
        };
        if roll < c.success_permille {
            success(ms(c.success_ms))
        } else if roll < c.success_permille + c.slow_permille {
            success(ms(c.slow_ms))
        } else if roll < c.success_permille + c.slow_permille + c.refuse_permille {
            (
                Duration::from_millis(c.refuse_ms),
                Err(FailureKind::Refused),
            )
        } else {
            timed_out
        }
    }
}

impl Fetcher for SimFetcher {
    fn fetch(
        &self,
        peer: SocketAddr,
        key: InfoHash,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        self.started.fetch_add(1, Ordering::Relaxed);
        let (delay, result) = self.outcome(peer, key);
        let tag = match result {
            Ok(()) => "ok",
            Err(FailureKind::Unreachable) => "unreachable",
            Err(FailureKind::Refused) => "refused",
            Err(_) => "timeout",
        };
        self.log.lock().expect("log lock").event(format_args!(
            "fetch {} {peer} {tag} {}ms",
            hex(&key),
            delay.as_millis()
        ));
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            result.map(|()| Fetched {
                info: key.0.to_vec(),
                v2: false,
                encrypted: true,
                pex: Vec::new(),
                client: None,
            })
        })
    }
}

/// The first 8 bytes of a hash, enough to tell keys apart in the log.
pub fn hex(key: &InfoHash) -> String {
    key.0[..8].iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario;

    fn fetcher() -> SimFetcher {
        SimFetcher {
            seed: 3,
            cfg: scenario::healthy().swarm.fetch,
            log: Arc::new(Mutex::new(Log::new(tokio::time::Instant::now(), false))),
            started: AtomicU64::new(0),
        }
    }

    fn peer(i: u32) -> SocketAddr {
        SocketAddr::from(([80, 1, (i >> 8) as u8, i as u8], 6881))
    }

    /// The same (peer, hash) gets the same answer whatever was asked before.
    #[tokio::test]
    async fn outcomes_ignore_call_order() {
        let f = fetcher();
        let pairs: Vec<_> = (0..200u32)
            .map(|i| (peer(i % 50), InfoHash([i as u8; 20])))
            .collect();
        let forward: Vec<_> = pairs.iter().map(|(p, k)| f.outcome(*p, *k)).collect();
        let mut backward: Vec<_> = pairs.iter().rev().map(|(p, k)| f.outcome(*p, *k)).collect();
        backward.reverse();
        assert_eq!(forward, backward);
    }

    /// Outcomes follow the permilles, and nothing succeeds after the deadline.
    #[tokio::test]
    async fn outcomes_match_the_script() {
        let f = fetcher();
        let c = f.cfg;
        let (mut ok, mut unreachable, mut n) = (0u32, 0u32, 0u32);
        for i in 0..4_000u32 {
            let (delay, result) = f.outcome(peer(i), InfoHash([(i % 251) as u8; 20]));
            n += 1;
            match result {
                Ok(()) => {
                    ok += 1;
                    assert!(delay < Duration::from_millis(c.timeout_ms));
                }
                Err(FailureKind::Unreachable) => {
                    unreachable += 1;
                    assert_eq!(delay, Duration::from_millis(c.connect_timeout_ms));
                }
                Err(_) => assert!(delay <= Duration::from_millis(c.timeout_ms)),
            }
        }
        let per_mille = |x: u32| x * 1000 / n;
        assert!(per_mille(unreachable).abs_diff(c.unreachable_permille) < 40);
        // Reachable peers succeed at about success + part of slow.
        let reachable = 1000 - c.unreachable_permille;
        let expect = reachable * c.success_permille / 1000;
        assert!(per_mille(ok) >= expect - 40, "{}", per_mille(ok));
        assert!(per_mille(ok) <= expect + reachable * c.slow_permille / 1000 + 40);
    }
}
