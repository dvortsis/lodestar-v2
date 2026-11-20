//! The MSE initiator (`lodestar_peer_wire::mse::initiate`, what the crawler
//! runs before a fetch) never panics or succeeds wrongly on what a peer
//! answers, even past the key exchange.
//!
//! A peer's answer is only meaningful under the keys both sides derive, so
//! each run first plays our initiator against our responder (`respond`),
//! both seeded from the input, and keeps what the responder sent. The input
//! then changes those bytes (flips, deletions, insertions, a cut) and the
//! initiator runs again with the same seed against the changed answer: its
//! keys are the same, so everything past the key exchange stays readable
//! except where it was changed. That reaches the sync search, the padding
//! lengths and the method the peer selects.
#![no_main]

use std::future::Future;
use std::io;
use std::pin::{Pin, pin};
use std::task::{Context, Poll, Waker};

use libfuzzer_sys::fuzz_target;
use lodestar_peer_wire::mse::{self, PLAINTEXT, RC4};
use rand::SeedableRng;
use rand::rngs::StdRng;
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};

const INFO_HASH: [u8; 20] = [7; 20];
/// What the initiator sends inside the handshake (a BitTorrent handshake's
/// length).
const INITIAL: [u8; 68] = [1; 68];

/// The responder's side of the pipe, keeping a copy of all it sends.
struct Recorded {
    inner: DuplexStream,
    sent: Vec<u8>,
}

impl AsyncRead for Recorded {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Recorded {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        let written = Pin::new(&mut this.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = written {
            this.sent.extend_from_slice(&buf[..n]);
        }
        written
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// A peer whose whole answer is already written down; what we send is
/// thrown away.
struct Said<'a>(&'a [u8]);

impl AsyncRead for Said<'_> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let n = self.0.len().min(buf.remaining());
        let (now, later) = self.0.split_at(n);
        buf.put_slice(now);
        self.0 = later;
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Said<'_> {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// The methods a side accepts, from two input bits: never none.
fn methods(bits: u8) -> u32 {
    match bits & 3 {
        1 => PLAINTEXT,
        2 => RC4,
        _ => PLAINTEXT | RC4,
    }
}

/// Changes `answer` as each four-byte op says: a position, a kind and a
/// value.
fn change(answer: &mut Vec<u8>, ops: &[u8]) {
    let (ops, _) = ops.as_chunks::<4>();
    for op in ops {
        if answer.is_empty() {
            break;
        }
        let at = usize::from(u16::from_be_bytes([op[0], op[1]])) % answer.len();
        match op[2] % 4 {
            0 => answer[at] ^= op[3] | 1,
            1 => {
                answer.remove(at);
            }
            2 => answer.insert(at, op[3]),
            _ => answer.truncate(at),
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((seeds, ops)) = data.split_first_chunk::<17>() else {
        return;
    };
    let ours = u64::from_le_bytes(seeds[..8].try_into().expect("8 bytes"));
    let theirs = u64::from_le_bytes(seeds[8..16].try_into().expect("8 bytes"));
    let provide = methods(seeds[16]);
    let allowed = methods(seeds[16] >> 2);
    let common = provide & allowed != 0;

    // The honest exchange, both sides polled in turn over an in-memory pipe.
    let (near, far) = tokio::io::duplex(4096);
    let mut near = near;
    let mut far = Recorded {
        inner: far,
        sent: Vec::new(),
    };
    let mut our_rng = StdRng::seed_from_u64(ours);
    let mut their_rng = StdRng::seed_from_u64(theirs);
    let honest = {
        let mut initiator = pin!(mse::initiate(
            &mut near,
            &INFO_HASH,
            provide,
            &INITIAL,
            &mut our_rng
        ));
        let mut responder = pin!(mse::respond(
            &mut far,
            &[INFO_HASH],
            allowed,
            &mut their_rng
        ));
        let mut cx = Context::from_waker(Waker::noop());
        let (mut a, mut b) = (None, None);
        for _ in 0..64 {
            if a.is_none()
                && let Poll::Ready(r) = initiator.as_mut().poll(&mut cx)
            {
                a = Some(r.map(|(_, selected)| selected));
            }
            if b.is_none()
                && let Poll::Ready(r) = responder.as_mut().poll(&mut cx)
            {
                b = Some(r.map(|(which, _, initial)| (which, initial)));
            }
            if a.is_some() && b.is_some() {
                break;
            }
        }
        if common {
            let selected = a
                .expect("the initiator finished")
                .expect("an honest exchange with a method in common succeeds");
            let (which, initial) = b
                .expect("the responder finished")
                .expect("the responder agrees");
            assert_eq!(which, 0);
            assert_eq!(initial, INITIAL);
            assert!(selected & provide & allowed != 0 && selected.count_ones() == 1);
            Some(selected)
        } else {
            None
        }
    };

    // The same initiator, against the answer as the input changes it.
    let mut answer = far.sent;
    change(&mut answer, ops);
    let mut rng = StdRng::seed_from_u64(ours);
    let mut peer = Said(&answer);
    let replay = pin!(mse::initiate(
        &mut peer, &INFO_HASH, provide, &INITIAL, &mut rng
    ));
    // Nothing here waits: it ends in one poll.
    let Poll::Ready(got) = replay.poll(&mut Context::from_waker(Waker::noop())) else {
        panic!("the initiator waits with nothing left to wait for");
    };
    if let Ok((_, selected)) = got {
        assert!(
            selected.count_ones() == 1 && selected & provide != 0,
            "it took a method it didn't offer"
        );
    }
    if ops.len() < 4 {
        // Unchanged, the answer gives the honest result.
        assert_eq!(got.ok().map(|(_, s)| s), honest);
    }
});
