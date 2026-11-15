//! A metadata fetch never panics, hangs or succeeds wrongly on what a peer
//! sends. The input is everything the peer says on one plain connection,
//! from its BEP 3 handshake on; what the fetch writes is thrown away. This
//! reaches the framing and the fetch loop (sizes, the request window,
//! piece assembly, PEX), which `peer_wire` doesn't. MSE isn't reached: its
//! key exchange is random. Nor are the pace checks (`Error::Behind`) or the
//! first-window deadline: they fall due seconds into a fetch, and here the
//! whole fetch runs in one poll, in microseconds, on a clock nothing
//! advances. Their unit tests in `lodestar_peer_wire::fetch` cover them.
#![no_main]

use std::future::Future;
use std::io;
use std::pin::{Pin, pin};
use std::task::{Context, Poll, Waker};

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{Encryption, FetchOptions, fetch_on_stream};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

thread_local! {
    /// A runtime for the fetch's timers.
    static CLOCK: tokio::runtime::Runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a runtime");
}

/// The one dict a fetch here may succeed with (the seeds serve it).
const DICT: &[u8] =
    b"d6:lengthi1e4:name4:seed12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae";

/// The peer's side of the connection, already written down.
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
        // Nothing put: the peer has closed.
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

fuzz_target!(|data: &[u8]| {
    // A v2 key: the dict's SHA-256, cut to 20 bytes.
    let key = InfoHash(Sha256::digest(DICT)[..20].try_into().expect("20 bytes"));
    let opts = FetchOptions {
        max_metadata: 1 << 20,
        encryption: Encryption::Disable,
        ..FetchOptions::default()
    };
    // The fetch's timers need a runtime's clock; it is never driven, as
    // nothing here waits.
    let _clock = CLOCK.with(|rt| rt.handle().clone()).enter();
    let fetch = pin!(fetch_on_stream(Said(data), &key, &opts, false));
    // The stream never waits, so neither may the fetch: it ends in one poll.
    let Poll::Ready(got) = fetch.poll(&mut Context::from_waker(Waker::noop())) else {
        panic!("the fetch waits with nothing left to wait for");
    };
    if let Ok(f) = got {
        assert!(f.info == DICT, "a fetch succeeded with other bytes");
        assert!(f.info.capacity() == f.info.len() && f.pex.len() <= 200);
    }
});
