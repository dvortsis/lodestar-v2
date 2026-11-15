//! Metadata fetches against a scripted peer on random traffic, from fixed
//! seeds, over an in-memory stream.
//!
//! An honest peer serves the dict in any order among messages a fetch must
//! put up with (keep-alives, other messages, unknown extensions, PEX,
//! repeated handshakes, pieces sent twice, its own requests): the fetch
//! must succeed. A hostile peer also breaks the protocol at random points:
//! the fetch must end, and may succeed only with the true dict.
//!
//! Both ways the test reads everything the fetch sends and checks that each
//! piece is asked for once, in order, within the announced size, with a
//! bounded number outstanding, and none past the first window before the
//! whole result is reserved; and that a result fits its reservation.
//!
//! The peer always ends by closing. The fetch keeps real deadlines (its
//! timeout, the pace a large dict must keep, the first window's 4 s), so
//! the test runs on tokio's paused clock: time moves only when every task
//! is idle, so a slow or loaded machine can't trip them, and a fetch that
//! truly hangs still ends at its own deadline. A failure prints the seed.

use std::collections::{BTreeMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{Encryption, FetchOptions, Fetched, Reserve, fetch_on_stream, message};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::mpsc;

const SEEDS: std::ops::RangeInclusive<u64> = 1..=1500;
const PIECE: usize = message::METADATA_PIECE;
/// The BEP 10 message id.
const EXTENDED: u8 = 20;
const MAX_METADATA: usize = 2 << 20;
/// Pieces a fetch may ask for before the whole result is reserved.
const FIRST_PIECES: usize = 4;
/// Piece requests a fetch keeps outstanding at most.
const WINDOW: usize = 64;
const MAX_PEX: usize = 200;
/// Turns a hostile peer gives the fetch to ask for more before it leaves.
const YIELDS: usize = 64;
/// A stuck fetch fails the test instead of hanging it (on the paused
/// clock, reached only when every task is idle).
const STUCK: Duration = Duration::from_secs(60);

/// xorshift64*: small, seeded, the same on every platform.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        ((self.next() >> 11) % n as u64) as usize
    }

    fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.next() >> 24) as u8).collect()
    }
}

/// What the peer and the checks share.
#[derive(Default)]
struct Seen {
    /// Pieces the peer has sent at their right length, true bytes or not:
    /// what a fetch may have taken as arrived.
    served: HashSet<usize>,
    /// Pieces the fetch asked for, in order.
    asked: Vec<usize>,
    /// The reservations asked for: `(now, total)`.
    reserved: Vec<(usize, usize)>,
    /// The size the peer announced first, if it was one a fetch accepts.
    announced: Option<usize>,
    wrong: Vec<String>,
}

type Shared = Arc<Mutex<Seen>>;

fn dict_size(rng: &mut Rng) -> usize {
    match rng.below(16) {
        0 => 1 + rng.below(200),
        1 => PIECE - 1 + rng.below(3),
        2 => 2 * PIECE - 1 + rng.below(3),
        3 => FIRST_PIECES * PIECE - 1 + rng.below(3),
        4 => (FIRST_PIECES + 1) * PIECE + rng.below(PIECE),
        5 => WINDOW * PIECE + rng.below(3 * PIECE),
        6 => MAX_METADATA - rng.below(2 * PIECE),
        _ => 1 + rng.below(8 * PIECE),
    }
}

fn frame(id: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 5);
    out.extend_from_slice(&(body.len() as u32 + 1).to_be_bytes());
    out.push(id);
    out.extend_from_slice(body);
    out
}

fn extended(ext_id: u8, payload: &[u8]) -> Vec<u8> {
    let mut body = vec![ext_id];
    body.extend_from_slice(payload);
    frame(EXTENDED, &body)
}

