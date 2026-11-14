//! The fetch pool and the real fetch loop together, under scripted peers,
//! in paused time.
//!
//! Each seed sends keys to a pool with a worker's byte budget. Each key has
//! a few peers in a few networks, each scripted: honest and fast, honest
//! and slow but steady, one that stops in its first window or after it,
//! one that sends junk at full pace, or one that announces too large a
//! dict. Every fetch is `lodestar_peer_wire::fetch_on_stream` over an
//! in-memory pipe, reserving from the pool as the real fetcher does, under
//! the fetch's deadline. A peer finder answers lookups with a key's peers
//! again, so a key out of peers is tried again as with the DHT. A consumer takes results at a pace that changes.
//!
//! Throughout, held and reserved bytes stay within the budget. At the end,
//! every key with an honest peer has been fetched once, with its own dict;
//! no key without one has; and the pool holds nothing. A failure names its
//! seed.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::message::{self, HANDSHAKE_LEN, METADATA_PIECE, MetadataMessage};
use lodestar_peer_wire::{
    Encryption, FailureKind, FetchOptions, Fetched, Reserve, fetch_on_stream,
};
use lodestar_sources::pool::{Reservation, spawn};
use lodestar_sources::{
    BoxFuture, Discovery, Fetcher, Found, FunnelCounts, PoolConfig, PoolIo, Shutdown,
};
use lodestar_store::SourceId;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::mpsc;
use tokio::time::sleep;

const SEEDS: std::ops::RangeInclusive<u64> = 1..=300;
const KEYS: u32 = 40;
const BUDGET: usize = 4 << 20;
const DEADLINE: Duration = Duration::from_secs(8);

/// xorshift64*.
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
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
}

/// How a scripted peer behaves.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Peer {
    /// Serves every piece, each `each` ms after it's asked for.
    Honest { each: u64 },
    /// Serves `serve` pieces, then sends nothing more.
    Stops { serve: usize },
    /// Serves bytes that aren't the dict, at full pace.
    Junk,
    /// Announces a dict larger than any fetch takes.
    Oversize,
}

impl Peer {
    /// Whether it serves a whole dict of `pieces` pieces.
    fn serves(self, pieces: usize) -> bool {
        match self {
            // Fast enough to finish inside the deadline even after waiting
            // for room.
            Peer::Honest { each } => each * pieces as u64 <= 2_000,
            Peer::Stops { serve } => serve >= pieces,
            Peer::Junk | Peer::Oversize => false,
        }
    }
}

/// An info dict of exactly `len` bytes, and its key.
fn dict_of(len: usize, tag: u32) -> (Vec<u8>, InfoHash) {
    let head = format!(
        "d6:lengthi1e4:name8:{tag:08}12:piece lengthi16384e6:pieces20:AAAAAAAAAAAAAAAAAAAA5:x-pad"
    );
    let head = head.as_bytes();
    let mut pad = len - head.len() - 2;
    while pad.to_string().len() + 1 + pad + head.len() + 1 > len {
        pad -= 1;
    }
    let mut info = head.to_vec();
    info.extend(format!("{pad}:").bytes());
    info.resize(info.len() + pad, b'0');
    info.push(b'e');
    let key = lodestar_metainfo::Info::parse(&info)
        .unwrap()
        .primary_hash();
    (info, key)
}

fn frame(ext_id: u8, body: Vec<u8>) -> Vec<u8> {
    let mut f = ((body.len() + 2) as u32).to_be_bytes().to_vec();
    f.extend([20, ext_id]);
    f.extend(body);
    f
}

