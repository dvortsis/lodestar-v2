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
                Duration::from_millis(c.timeout_ms),
                Err(FailureKind::Unreachable),
            );
        }
        let roll = (hash(&[self.seed, 2, p, word(&key.0)]) % 1000) as u32;
        let ms = |base: u64| {
            Duration::from_millis(base + hash(&[self.seed, 3, p, word(&key.0)]) % (base / 2 + 1))
        };
        if roll < c.success_permille {
            (ms(c.success_ms), Ok(()))
        } else if roll < c.success_permille + c.slow_permille {
            (ms(c.slow_ms), Ok(()))
        } else if roll < c.success_permille + c.slow_permille + c.refuse_permille {
            (
                Duration::from_millis(c.refuse_ms),
                Err(FailureKind::Refused),
            )
        } else {
            (
                Duration::from_millis(c.timeout_ms),
                Err(FailureKind::TimedOut),
            )
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