/// The peer's extended handshake; `None` leaves a field out.
fn ext_handshake(ut_metadata: Option<u8>, size: Option<i64>, client: Option<&[u8]>) -> Vec<u8> {
    let mut m = BTreeMap::new();
    if let Some(id) = ut_metadata {
        m.insert(b"ut_metadata".to_vec(), OwnedValue::Int(i64::from(id)));
    }
    let mut d = BTreeMap::new();
    d.insert(b"m".to_vec(), OwnedValue::Dict(m));
    if let Some(size) = size {
        d.insert(b"metadata_size".to_vec(), OwnedValue::Int(size));
    }
    if let Some(client) = client {
        d.insert(b"v".to_vec(), OwnedValue::Bytes(client.to_vec()));
    }
    extended(0, &to_vec(&OwnedValue::Dict(d)))
}

/// A `ut_metadata` data message; `total` of `None` leaves `total_size` out.
fn data(piece: i64, total: Option<i64>, bytes: &[u8]) -> Vec<u8> {
    let mut d = BTreeMap::new();
    d.insert(b"msg_type".to_vec(), OwnedValue::Int(1));
    d.insert(b"piece".to_vec(), OwnedValue::Int(piece));
    if let Some(total) = total {
        d.insert(b"total_size".to_vec(), OwnedValue::Int(total));
    }
    let mut out = to_vec(&OwnedValue::Dict(d));
    out.extend_from_slice(bytes);
    out
}

fn pex_peers(rng: &mut Rng) -> Vec<SocketAddr> {
    (0..rng.below(120))
        .map(|_| {
            // A small space, so lists overlap.
            let ip = [203, 0, 113, rng.below(250) as u8 + 1];
            SocketAddr::from((ip, 6881 + rng.below(3) as u16))
        })
        .collect()
}

struct Peer {
    rng: Rng,
    hostile: bool,
    out: WriteHalf<DuplexStream>,
    asked: mpsc::UnboundedReceiver<usize>,
    seen: Shared,
    info: Arc<Vec<u8>>,
    key: InfoHash,
    /// The id the fetch must use for `ut_metadata` messages to this peer.
    their_id: u8,
    client: Vec<u8>,
    pex_sent: HashSet<SocketAddr>,
    /// Asked for and not yet served.
    open: Vec<usize>,
    done: HashSet<usize>,
}

impl Peer {
    fn pieces(&self) -> usize {
        self.info.len().div_ceil(PIECE)
    }

    fn piece(&self, i: usize) -> &[u8] {
        &self.info[i * PIECE..((i + 1) * PIECE).min(self.info.len())]
    }

    /// Sends a data message, noting the piece as served first if a fetch
    /// may take it: a piece of the right length for the size announced.
    /// (Whether the fetch has asked for it yet isn't known here: its
    /// requests may still be on their way to the test. Noting too much
    /// only loosens the checks; the fetch can't have a piece sooner than
    /// its note.)
    async fn send_data(&mut self, piece: i64, total: Option<i64>, bytes: &[u8]) -> bool {
        {
            let mut s = self.seen.lock().unwrap();
            let fits = usize::try_from(piece).ok().and_then(|p| {
                let size = s.announced?;
                let start = p.checked_mul(PIECE)?;
                (start < size && bytes.len() == (size - start).min(PIECE)).then_some(p)
            });
            if let Some(p) = fits {
                s.served.insert(p);
            }
        }
        let msg = data(piece, total, bytes);
        self.send(&extended(message::OUR_UT_METADATA, &msg)).await
    }

    /// `false` once the fetch has gone.
    async fn send(&mut self, bytes: &[u8]) -> bool {
        self.out.write_all(bytes).await.is_ok()
    }

    /// A message a fetch must put up with at any time after the BEP 3
    /// handshake.
    async fn harmless(&mut self) -> bool {
        match self.rng.below(5) {
            0 => self.send(&[0, 0, 0, 0]).await,
            1 => {
                // Any message that isn't extended: a bitfield, a have...
                let id = [0u8, 1, 2, 4, 5, 9][self.rng.below(6)];
                let n = self.rng.below(6000);
                let body = self.rng.bytes(n);
                self.send(&frame(id, &body)).await
            }
            2 => {
                let id = 3 + self.rng.below(250) as u8;
                let n = 1 + self.rng.below(300);
                let junk = self.rng.bytes(n);
                self.send(&extended(id, &junk)).await
            }
            _ => {
                let peers = pex_peers(&mut self.rng);
                self.pex_sent.extend(peers.iter().copied());
                self.send(&extended(
                    message::OUR_UT_PEX,
                    &message::pex_message(&peers),
                ))
                .await
            }
        }
    }

