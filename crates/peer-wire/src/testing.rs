//! A fake seeding peer that serves info dictionaries over the peer wire,
//! with configurable encryption and misbehaviour. Used by this crate's
//! tests and the pipeline's end-to-end tests (feature `testing`).

use std::net::SocketAddr;
use std::sync::Arc;

use rand::SeedableRng;
use rand::rngs::StdRng;
use sha1::{Digest, Sha1};
use tokio::io::BufReader;
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
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
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
        FakePeer { addr, task }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
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
    // Plain BitTorrent starts with "\x13BitTorrent protocol"; an MSE
    // public key is random, so a single byte would misfire 1 in 256 times.
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
        head[0] == 19 && &head[1..] == message::PROTOCOL
    };
    let mut stream = BufReader::new(stream);
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
