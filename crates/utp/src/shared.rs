//! A uTP socket for a UDP port someone else owns (the DHT's, §5), made
//! before that port is bound: the metadata fetcher gets its socket at
//! start, and the DHT attaches its sending sockets once it has bound them.

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use crate::socket::{Config, DualSink, PacketSink, UtpSocket};

/// The sink until the port's owner attaches: sends are refused (and uTP
/// retransmits or times out), and no family is reachable.
#[derive(Default)]
struct LateSink(OnceLock<DualSink>);

impl PacketSink for LateSink {
    fn send_to(&self, packet: &[u8], to: SocketAddr) -> io::Result<()> {
        match self.0.get() {
            Some(sink) => sink.send_to(packet, to),
            None => Err(io::ErrorKind::NotConnected.into()),
        }
    }

    fn reaches(&self, to: SocketAddr) -> bool {
        self.0.get().is_some_and(|s| s.reaches(to))
    }
}

/// A [`UtpSocket`] sharing a port: the owner of the UDP sockets calls
/// [`attach`](Self::attach) once with its sending halves and passes uTP
/// datagrams to [`feed`](Self::feed). Clones share everything.
#[derive(Clone)]
pub struct SharedPort {
    socket: UtpSocket,
    sink: Arc<LateSink>,
}

impl fmt::Debug for SharedPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedPort")
            .field("attached", &self.attached())
            .field("socket", &self.socket)
            .finish()
    }
}

impl SharedPort {
    pub fn new(config: Config) -> SharedPort {
        let sink = Arc::new(LateSink::default());
        let socket = UtpSocket::with_sink(sink.clone(), config);
        SharedPort { socket, sink }
    }

    /// The socket to connect from.
    pub fn socket(&self) -> UtpSocket {
        self.socket.clone()
    }

    /// Routes sends through the owner's sockets, by family. Only the first
    /// call takes effect; it returns whether this one did.
    pub fn attach(&self, sink: DualSink) -> bool {
        self.sink.0.set(sink).is_ok()
    }

    pub fn attached(&self) -> bool {
        self.sink.0.get().is_some()
    }

    /// A datagram the owner classified as uTP ([`crate::classify`]).
    /// Returns whether it parsed. Must run inside a tokio runtime.
    pub fn feed(&self, datagram: &[u8], from: SocketAddr) -> bool {
        self.socket.feed(datagram, from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sends_wait_for_the_owner_and_go_out_by_family() {
        let port = SharedPort::new(Config::default());
        let v4: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let v6: SocketAddr = "[::1]:9".parse().unwrap();
        assert!(!port.attached());
        assert!(!port.socket().reaches(v4));
        let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let owner: Arc<dyn PacketSink> = Arc::new(udp);
        assert!(port.attach(DualSink {
            v4: Some(owner.clone()),
            v6: None,
        }));
        assert!(
            !port.attach(DualSink::default()),
            "only the first attach counts"
        );
        assert!(port.socket().reaches(v4));
        // A v4-mapped address is the IPv4 peer.
        assert!(
            port.socket()
                .reaches("[::ffff:127.0.0.1]:9".parse().unwrap())
        );
        assert!(!port.socket().reaches(v6), "no IPv6 socket attached");
    }
}