    /// Something a fetch must put up with once the peer has announced its
    /// size.
    async fn harmless_later(&mut self) -> bool {
        match self.rng.below(4) {
            0 => {
                // A handshake again, saying something else: it changes nothing.
                let size = Some(1 + self.rng.below(MAX_METADATA) as i64);
                self.send(&ext_handshake(Some(9), size, Some(b"other")))
                    .await
            }
            1 => {
                let piece = self.rng.below(self.pieces() + 2);
                let ask = message::metadata_request(piece);
                self.send(&extended(message::OUR_UT_METADATA, &ask)).await
            }
            2 if !self.done.is_empty() => {
                // A piece again. Its bytes no longer matter: the first kept.
                let mut done: Vec<usize> = self.done.iter().copied().collect();
                done.sort_unstable();
                let i = done[self.rng.below(done.len())];
                let bytes = self.rng.bytes(self.piece(i).len());
                self.send_data(i as i64, None, &bytes).await
            }
            _ => self.harmless().await,
        }
    }

    /// One break of the protocol. `true`: keep going afterwards.
    async fn breach(&mut self) -> bool {
        let size = self.info.len() as i64;
        let ut = message::OUR_UT_METADATA;
        match self.rng.below(9) {
            0 => {
                // A piece that wasn't asked for.
                let asked = self.seen.lock().unwrap().asked.len();
                let i = asked + self.rng.below(3);
                let bytes = self.rng.bytes(PIECE);
                self.send_data(i as i64, Some(size), &bytes).await
            }
            1 => {
                // A piece of the wrong length.
                let n = self.rng.below(PIECE + 600);
                let bytes = self.rng.bytes(n);
                self.send_data(0, Some(size), &bytes).await
            }
            2 => {
                let other = size + 1 + self.rng.below(5) as i64;
                let bytes = self.piece(0).to_vec();
                self.send_data(0, Some(other), &bytes).await
            }
            3 => {
                let no = message::metadata_reject(self.rng.below(4));
                self.send(&extended(ut, &no)).await
            }
            4 => {
                let n = 1 + self.rng.below(200);
                let junk = self.rng.bytes(n);
                self.send(&extended(ut, &junk)).await
            }
            5 => {
                // An extended message past any metadata piece: the length
                // alone, then whatever follows.
                let len = (PIECE + 1024 + 2 + self.rng.below(1 << 20)) as u32;
                let mut head = len.to_be_bytes().to_vec();
                head.extend_from_slice(&[EXTENDED, ut]);
                self.send(&head).await
            }
            6 => {
                let len = (4u32 << 20) + 2 + self.rng.below(1 << 20) as u32;
                let mut head = len.to_be_bytes().to_vec();
                head.push(5);
                self.send(&head).await
            }
            7 => {
                let n = 1 + self.rng.below(64);
                let junk = self.rng.bytes(n);
                self.send(&junk).await
            }
            _ => {
                let i = self.rng.below(self.pieces()) as i64;
                self.send_data(-1 - i, Some(size), &[]).await
            }
        }
    }