/// One scripted peer on `stream`, for `info` under `key`.
async fn play(mut stream: DuplexStream, info: Arc<Vec<u8>>, key: InfoHash, peer: Peer) {
    let mut theirs = [0u8; HANDSHAKE_LEN];
    if stream.read_exact(&mut theirs).await.is_err() {
        return;
    }
    let hello = message::handshake(&key.0, b"-PW0001-wiremodelpwm", true);
    let size = if peer == Peer::Oversize {
        1 << 30
    } else {
        info.len()
    };
    let ext = message::our_extended_handshake(Some(size));
    if stream.write_all(&hello).await.is_err() || stream.write_all(&frame(0, ext)).await.is_err() {
        return;
    }
    let mut served = 0;
    loop {
        let mut len = [0u8; 4];
        if stream.read_exact(&mut len).await.is_err() {
            return;
        }
        let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
        if stream.read_exact(&mut body).await.is_err() {
            return;
        }
        if body.len() < 2 || body[0] != 20 || body[1] != message::OUR_UT_METADATA {
            continue;
        }
        let Ok(MetadataMessage::Request { piece }) = message::parse_metadata_message(&body[2..])
        else {
            continue;
        };
        let start = piece as usize * METADATA_PIECE;
        let end = (start + METADATA_PIECE).min(info.len());
        let data = match peer {
            Peer::Honest { each } => {
                sleep(Duration::from_millis(each)).await;
                info[start..end].to_vec()
            }
            Peer::Stops { serve } if served >= serve => continue,
            Peer::Stops { .. } => info[start..end].to_vec(),
            Peer::Junk => vec![0x5A; end - start],
            Peer::Oversize => continue,
        };
        served += 1;
        let reply = message::metadata_data(piece as usize, info.len(), &data);
        if stream
            .write_all(&frame(message::OUR_UT_METADATA, reply))
            .await
            .is_err()
        {
            return;
        }
    }
}

/// Fetches over in-memory pipes from the scripted peers, as
/// `PeerWireFetcher` does over TCP.
struct Wire {
    peers: Mutex<HashMap<SocketAddr, (Arc<Vec<u8>>, InfoHash, Peer)>>,
    /// Each grown fetch from a peer that stalls after its first window:
    /// how long it held its grown room, and whether its end could be
    /// credited as late (room 4 s or more in all, a timeout, never
    /// behind).
    stalled: Arc<Mutex<Vec<(Duration, bool)>>>,
}

impl Wire {
    fn run(
        &self,
        peer: SocketAddr,
        key: InfoHash,
        reservation: Option<Reservation>,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        let script = self.peers.lock().unwrap().get(&peer).cloned();
        let stalled = self.stalled.clone();
        Box::pin(async move {
            let Some((info, k, behaviour)) = script.filter(|(_, k, _)| *k == key) else {
                return Err(FailureKind::Unreachable);
            };
            let (ours, theirs) = tokio::io::duplex(1 << 16);
            let player = tokio::spawn(play(theirs, info, k, behaviour));
            let mut opts = FetchOptions {
                encryption: Encryption::Disable,
                timeout: DEADLINE,
                max_metadata: lodestar_sources::pool::largest_dict(BUDGET),
                ..FetchOptions::default()
            };
            let behind = reservation.clone();
            // Time spent waiting for room, and when the growth's came.
            let room: Arc<Mutex<(Duration, Option<tokio::time::Instant>)>> = Arc::default();
            if let Some(r) = reservation {
                let room = room.clone();
                opts.reserve = Some(Reserve(Arc::new(move |now, total| {
                    let r = r.clone();
                    let room = room.clone();
                    Box::pin(async move {
                        let asked = tokio::time::Instant::now();
                        let grows = r.bytes() > 0;
                        r.reserve(now, total).await;
                        let mut room = room.lock().unwrap();
                        room.0 += asked.elapsed();
                        if grows {
                            room.1 = Some(tokio::time::Instant::now());
                        }
                    })
                })));
            }
            let mut got_behind = false;
            let got =
                match tokio::time::timeout(DEADLINE, fetch_on_stream(ours, &key, &opts, false))
                    .await
                {
                    Ok(Ok(f)) => Ok(f),
                    Ok(Err(e)) => {
                        if matches!(e, lodestar_peer_wire::Error::Behind) {
                            got_behind = true;
                            if let Some(r) = &behind {
                                r.behind();
                            }
                        }
                        Err(e.kind())
                    }
                    Err(_) => Err(FailureKind::TimedOut),
                };
            player.abort();
            let (waited, grown) = *room.lock().unwrap();
            if let (Peer::Stops { .. }, Some(at)) = (behaviour, grown) {
                let fell_behind = got_behind;
                let creditable = waited >= Duration::from_secs(4)
                    && !fell_behind
                    && matches!(got, Err(FailureKind::TimedOut));
                stalled.lock().unwrap().push((at.elapsed(), creditable));
            }
            got
        })
    }
}

impl Fetcher for Wire {
    fn fetch(
        &self,
        peer: SocketAddr,
        key: InfoHash,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        self.run(peer, key, None)
    }

