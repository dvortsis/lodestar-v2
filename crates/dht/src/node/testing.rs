//! Test-only seam for the deterministic crawler simulator (feature
//! `testing`, never enabled by a production build).
//!
//! [`start`] runs the real node actor on an in-memory socket instead of
//! UDP, with a seeded RNG. Every packet the node sends comes out of
//! [`SimPort::outbound`]; the scripted network answers through
//! [`SimPort::inbound`]. With tokio's paused clock and a current-thread
//! runtime, the same seed and script give the same run.

use std::io;
use std::net::SocketAddr;

use rand::SeedableRng;
use rand::rngs::StdRng;
use tokio::sync::{Mutex, mpsc};

use super::transport::{RecvSocket, SendSocket};
use super::{Config, Harvest, Node, NodeHandle, StatsSnapshot};

/// A datagram the node sent: destination and bytes.
pub type Outbound = (SocketAddr, Vec<u8>);
/// A datagram for the node: bytes and source.
pub type Inbound = (Vec<u8>, SocketAddr);

#[derive(Debug)]
pub(crate) struct SimSend(mpsc::UnboundedSender<Outbound>);

impl SimSend {
    pub(crate) fn send_to(&self, buf: &[u8], to: SocketAddr) -> io::Result<usize> {
        self.0
            .send((to, buf.to_vec()))
            .map(|()| buf.len())
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))
    }
}

#[derive(Debug)]
pub(crate) struct SimRecv {
    pub(crate) local: SocketAddr,
    rx: Mutex<mpsc::UnboundedReceiver<Inbound>>,
}

impl SimRecv {
    pub(crate) async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let mut rx = self.rx.lock().await;
        match rx.recv().await {
            Some((bytes, from)) => {
                let n = bytes.len().min(buf.len());
                buf[..n].copy_from_slice(&bytes[..n]);
                Ok((n, from))
            }
            // The network is gone: park rather than spin on errors.
            None => std::future::pending().await,
        }
    }
}

/// The network's side of a simulated node's socket.
#[derive(Debug)]
pub struct SimPort {
    pub outbound: mpsc::UnboundedReceiver<Outbound>,
    pub inbound: mpsc::UnboundedSender<Inbound>,
}

/// Starts the real node actor on one in-memory IPv4 or IPv6 socket at
/// `local`, seeding its RNG with `seed`. `config.bind` is ignored.
/// `config.bootstrap` entries must be literal `ip:port`s (no DNS).
pub fn start(
    mut config: Config,
    local: SocketAddr,
    seed: u64,
    harvest: Option<mpsc::Sender<Harvest>>,
) -> (Node, SimPort) {
    config.bind = vec![local];
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let sockets = vec![(
        RecvSocket::Sim(SimRecv {
            local,
            rx: Mutex::new(in_rx),
        }),
        SendSocket::Sim(SimSend(out_tx)),
    )];
    let node = Node::spawn(
        config,
        sockets,
        vec![local],
        StdRng::seed_from_u64(seed),
        harvest,
    );
    (
        node,
        SimPort {
            outbound: out_rx,
            inbound: in_tx,
        },
    )
}

/// The node's stats through a handle, which outlives [`Node::shutdown`]:
/// what a run counted up to its very end, shutdown work included.
pub fn stats(handle: &NodeHandle) -> StatsSnapshot {
    handle.stats.snapshot()
}