    async fn serve(&mut self) {
        // The BEP 3 handshake.
        let hello = if self.hostile && self.rng.chance(12) {
            match self.rng.below(3) {
                0 => message::handshake(&[0x5A; 20], b"-XX0001-abcdefghijkl", true).to_vec(),
                1 => message::handshake(&self.key.0, b"-XX0001-abcdefghijkl", false).to_vec(),
                _ => self.rng.bytes(message::HANDSHAKE_LEN),
            }
        } else {
            message::handshake(&self.key.0, b"-XX0001-abcdefghijkl", true).to_vec()
        };
        if !self.send(&hello).await {
            return;
        }
        for _ in 0..self.rng.below(4) {
            if self.hostile && self.rng.chance(10) {
                if !self.breach().await {
                    return;
                }
            } else if !self.harmless().await {
                return;
            }
        }
        // The extended handshake.
        let size = self.info.len() as i64;
        let announce = if self.hostile && self.rng.chance(6) {
            match self.rng.below(7) {
                0 => ext_handshake(None, Some(size), Some(&self.client)),
                1 => ext_handshake(Some(self.their_id), None, Some(&self.client)),
                2 => ext_handshake(Some(self.their_id), Some(0), None),
                3 => ext_handshake(Some(self.their_id), Some(-size), None),
                4 => ext_handshake(Some(self.their_id), Some(MAX_METADATA as i64 + 1), None),
                5 => ext_handshake(Some(self.their_id), Some(i64::MAX), None),
                _ => {
                    // A size that isn't the dict's.
                    let lie = 1 + self.rng.below(MAX_METADATA);
                    self.seen.lock().unwrap().announced = Some(lie);
                    ext_handshake(Some(self.their_id), Some(lie as i64), Some(&self.client))
                }
            }
        } else {
            self.seen.lock().unwrap().announced = Some(self.info.len());
            ext_handshake(Some(self.their_id), Some(size), Some(&self.client))
        };
        if !self.send(&announce).await {
            return;
        }
        // Serve what's asked, in any order, among other messages.
        let total = self.pieces();
        while self.done.len() < total {
            if self.hostile && self.rng.chance(40) {
                return; // gone, mid-fetch
            }
            if self.hostile && self.rng.chance(25) {
                if !self.breach().await {
                    return;
                }
                continue;
            }
            if self.rng.chance(4) {
                if !self.harmless_later().await {
                    return;
                }
                continue;
            }
            while let Ok(i) = self.asked.try_recv() {
                self.open.push(i);
            }
            if self.open.is_empty() && self.hostile {
                // After a breach or a lie the fetch may never ask again,
                // and it has no deadline of its own here: let it run, and
                // leave if it still wants nothing.
                for _ in 0..YIELDS {
                    tokio::task::yield_now().await;
                    if let Ok(i) = self.asked.try_recv() {
                        self.open.push(i);
                        break;
                    }
                }
                if self.open.is_empty() {
                    return;
                }
            } else if self.open.is_empty() {
                // An honest peer waits to be asked; the fetch has gone if
                // the channel closes.
                match self.asked.recv().await {
                    Some(i) => self.open.push(i),
                    None => return,
                }
            }
            let at = self.rng.below(self.open.len());
            let i = self.open.swap_remove(at);
            if i >= total {
                continue; // asked past a size the peer lied about
            }
            let mut bytes = self.piece(i).to_vec();
            let flipped = self.hostile && self.rng.chance(30);
            if flipped {
                let at = self.rng.below(bytes.len());
                bytes[at] ^= 0x40;
            }
            let with_total = if self.rng.chance(2) { Some(size) } else { None };
            self.done.insert(i);
            if !self.send_data(i as i64, with_total, &bytes).await {
                return;
            }
        }
        // The fetch has all it asked for. Stay until it goes (a hostile
        // peer may have spoiled the dict; then closing ends the fetch).
        if self.hostile {
            return;
        }
        while self.asked.recv().await.is_some() {}
    }
}

