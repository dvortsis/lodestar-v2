//! uTP, the µTorrent Transport Protocol (BEP 29): reliable, ordered byte
//! streams over UDP with LEDBAT congestion control, which backs off when
//! it starts to add queueing delay to the user's link (architecture §6).
//!
//! - [`packet`]: the wire format. Every datagram is untrusted; parsing is
//!   bounds-checked, allocation-free and fuzzed.
//! - A sans-IO connection state machine (selective ACK, fast retransmit,
//!   timeouts with exponential backoff, sequence-number wraparound) whose
//!   buffers are bounded by [`Config`], whatever the peer sends.
//! - [`UtpSocket`] and [`UtpStream`] (`AsyncRead + AsyncWrite`) on tokio,
//!   over an owned UDP socket or one shared with the DHT on port 6881
//!   (§5), using [`classify`] to split the traffic.

mod conn;
mod ledbat;
pub mod packet;
mod shared;
mod socket;

#[doc(hidden)]
pub mod fuzzing;

pub use shared::SharedPort;
pub use socket::{Config, DualSink, PacketSink, StatsSnapshot, UtpListener, UtpSocket, UtpStream};

/// What a datagram on the shared DHT/uTP port is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// First byte: version nibble 1, type 0–4.
    Utp,
    /// A bencoded dictionary (`d…`).
    Krpc,
    Unknown,
}

/// Splits traffic on a port shared by the DHT and uTP by its first byte.
/// The two can't be confused: `d` (0x64) would be uTP type 6, version 4.
pub fn classify(datagram: &[u8]) -> Protocol {
    if datagram.first() == Some(&b'd') {
        Protocol::Krpc
    } else if packet::looks_like_utp(datagram) {
        Protocol::Utp
    } else {
        Protocol::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_shared_port_traffic() {
        assert_eq!(classify(b"d1:y1:qe"), Protocol::Krpc);
        assert_eq!(classify(&[0x41; 20]), Protocol::Utp);
        assert_eq!(classify(&[0x21; 20]), Protocol::Utp);
        assert_eq!(classify(&[0x41; 5]), Protocol::Unknown);
        assert_eq!(classify(b""), Protocol::Unknown);
        assert_eq!(classify(&[0x61; 20]), Protocol::Unknown);
    }

    proptest::proptest! {
        /// The demux never sends bencode to uTP, and never treats a
        /// datagram shorter than a uTP header as uTP.
        #[test]
        fn bencode_is_never_utp_and_short_datagrams_never_are(
            tail in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..64),
            short in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..packet::HEADER_LEN),
        ) {
            let mut krpc = vec![b'd'];
            krpc.extend_from_slice(&tail);
            proptest::prop_assert_eq!(classify(&krpc), Protocol::Krpc);
            proptest::prop_assert_ne!(classify(&short), Protocol::Utp);
        }
    }
}
