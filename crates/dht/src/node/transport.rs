//! The node's sockets. Production builds have only the UDP variants; the
//! `testing` feature adds an in-memory one for the deterministic crawler
//! simulator (`crates/sources/tests/sim`), so the real actor can run
//! against a scripted network in paused time.

use std::io;
use std::net::SocketAddr;

use tokio::net::UdpSocket;

/// The sending half: never waits (see `bind` in `node/mod.rs`).
#[derive(Debug)]
pub(crate) enum SendSocket {
    Udp(std::net::UdpSocket),
    #[cfg(feature = "testing")]
    Sim(super::testing::SimSend),
}

impl SendSocket {
    pub(crate) fn send_to(&self, buf: &[u8], to: SocketAddr) -> io::Result<usize> {
        match self {
            SendSocket::Udp(s) => s.send_to(buf, to),
            #[cfg(feature = "testing")]
            SendSocket::Sim(s) => s.send_to(buf, to),
        }
    }
}

/// The receiving half, polled by the actor's event loop.
#[derive(Debug)]
pub(crate) enum RecvSocket {
    Udp(UdpSocket),
    #[cfg(feature = "testing")]
    Sim(super::testing::SimRecv),
}

impl RecvSocket {
    pub(crate) async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        match self {
            RecvSocket::Udp(s) => s.recv_from(buf).await,
            #[cfg(feature = "testing")]
            RecvSocket::Sim(s) => s.recv_from(buf).await,
        }
    }

    pub(crate) fn local_addr(&self) -> io::Result<SocketAddr> {
        match self {
            RecvSocket::Udp(s) => s.local_addr(),
            #[cfg(feature = "testing")]
            RecvSocket::Sim(s) => Ok(s.local),
        }
    }
}

/// `HashMap`/`HashSet` hashing for the node's state. Production keeps the
/// randomly keyed std hasher (hash-flooding resistance); the simulator
/// needs iteration order to repeat across runs and toolchains, so `testing`
/// uses a fixed FNV-1a (std's `DefaultHasher` may change between releases).
#[cfg(not(feature = "testing"))]
pub(crate) type Hasher = std::collections::hash_map::RandomState;
#[cfg(feature = "testing")]
pub(crate) type Hasher = std::hash::BuildHasherDefault<Fnv>;

#[cfg(feature = "testing")]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Fnv(u64);

#[cfg(feature = "testing")]
impl Default for Fnv {
    fn default() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

#[cfg(feature = "testing")]
impl std::hash::Hasher for Fnv {
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

pub(crate) type Map<K, V> = std::collections::HashMap<K, V, Hasher>;
pub(crate) type Set<T> = std::collections::HashSet<T, Hasher>;