/// Reads all the fetch sends, checks its requests and passes them on.
async fn watch(
    mut from: ReadHalf<DuplexStream>,
    their_id: u8,
    seen: Shared,
    asked: mpsc::UnboundedSender<usize>,
) {
    let wrong = |seen: &Shared, what: String| seen.lock().unwrap().wrong.push(what);
    let mut hello = [0u8; message::HANDSHAKE_LEN];
    if from.read_exact(&mut hello).await.is_err() {
        return;
    }
    if message::parse_handshake(&hello).is_err() {
        wrong(&seen, "the fetch's handshake doesn't parse".into());
    }
    loop {
        let mut len = [0u8; 4];
        if from.read_exact(&mut len).await.is_err() {
            return;
        }
        let len = u32::from_be_bytes(len) as usize;
        if len > PIECE {
            wrong(&seen, format!("the fetch sent a message of {len} bytes"));
            return;
        }
        let mut body = vec![0u8; len];
        if from.read_exact(&mut body).await.is_err() {
            return;
        }
        if body.len() < 2 || body[0] != EXTENDED || body[1] != their_id {
            continue; // its own handshake (id 0), or nothing we check
        }
        let Ok(msg) = message::parse_metadata_message(&body[2..]) else {
            wrong(
                &seen,
                "the fetch sent a ut_metadata message that doesn't parse".into(),
            );
            continue;
        };
        let message::MetadataMessage::Request { piece } = msg else {
            continue; // its reject of the peer's own request
        };
        let mut s = seen.lock().unwrap();
        let piece = usize::try_from(piece).unwrap_or(usize::MAX);
        if piece != s.asked.len() {
            let n = s.asked.len();
            s.wrong
                .push(format!("piece {piece} asked for when {n} was next"));
        }
        match s.announced {
            Some(size) if piece < size.div_ceil(PIECE) => {}
            other => s
                .wrong
                .push(format!("piece {piece} asked for, announced {other:?}")),
        }
        s.asked.push(piece);
        if s.asked.len() > s.served.len() + WINDOW {
            let (a, d) = (s.asked.len(), s.served.len());
            s.wrong
                .push(format!("{a} pieces asked for with {d} served"));
        }
        // Past the first window only with the whole result reserved (or
        // no reservation hook at all: `reserved` stays empty).
        let whole = s.reserved.last().is_none_or(|(now, total)| now == total);
        if piece >= FIRST_PIECES && !whole {
            s.wrong.push(format!(
                "piece {piece} asked for before the whole was reserved"
            ));
        }
        drop(s);
        let _ = asked.send(piece);
    }
}

fn reserve_hook(seen: Shared) -> Reserve {
    Reserve(Arc::new(move |now, total| {
        let mut s = seen.lock().unwrap();
        if now > total {
            s.wrong.push(format!("reserved {now} of {total}"));
        }
        match (s.reserved.len(), s.announced) {
            (0, Some(_)) => {}
            (1, Some(size)) => {
                // The second step: the whole, once the first window is in.
                let window = size.div_ceil(PIECE).min(FIRST_PIECES);
                let first = s.reserved[0];
                if now != total || total != first.1 || first.0 == first.1 {
                    s.wrong.push(format!(
                        "second reservation ({now}, {total}) after {first:?}"
                    ));
                }
                if !(0..window).all(|i| s.served.contains(&i)) {
                    s.wrong
                        .push("the whole was reserved before the first window came".into());
                }
            }
            (n, announced) => s
                .wrong
                .push(format!("reservation {n}, announced {announced:?}")),
        }
        s.reserved.push((now, total));
        Box::pin(async {})
    }))
}