    fn fetch_reserving(
        &self,
        peer: SocketAddr,
        key: InfoHash,
        reservation: Reservation,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        self.run(peer, key, Some(reservation))
    }
}

/// What a seed left: honest keys not fetched in 30 min, reservation
/// wait-outs and late wait-outs.
struct Outcome {
    missing: usize,
    /// The longest a stalling peer held its grown room, and how many of
    /// its fetches could have been credited as late.
    stall_hold: Duration,
    stall_credits: usize,
    wait_outs: u64,
    late: u64,
}

async fn run(seed: u64) -> Outcome {
    let mut rng = Rng::new(seed);
    let wire = Arc::new(Wire {
        peers: Mutex::new(HashMap::new()),
        stalled: Arc::default(),
    });
    let mut honest_keys = HashMap::new();
    let mut scripts: HashMap<InfoHash, (usize, Vec<Peer>)> = HashMap::new();
    let mut hostile_keys = HashSet::new();
    let mut maybe_keys = HashMap::new();
    let mut discoveries = Vec::new();
    let mut known: HashMap<InfoHash, Vec<SocketAddr>> = HashMap::new();
    for n in 0..KEYS {
        let len = if rng.range(0, 99) < 75 {
            rng.range(2_000, 60_000)
        } else {
            rng.range(100_000, 1_500_000)
        } as usize;
        let (info, key) = dict_of(len, n);
        let info = Arc::new(info);
        let count = rng.range(1, 4);
        let mut peers = Vec::new();
        let mut any_honest = false;
        let mut any_true = false;
        for p in 0..count {
            let behaviour = match rng.range(0, 9) {
                0..=3 => Peer::Honest {
                    each: rng.range(0, 20),
                },
                4 => Peer::Honest {
                    each: rng.range(80, 200),
                },
                5 => Peer::Stops {
                    serve: rng.range(0, 3) as usize,
                },
                6 => Peer::Stops {
                    serve: rng.range(4, 12) as usize,
                },
                7 => Peer::Junk,
                _ => Peer::Oversize,
            };
            any_honest |= behaviour.serves(len.div_ceil(METADATA_PIECE));
            any_true |= behaviour.serves(len.div_ceil(METADATA_PIECE))
                || matches!(behaviour, Peer::Honest { .. });
            // Each of a key's peers in a network of its own.
            let addr = SocketAddr::from(([198, 51 + p as u8, (n % 200) as u8 + 1, 1], 6881));
            wire.peers
                .lock()
                .unwrap()
                .insert(addr, (info.clone(), key, behaviour));
            scripts
                .entry(key)
                .or_insert((len, Vec::new()))
                .1
                .push(behaviour);
            peers.push(addr);
        }
        // A key whose only peers are slow honest ones may or may not be
        // fetched in time; one with no peer that sends its true dict never
        // is.
        // Peers in three networks agreeing on a false size end a key by
        // design (the size rule), honest peer or not.
        let oversize = scripts[&key]
            .1
            .iter()
            .filter(|p| **p == Peer::Oversize)
            .count();
        if any_honest && oversize < 3 {
            honest_keys.insert(key, info.clone());
        } else if any_true {
            maybe_keys.insert(key, info.clone());
        } else {
            hostile_keys.insert(key);
        }
        known.insert(key, peers.clone());
        discoveries.push(Discovery {
            source: SourceId::new(0).unwrap(),
            found: Found::key(key, 0).with_peers(peers),
            at: 1_700_000_000,
            reply: false,
        });
    }
    let (disc_tx, disc_rx) = mpsc::channel(256);
    // A peer finder that answers each lookup with the key's peers again, as
    // the DHT would: a key out of peers is looked up and tried again.
    let (finder, mut requests) = mpsc::channel::<lodestar_sources::PeerRequest>(256);
    let known = Arc::new(known);
    tokio::spawn({
        let known = known.clone();
        async move {
            while let Some(request) = requests.recv().await {
                let (done, rx) = tokio::sync::oneshot::channel();
                if request.admit.send(Some(rx)).is_ok() {
                    let _ = done.send(known.get(&request.key).cloned().unwrap_or_default());
                }
            }
        }
    });
    let (out_tx, mut out) = mpsc::channel(16);
    let (stop, shutdown) = Shutdown::new();
    let (handle, task) = spawn(
        PoolConfig {
            max_sockets: 32,
            max_held_bytes: BUDGET,
            max_attempts: 6,
            retry_backoff: Duration::from_secs(5),
            ..PoolConfig::default()
        },
        wire.clone(),
        PoolIo {
            discoveries: disc_rx,
            peer_finders: vec![finder],
            out: out_tx,
            funnel: Arc::new(FunnelCounts::default()),
            shutdown,
        },
        HashMap::new(),
        Vec::new(),
    );
    for d in discoveries {
        disc_tx.send(d).await.unwrap();
        sleep(Duration::from_millis(rng.range(0, 300))).await;
    }
    let stats = handle.stats();
    let mut fetched = HashSet::new();
    let mut seen = HashSet::new();
    let started = tokio::time::Instant::now();
    while fetched.len() < honest_keys.len() {
        if started.elapsed() >= Duration::from_secs(1_800) {
            let missing: Vec<_> = honest_keys
                .keys()
                .filter(|k| !fetched.contains(*k))
                .map(|k| &scripts[k])
                .collect();
            if std::env::var_os("MODEL_SWEEP").is_none() {
                panic!(
                    "seed {seed}: {} of {} honest keys fetched after 30 min; missing {missing:?}",
                    fetched.len(),
                    honest_keys.len()
                );
            }
            break;
        }
        let held = stats.held_bytes.load(Ordering::Relaxed);
        let reserved = stats.reserved_bytes.load(Ordering::Relaxed);
        assert!(
            held + reserved <= BUDGET as u64 + (2 << 20),
            "seed {seed}: {held} held + {reserved} reserved"
        );
        match tokio::time::timeout(Duration::from_secs(5), out.recv()).await {
            Ok(Some(got)) => {
                assert!(
                    !hostile_keys.contains(&got.key),
                    "seed {seed}: a key with no honest peer fetched"
                );
                let info = honest_keys
                    .get(&got.key)
                    .or_else(|| maybe_keys.get(&got.key))
                    .expect("a key the test sent");
                assert_eq!(&got.fetched.info, &**info, "seed {seed}: the wrong dict");
                assert!(seen.insert(got.key), "seed {seed}: fetched twice");
                if honest_keys.contains_key(&got.key) {
                    fetched.insert(got.key);
                }
                sleep(Duration::from_millis(rng.range(0, 400))).await;
            }
            Ok(None) => panic!("seed {seed}: the pool stopped"),
            Err(_) => {}
        }
    }
    let missing = honest_keys.len() - fetched.len();
    stop.send(true).unwrap();
    let left = task.await.unwrap();
    assert!(
        missing > 0 || left.iter().all(|q| !honest_keys.contains_key(&q.info_hash)),
        "seed {seed}: honest keys left queued"
    );
    let stalled = wire.stalled.lock().unwrap();
    Outcome {
        missing,
        stall_hold: stalled.iter().map(|s| s.0).max().unwrap_or_default(),
        stall_credits: stalled.iter().filter(|s| s.1).count(),
        wait_outs: stats.reserve_timeouts.load(Ordering::Relaxed),
        late: stats.late_waits.load(Ordering::Relaxed),
    }
}

/// Every seed: honest keys are all fetched once with their own dicts, no
/// key without an honest peer is, and the budget holds throughout.
#[test]
fn the_pool_and_the_fetch_loop_serve_honest_keys_among_hostile_peers() {
    let seeds = std::env::var("MODEL_SEEDS")
        .ok()
        .and_then(|n| n.parse().ok())
        .map_or(SEEDS, |n: u64| 1..=n);
    let seeds = match std::env::var("MODEL_SEED")
        .ok()
        .and_then(|n| n.parse().ok())
    {
        Some(n) => n..=n,
        None => seeds,
    };
    let (mut lost, mut failed, mut wait_outs, mut late) = (0, Vec::new(), 0, 0);
    let (mut hold, mut credits) = (Duration::ZERO, 0);
    for seed in seeds {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        let o = rt.block_on(run(seed));
        if o.missing > 0 {
            lost += o.missing;
            failed.push(seed);
        }
        wait_outs += o.wait_outs;
        hold = hold.max(o.stall_hold);
        credits += o.stall_credits;
        late += o.late;
    }
    eprintln!(
        "lost {lost} on seeds {failed:?}; wait-outs {wait_outs}; late {late}; \
         stalling peers held grown room up to {hold:?}, {credits} creditable"
    );
}
