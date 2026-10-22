//! Entry points for the `utp` fuzz target (`fuzz/fuzz_targets/utp.rs`).
//! Not a stable API.

use std::time::{Duration, Instant};

use crate::conn::{Connection, Limits};
use crate::packet::{self, Header, PacketType};

const LIMITS: Limits = Limits {
    recv_window: 8192,
    send_buffer: 8192,
    idle_timeout: std::time::Duration::from_secs(60),
};

/// Parses `data` as a datagram. Whatever parses re-encodes to an equal
/// packet, and the demultiplexer agrees with the parser.
pub fn packet(data: &[u8]) {
    let parsed = packet::parse(data);
    if let Ok(p) = parsed {
        assert!(packet::looks_like_utp(data));
        let mut out = Vec::new();
        packet::encode(&p.header, p.sack, p.payload, &mut out);
        assert_eq!(packet::parse(&out).expect("re-encoded packet parses"), p);
    } else if data.len() >= packet::HEADER_LEN && data[1] == 0 {
        // Without extensions only the type and version can be wrong.
        assert!(!packet::looks_like_utp(data));
    }
}

/// Runs one connection through a script of hostile packets and local
/// events read from `data`. The connection must never panic, must keep its
/// buffers within bounds, and must only emit packets that parse.
pub fn connection(data: &[u8]) {
    let Some((&mode, mut rest)) = data.split_first() else {
        return;
    };
    let t0 = Instant::now();
    let mut now = t0;
    let mut c = if mode & 1 == 0 {
        Connection::connect(0x1234, u16::from(mode), LIMITS, t0, now)
    } else {
        let syn = Header {
            ty: PacketType::Syn,
            conn_id: 0x1233,
            timestamp_us: 0,
            timestamp_diff_us: 0,
            wnd_size: 1 << 16,
            seq_nr: u16::from(mode) << 8,
            ack_nr: 0,
        };
        Connection::accept(&syn, 0xfff0, LIMITS, t0, now)
    };
    let mut out = Vec::new();
    let mut buf = [0u8; 3000];
    let take = |rest: &mut &[u8], n: usize| -> Vec<u8> {
        let n = n.min(rest.len());
        let (a, b) = rest.split_at(n);
        *rest = b;
        a.to_vec()
    };
    while let Some((&op, tail)) = rest.split_first() {
        rest = tail;
        match op % 8 {
            0..=2 => {
                let len = take(&mut rest, 2);
                let len = len.iter().fold(0usize, |a, b| a << 8 | usize::from(*b)) % 2048;
                let datagram = take(&mut rest, len);
                if let Ok(p) = packet::parse(&datagram) {
                    c.on_packet(&p, now);
                }
            }
            3 => {
                let chunk = take(&mut rest, usize::from(op >> 3) * 97);
                let _ = c.write(&chunk);
            }
            4 => {
                let n = usize::from(op >> 3) * 211;
                let n = n.min(buf.len());
                let _ = c.read(&mut buf[..n]);
            }
            5 => {
                now += Duration::from_millis(u64::from(op >> 3) * 250);
                c.on_timeout(now);
            }
            6 => match op >> 3 {
                0 => c.close(now),
                1 => c.abandon(now),
                2 => c.reset(),
                3 => c.expire(),
                _ => {}
            },
            _ => {}
        }
        let mut sent = 0;
        while c.poll_transmit(now, &mut out) {
            let p = packet::parse(&out).expect("our packets parse");
            assert!(p.payload.len() <= crate::conn::MSS);
            sent += 1;
            assert!(sent < 10_000, "transmit loop must terminate");
        }
        assert!(
            c.buffered() <= 2 * LIMITS.recv_window as usize + LIMITS.send_buffer,
            "buffers within bounds"
        );
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]
        #[test]
        fn connection_survives_scripts(data in proptest::collection::vec(any::<u8>(), 0..600)) {
            super::connection(&data);
        }

        #[test]
        fn packet_survives(data in proptest::collection::vec(any::<u8>(), 0..100)) {
            super::packet(&data);
        }
    }
}