async fn run(seed: u64) {
    let mut rng = Rng::new(seed);
    let hostile = rng.chance(2);
    let size = dict_size(&mut rng);
    let info = Arc::new(rng.bytes(size));
    let v2 = rng.chance(4);
    let key = if v2 {
        let d = Sha256::digest(info.as_slice());
        InfoHash(d[..20].try_into().unwrap())
    } else {
        InfoHash(Sha1::digest(info.as_slice()).into())
    };
    let their_id = 1 + rng.below(255) as u8;
    let client = {
        let n = rng.below(90);
        rng.bytes(n)
    };
    let seen: Shared = Shared::default();
    let with_reserve = !rng.chance(5);
    let opts = FetchOptions {
        max_metadata: MAX_METADATA,
        encryption: Encryption::Disable,
        reserve: with_reserve.then(|| reserve_hook(seen.clone())),
        ..FetchOptions::default()
    };

    let (ours, theirs) = tokio::io::duplex(1 + rng.below(64 << 10));
    let (from_fetch, to_fetch) = tokio::io::split(theirs);
    let (asked_tx, asked_rx) = mpsc::unbounded_channel();
    let watcher = tokio::spawn(watch(from_fetch, their_id, seen.clone(), asked_tx));
    let peer = Peer {
        rng: Rng::new(seed ^ 0xD1B5_4A32_D192_ED03),
        hostile,
        out: to_fetch,
        asked: asked_rx,
        seen: seen.clone(),
        info: info.clone(),
        key,
        their_id,
        client: client.clone(),
        pex_sent: HashSet::new(),
        open: Vec::new(),
        done: HashSet::new(),
    };
    let peer = tokio::spawn(async move {
        let mut peer = peer;
        peer.serve().await;
        // Closing its side is how the peer leaves: the fetch reads the end.
        let _ = peer.out.shutdown().await;
        peer.pex_sent
    });

    let got = tokio::time::timeout(STUCK, fetch_on_stream(ours, &key, &opts, false))
        .await
        .unwrap_or_else(|_| panic!("seed {seed}: the fetch didn't end (hostile: {hostile})"));
    let pex_sent = tokio::time::timeout(STUCK, peer)
        .await
        .unwrap_or_else(|_| panic!("seed {seed}: the peer didn't end"))
        .unwrap_or_else(|e| panic!("seed {seed}: the peer failed: {e}"));
    tokio::time::timeout(STUCK, watcher)
        .await
        .unwrap_or_else(|_| panic!("seed {seed}: the fetch's stream stayed open"))
        .unwrap_or_else(|e| panic!("seed {seed}: the watcher failed: {e}"));

    let s = seen.lock().unwrap();
    assert!(
        s.wrong.is_empty(),
        "seed {seed} (hostile: {hostile}): {:?}",
        &s.wrong[..s.wrong.len().min(4)]
    );
    match &got {
        Ok(f) => check_result(seed, f, &info, v2, &client, &pex_sent, &s),
        Err(e) => assert!(hostile, "seed {seed}: an honest peer's fetch failed: {e:?}"),
    }
}

fn check_result(
    seed: u64,
    f: &Fetched,
    info: &[u8],
    v2: bool,
    client: &[u8],
    pex_sent: &HashSet<SocketAddr>,
    s: &Seen,
) {
    assert!(
        f.info == info,
        "seed {seed}: a fetch succeeded with other bytes"
    );
    assert_eq!(f.v2, v2, "seed {seed}");
    assert!(!f.encrypted, "seed {seed}");
    let name = String::from_utf8_lossy(&client[..client.len().min(64)]).into_owned();
    assert_eq!(f.client.as_deref(), Some(name.as_str()), "seed {seed}");
    assert!(
        f.pex.len() <= MAX_PEX,
        "seed {seed}: {} PEX peers",
        f.pex.len()
    );
    let distinct: HashSet<&SocketAddr> = f.pex.iter().collect();
    assert_eq!(distinct.len(), f.pex.len(), "seed {seed}: a PEX peer twice");
    assert!(
        f.pex.iter().all(|p| pex_sent.contains(p)),
        "seed {seed}: a PEX peer nobody sent"
    );
    if let Some((_, total)) = s.reserved.last() {
        let held = f.info.capacity()
            + f.pex.capacity() * std::mem::size_of::<SocketAddr>()
            + f.client.as_ref().map_or(0, String::capacity);
        assert!(
            held <= *total,
            "seed {seed}: the result holds {held}, reserved {total}"
        );
    }
    assert_eq!(
        s.asked.len(),
        info.len().div_ceil(PIECE),
        "seed {seed}: pieces asked for"
    );
}

#[tokio::test(start_paused = true)]
async fn fetches_end_right_whatever_the_peer_sends() {
    for seed in SEEDS {
        run(seed).await;
    }
}
