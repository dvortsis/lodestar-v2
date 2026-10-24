//! A fake seeding peer that serves info dictionaries over the peer wire,
//! with configurable encryption and misbehaviour. Used by this crate's
//! tests and the pipeline's end-to-end tests (feature `testing`).

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use lodestar_utp::UtpSocket;
use rand::SeedableRng;
use rand::rngs::StdRng;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, BufReader, ReadBuf};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::message::{self, HANDSHAKE_LEN, METADATA_PIECE, MetadataMessage};
use crate::mse::{self, Ciphers};
use crate::wire::{Incoming, Wire};

/// Which handshakes the peer accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accept {
    PlaintextOnly,
    /// MSE required, RC4 only (like clients set to "require encryption").
    EncryptedOnly,
    /// Plain BitTorrent or MSE (RC4 or plaintext).
    Both,
}

/// Ways the peer can misbehave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behavior {
    Honest,
    /// Serves bytes that don't hash to the info hash.
    WrongData,
    /// Rejects every metadata request.
    Reject,
    /// Doesn't set the extension bit.
    NoExtensions,
    /// Advertises this metadata size instead of the real one.
    LieAboutSize(i64),
    /// Completes the handshakes, then never answers.
    Stall,
}

#[derive(Debug)]
pub struct FakePeer {
    addr: SocketAddr,
    task: JoinHandle<()>,
    utp: Option<UtpSocket>,
}

/// One torrent the peer seeds.
#[derive(Debug, Clone)]
pub struct Seed {
    pub info: Vec<u8>,
    pub info_hash: [u8; 20],
}

impl Seed {
    /// Seeds `info` under its SHA-1 (v1) hash.
    pub fn v1(info: Vec<u8>) -> Seed {
        let info_hash = Sha1::digest(&info).into();
        Seed { info, info_hash }
    }
}

impl FakePeer {
    pub async fn spawn(
        seeds: Vec<Seed>,
        accept: Accept,
        behavior: Behavior,
        pex: Vec<SocketAddr>,
    ) -> FakePeer {
        let addr = "127.0.0.1:0".parse().expect("addr");
        FakePeer::spawn_at(addr, seeds, accept, behavior, pex)
            .await
            .expect("bind")
    }

    /// [`spawn`](Self::spawn) on `addr`, which may be taken.
    pub async fn spawn_at(
        addr: SocketAddr,
        seeds: Vec<Seed>,
        accept: Accept,
        behavior: Behavior,
        pex: Vec<SocketAddr>,
    ) -> std::io::Result<FakePeer> {
        let listener = TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let seeds = Arc::new(seeds);
        let pex = Arc::new(pex);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let seeds = seeds.clone();
                let pex = pex.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, &seeds, accept, behavior, &pex).await;
                });
            }
        });
        Ok(FakePeer {
            addr,
            task,
            utp: None,
        })
    }

    /// The same peer, reached over uTP on a loopback UDP port.
    pub async fn spawn_utp(
        seeds: Vec<Seed>,
        accept: Accept,
        behavior: Behavior,
        pex: Vec<SocketAddr>,
    ) -> FakePeer {
        // Test clients all come from 127.0.0.1: lift the per-IP cap.
        let config = lodestar_utp::Config {
            max_per_ip: 1024,
            ..lodestar_utp::Config::default()
        };
        let socket = UtpSocket::bind_with("127.0.0.1:0".parse().expect("addr"), config)
            .await
            .expect("bind");
        let addr = socket.local_addr().expect("bound");
        let mut listener = socket.listen(64);
        let seeds = Arc::new(seeds);
        let pex = Arc::new(pex);
        let task = tokio::spawn(async move {
            while let Ok(mut stream) = listener.accept().await {
                let seeds = seeds.clone();
                let pex = pex.clone();
                tokio::spawn(async move {
                    // No peek on a uTP stream: read the first 20 bytes and
                    // put them back in front.
                    let mut head = [0u8; 20];
                    if stream.read_exact(&mut head).await.is_err() {
                        return;
                    }
                    let plain_hello = is_plain_hello(&head);
                    let stream = Prefixed {
                        head: head.to_vec(),
                        inner: stream,
                    };
                    let _ = serve_stream(
                        BufReader::new(stream),
                        plain_hello,
                        &seeds,
                        accept,
                        behavior,
                        &pex,
                    )
                    .await;
                });
            }
        });
        FakePeer {
            addr,
            task,
            utp: Some(socket),
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The peer's uTP socket (peers from [`spawn_utp`](Self::spawn_utp)).
    pub fn utp(&self) -> Option<&UtpSocket> {
        self.utp.as_ref()
    }
}

/// Plain BitTorrent starts with "\x13BitTorrent protocol"; an MSE public
/// key is random, so a single byte would misfire 1 in 256 times.
fn is_plain_hello(head: &[u8; 20]) -> bool {
    head[0] == 19 && &head[1..] == message::PROTOCOL
}

/// A stream with bytes already read from it put back in front.
struct Prefixed<S> {
    head: Vec<u8>,
    inner: S,
}

impl<S: AsyncRead + Unpin> AsyncRead for Prefixed<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if !self.head.is_empty() {
            let n = self.head.len().min(buf.remaining());
            buf.put_slice(&self.head[..n]);
            self.head.drain(..n);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Prefixed<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, data)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

impl Drop for FakePeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    stream: tokio::net::TcpStream,
    seeds: &[Seed],
    accept: Accept,
    behavior: Behavior,
    pex: &[SocketAddr],
) -> Result<(), crate::Error> {
    let plain_hello = {
        let mut head = [0u8; 20];
        loop {
            stream.readable().await?;
            match stream.peek(&mut head).await? {
                0 => return Ok(()),
                n if n >= head.len() => break,
                _ => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
            }
        }
        is_plain_hello(&head)
    };
    serve_stream(
        BufReader::new(stream),
        plain_hello,
        seeds,
        accept,
        behavior,
        pex,
    )
    .await
}

