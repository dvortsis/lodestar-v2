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

/// Maps and sets for the node's state. Production keeps the randomly
/// keyed std hash tables (hash-flooding resistance). The simulator needs
/// iteration order to repeat across runs, platforms and toolchains; a fixed
/// hasher isn't enough, since the table layout differs by SIMD width and
/// std release. So `testing` swaps in B-trees, whose order is `Ord` alone.
/// Only the methods both types share are used.
#[cfg(not(feature = "testing"))]
pub(crate) type Map<K, V> = std::collections::HashMap<K, V>;
#[cfg(not(feature = "testing"))]
pub(crate) type Set<T> = std::collections::HashSet<T>;
#[cfg(feature = "testing")]
pub(crate) type Map<K, V> = std::collections::BTreeMap<K, V>;
#[cfg(feature = "testing")]
pub(crate) type Set<T> = std::collections::BTreeSet<T>;