async fn serve_stream<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    plain_hello: bool,
    seeds: &[Seed],
    accept: Accept,
    behavior: Behavior,
    pex: &[SocketAddr],
) -> Result<(), crate::Error> {
    let (ciphers, initial) = match (plain_hello, accept) {
        (true, Accept::EncryptedOnly) | (false, Accept::PlaintextOnly) => return Ok(()),
        (true, _) => (
            Ciphers {
                read: None,
                write: None,
            },
            Vec::new(),
        ),
        (false, _) => {
            let hashes: Vec<[u8; 20]> = seeds.iter().map(|s| s.info_hash).collect();
            let allowed = if accept == Accept::EncryptedOnly {
                mse::RC4
            } else {
                mse::RC4 | mse::PLAINTEXT
            };
            let mut rng = StdRng::from_rng(&mut rand::rng());
            let (_, ciphers, ia) = mse::respond(&mut stream, &hashes, allowed, &mut rng).await?;
            (ciphers, ia)
        }
    };
    let mut wire = Wire::new(stream, ciphers, initial);
    let mut theirs = [0u8; HANDSHAKE_LEN];
    wire.read_exact(&mut theirs).await?;
    let hs = message::parse_handshake(&theirs)?;
    let Some(seed) = seeds.iter().find(|s| s.info_hash == hs.info_hash) else {
        return Ok(());
    };
    let ours = message::handshake(
        &seed.info_hash,
        b"-FK0001-fakefakefake",
        behavior != Behavior::NoExtensions,
    );
    wire.write_all(&ours).await?;
    if behavior == Behavior::NoExtensions {
        return Ok(());
    }
    // Different ids from ours, to check the requester maps them correctly.
    let mut ext = Vec::new();
    let size = match behavior {
        Behavior::LieAboutSize(n) => n,
        _ => seed.info.len() as i64,
    };
    lodestar_bencode::Encoder::new(&mut ext).dict(|d| {
        d.entry(b"m").dict(|m| {
            m.entry(b"ut_metadata").int(3);
            m.entry(b"ut_pex").int(4);
        });
        d.entry(b"metadata_size").int(size);
        d.entry(b"v").str("FakePeer 1.0");
    });
    // A bitfield first, as real clients send one; the requester skips it.
    wire.write_all(&[0, 0, 0, 3, 5, 0xff, 0x80]).await?;
    wire.send_extended(0, &ext).await?;
    let mut data = seed.info.clone();
    if behavior == Behavior::WrongData {
        data[0] ^= 0xff;
    }
    // The ids the requester asked us to use, from its extended handshake.
    let mut their_metadata_id = None;
    loop {
        let payload = match wire.read_message(METADATA_PIECE + 1024, 1 << 20).await? {
            Incoming::Extended(p) => p,
            _ => continue,
        };
        if payload[0] == 0 {
            let theirs = message::parse_extended_handshake(&payload[1..])?;
            their_metadata_id = theirs.ut_metadata;
            if let (Some(id), false) = (theirs.ut_pex, pex.is_empty()) {
                wire.send_extended(id, &message::pex_message(pex)).await?;
            }
            continue;
        }
        if payload[0] != 3 {
            continue;
        }
        let Some(reply_id) = their_metadata_id else {
            continue;
        };
        if behavior == Behavior::Stall {
            continue;
        }
        if let MetadataMessage::Request { piece } = message::parse_metadata_message(&payload[1..])?
        {
            let piece = usize::try_from(piece).unwrap_or(usize::MAX);
            let start = piece.saturating_mul(METADATA_PIECE);
            let reply = if behavior == Behavior::Reject || start >= data.len() {
                message::metadata_reject(piece)
            } else {
                let end = (start + METADATA_PIECE).min(data.len());
                message::metadata_data(piece, data.len(), &data[start..end])
            };
            wire.send_extended(reply_id, &reply).await?;
        }
    }
}
