//! End-to-end metadata fetches against the fake peer over real TCP.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_peer_wire::testing::{Accept, Behavior, FakePeer, Seed};
use lodestar_peer_wire::{Deny, Encryption, Error, FetchOptions, Fetched, Reserve, fetch, message};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// A realistic info dict of roughly `size` bytes (grown via file entries).
fn info_dict(size: usize) -> Vec<u8> {
    let mut files = Vec::new();
    let mut i = 0;
    while to_vec(&OwnedValue::List(files.clone())).len() < size {
        let mut f = std::collections::BTreeMap::new();
        f.insert(b"length".to_vec(), OwnedValue::Int(1000 + i));
        f.insert(
            b"path".to_vec(),
            OwnedValue::List(vec![OwnedValue::from(
                format!("Season 1/Episode {i:05}.mkv").as_str(),
            )]),
        );
        files.push(OwnedValue::Dict(f));
        i += 1;
    }
    let mut d = std::collections::BTreeMap::new();
    d.insert(b"files".to_vec(), OwnedValue::List(files));
    d.insert(b"name".to_vec(), OwnedValue::from("Test Pack"));
    d.insert(b"piece length".to_vec(), OwnedValue::Int(1 << 20));
    d.insert(b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20]));
    to_vec(&OwnedValue::Dict(d))
}

fn opts(encryption: Encryption) -> FetchOptions {
    FetchOptions {
        allow_private: true,
        timeout: Duration::from_secs(3),
        encryption,
        ..FetchOptions::default()
    }
}

/// The pipeline spawns fetches onto a multi-threaded runtime.
#[test]
fn fetch_future_is_send() {
    fn assert_send<T: Send>(_: &T) {}
    let o = opts(Encryption::Prefer);
    let f = fetch("127.0.0.1:1".parse().unwrap(), InfoHash([0; 20]), &o);
    assert_send(&f);
}

#[tokio::test]
async fn public_fetch_defaults_reject_local_and_mapped_destinations_before_dial() {
    let opts = FetchOptions::default();
    for addr in [
        "127.0.0.1:80",
        "[::ffff:127.0.0.1]:80",
        "[::1]:80",
        "10.1.2.3:80",
        "1.1.1.1:0",
    ] {
        assert!(matches!(
            fetch(addr.parse().unwrap(), InfoHash([1; 20]), &opts).await,
            Err(Error::Denied)
        ));
    }
    assert_eq!(opts.stats.attempts(lodestar_peer_wire::Transport::Tcp), 0);
}

#[tokio::test]
async fn fetches_across_encryption_combinations() {
    let info = info_dict(40_000); // three metadata pieces
    let seed = Seed::v1(info.clone());
    let hash = InfoHash(seed.info_hash);
    let cases = [
        (Accept::Both, Encryption::Prefer, true),
        (Accept::Both, Encryption::Disable, false),
        (Accept::Both, Encryption::Require, true),
        (Accept::EncryptedOnly, Encryption::Prefer, true),
        (Accept::EncryptedOnly, Encryption::Require, true),
        (Accept::PlaintextOnly, Encryption::Prefer, false), // falls back
        (Accept::PlaintextOnly, Encryption::Disable, false),
    ];
    for (accept, mode, encrypted) in cases {
        let peer = FakePeer::spawn(vec![seed.clone()], accept, Behavior::Honest, vec![]).await;
        let got = fetch(peer.addr(), hash, &opts(mode))
            .await
            .unwrap_or_else(|e| panic!("{accept:?}/{mode:?}: {e}"));
        assert_eq!(got.info, info, "{accept:?}/{mode:?}");
        assert!(!got.v2);
        assert_eq!(got.encrypted, encrypted, "{accept:?}/{mode:?}");
        assert_eq!(got.client.as_deref(), Some("FakePeer 1.0"));
        // The fetched dict is real metainfo.
        assert_eq!(Info::parse(&got.info).unwrap().primary_hash(), hash);
    }
}

#[tokio::test]
async fn impossible_combinations_fail_cleanly() {
    let seed = Seed::v1(info_dict(100));
    let hash = InfoHash(seed.info_hash);
    let peer = FakePeer::spawn(
        vec![seed.clone()],
        Accept::EncryptedOnly,
        Behavior::Honest,
        vec![],
    )
    .await;
    assert!(
        fetch(peer.addr(), hash, &opts(Encryption::Disable))
            .await
            .is_err()
    );
    let peer = FakePeer::spawn(vec![seed], Accept::PlaintextOnly, Behavior::Honest, vec![]).await;
    assert!(matches!(
        fetch(peer.addr(), hash, &opts(Encryption::Require)).await,
        Err(Error::Mse(_))
    ));
}

#[tokio::test]
async fn verifies_v2_keys_by_truncated_sha256() {
    let info = info_dict(500);
    let key: [u8; 20] = Sha256::digest(&info)[..20].try_into().unwrap();
    let seed = Seed {
        info: info.clone(),
        info_hash: key,
    };
    let peer = FakePeer::spawn(vec![seed], Accept::Both, Behavior::Honest, vec![]).await;
    let got = fetch(peer.addr(), InfoHash(key), &opts(Encryption::Prefer))
        .await
        .unwrap();
    assert!(got.v2);
    assert_eq!(got.info, info);
}

#[tokio::test]
async fn collects_pex_peers() {
    let seed = Seed::v1(info_dict(100));
    let hash = InfoHash(seed.info_hash);
    let pex: Vec<SocketAddr> = vec![
        "10.1.2.3:6881".parse().unwrap(),
        "[2001:db8::7]:51413".parse().unwrap(),
    ];
    let peer = FakePeer::spawn(vec![seed], Accept::Both, Behavior::Honest, pex.clone()).await;
    let got = fetch(peer.addr(), hash, &opts(Encryption::Prefer))
        .await
        .unwrap();
    assert_eq!(got.pex, pex);
}

#[tokio::test]
async fn hostile_and_broken_peers() {
    let seed = Seed::v1(info_dict(20_000));
    let hash = InfoHash(seed.info_hash);
    let run = |behavior| {
        let seed = seed.clone();
        async move {
            let peer = FakePeer::spawn(vec![seed], Accept::Both, behavior, vec![]).await;
            fetch(peer.addr(), hash, &opts(Encryption::Prefer)).await
        }
    };
    assert!(matches!(
        run(Behavior::WrongData).await,
        Err(Error::HashMismatch)
    ));
    assert!(matches!(run(Behavior::Reject).await, Err(Error::Rejected)));
    assert!(matches!(
        run(Behavior::NoExtensions).await,
        Err(Error::NoExtensions)
    ));
    assert!(matches!(
        run(Behavior::LieAboutSize(0)).await,
        Err(Error::MetadataSize(0))
    ));
    assert!(matches!(
        run(Behavior::LieAboutSize(1 << 40)).await,
        Err(Error::MetadataSize(_))
    ));
    // Claiming a smaller size makes the real pieces the wrong length.
    assert!(matches!(
        run(Behavior::LieAboutSize(1000)).await,
        Err(Error::Protocol(_))
    ));
    let started = std::time::Instant::now();
    assert!(matches!(run(Behavior::Stall).await, Err(Error::Timeout)));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "deadline enforced"
    );
}

#[tokio::test]
async fn unknown_hash_and_unreachable_peer() {
    let seed = Seed::v1(info_dict(100));
    let peer = FakePeer::spawn(vec![seed], Accept::Both, Behavior::Honest, vec![]).await;
    // A hash the peer doesn't have: it hangs up.
    assert!(
        fetch(peer.addr(), InfoHash([9; 20]), &opts(Encryption::Disable))
            .await
            .is_err()
    );
    // Nobody listening.
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    assert!(matches!(
        fetch(closed, InfoHash([9; 20]), &opts(Encryption::Disable)).await,
        Err(Error::Connect(_))
    ));
}

#[tokio::test]
async fn denied_addresses_are_never_connected_to() {
    let seed = Seed::v1(info_dict(100));
    let hash = InfoHash(seed.info_hash);
    let peer = FakePeer::spawn(vec![seed], Accept::Both, Behavior::Honest, vec![]).await;
    let mut o = opts(Encryption::Prefer);
    o.deny = Some(Deny(Arc::new(|ip| ip.is_loopback())));
    let err = fetch(peer.addr(), hash, &o).await.unwrap_err();
    assert!(matches!(err, Error::Denied), "{err}");
    o.deny = Some(Deny(Arc::new(|_| false)));
    assert!(fetch(peer.addr(), hash, &o).await.is_ok());
}

// X-fetch-meta-reserve: the fetch reserves its result's bytes once the
// size is announced, before it requests any piece.

/// A byte budget for the tests: a semaphore of one permit per byte. Each
/// fetch keeps what it reserved in `held` until the test drops it, as the
/// pool's claim does until its fetch ends.
#[derive(Clone)]
struct TestBudget {
    permits: Arc<Semaphore>,
    total: usize,
    /// Every call's `(now, total)`.
    calls: Arc<std::sync::Mutex<Vec<(usize, usize)>>>,
}

impl TestBudget {
    fn new(total: usize) -> TestBudget {
        TestBudget {
            permits: Arc::new(Semaphore::new(total)),
            total,
            calls: Arc::default(),
        }
    }

    fn reserved(&self) -> usize {
        self.total - self.permits.available_permits()
    }

    /// Options whose hook reserves into `held`, keeping the largest.
    fn opts(&self, held: Arc<std::sync::Mutex<Vec<OwnedSemaphorePermit>>>) -> FetchOptions {
        let budget = self.clone();
        let mut o = opts(Encryption::Disable);
        o.reserve = Some(Reserve(Arc::new(move |now, total| {
            let (budget, held) = (budget.clone(), held.clone());
            Box::pin(async move {
                budget.calls.lock().unwrap().push((now, total));
                let mine: usize = held.lock().unwrap().iter().map(|p| p.num_permits()).sum();
                let more = now.min(budget.total).saturating_sub(mine);
                if more > 0 {
                    let n = u32::try_from(more).unwrap();
                    let permit = budget.permits.clone().acquire_many_owned(n).await.unwrap();
                    held.lock().unwrap().push(permit);
                }
            })
        })));
        o
    }
}

/// An info dict of about `size` bytes, built in linear time: its
/// `pieces` string carries the bulk.
fn sized_dict(size: usize) -> Vec<u8> {
    let mut d = std::collections::BTreeMap::new();
    d.insert(b"length".to_vec(), OwnedValue::Int(size as i64));
    d.insert(b"name".to_vec(), OwnedValue::from("Large Pack"));
    d.insert(b"piece length".to_vec(), OwnedValue::Int(1 << 14));
    d.insert(
        b"pieces".to_vec(),
        OwnedValue::Bytes(vec![7; size / 20 * 20]),
    );
    to_vec(&OwnedValue::Dict(d))
}

/// The bytes a result's heap holds, as the pool weighs them.
fn heap(f: &Fetched) -> usize {
    f.info.capacity()
        + f.pex.capacity() * std::mem::size_of::<SocketAddr>()
        + f.client.as_ref().map_or(0, String::capacity)
}

/// Many peers announce dicts against a budget that fits two: the fetches
/// wait for room rather than download, what's reserved never passes the
/// budget, and each result fits what its fetch reserved. The dicts are
/// under the first window (64 KiB), so each reserves whole, once.
#[tokio::test]
async fn fetches_wait_for_room_and_stay_within_their_reservations() {
    const PEERS: usize = 8;
    let size = 50_000;
    let budget = TestBudget::new(2 * size + 2 * 16_384);
    let mut peers = Vec::new();
    for i in 0..PEERS {
        // Distinct dicts, so each peer serves its own key.
        let seed = Seed::v1(sized_dict(size + i));
        peers.push((
            FakePeer::spawn(vec![seed.clone()], Accept::Both, Behavior::Honest, vec![]).await,
            seed,
        ));
    }
    // The budget starts full, so every fetch must wait at its hook.
    let blocker = budget
        .permits
        .clone()
        .acquire_many_owned(budget.total as u32)
        .await
        .unwrap();
    let max = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for (peer, seed) in &peers {
        let (budget, max) = (budget.clone(), max.clone());
        let (addr, hash, want) = (peer.addr(), InfoHash(seed.info_hash), seed.info.clone());
        tasks.push(tokio::spawn(async move {
            let held = Arc::new(std::sync::Mutex::new(Vec::new()));
            let mut o = budget.opts(held.clone());
            o.timeout = Duration::from_secs(30);
            let got = fetch(addr, hash, &o).await.expect("fetched");
            max.fetch_max(budget.reserved(), std::sync::atomic::Ordering::SeqCst);
            let mine: usize = held.lock().unwrap().iter().map(|p| p.num_permits()).sum();
            assert_eq!(got.info, want);
            assert!(
                heap(&got) <= mine,
                "{} bytes over the {mine} reserved",
                heap(&got)
            );
            // Released as the fetch ends.
            drop(held);
        }));
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while budget.calls.lock().unwrap().len() < PEERS {
        assert!(
            std::time::Instant::now() < deadline,
            "every fetch reaches its hook"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(tasks.iter().all(|t| !t.is_finished()), "none downloads yet");
    drop(blocker);
    for t in tasks {
        t.await.unwrap();
    }
    assert!(max.load(std::sync::atomic::Ordering::SeqCst) <= budget.total);
    assert_eq!(budget.reserved(), 0);
    let calls = budget.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), PEERS, "one reservation per fetch: {calls:?}");
    assert!(
        calls
            .iter()
            .all(|&(now, total)| now == total && now >= size),
        "{calls:?}"
    );
}

/// A peer that announces a size, then never sends a piece: its fetch ends
/// at the deadline, and what it reserved goes with it.
#[tokio::test]
async fn a_peer_that_announces_then_stalls_ends_at_the_deadline() {
    let seed = Seed::v1(info_dict(20_000));
    let peer = FakePeer::spawn(vec![seed.clone()], Accept::Both, Behavior::Stall, vec![]).await;
    let budget = TestBudget::new(1 << 20);
    let held = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut o = budget.opts(held.clone());
    // Long enough for the handshakes on a slow runner; a stalled peer
    // ends only at the deadline, so the test takes exactly this long.
    o.timeout = Duration::from_secs(3);
    let r = fetch(peer.addr(), InfoHash(seed.info_hash), &o).await;
    assert!(matches!(r, Err(Error::Timeout)), "{r:?}");
    assert!(
        budget.reserved() >= seed.info.len(),
        "reserved before it stalled"
    );
    drop((o, held));
    assert_eq!(budget.reserved(), 0, "freed when the fetch ended");
}

/// A peer that sends more than it announced is refused before its pieces
/// can pass the size the fetch reserved for.
#[tokio::test]
async fn a_peer_that_sends_more_than_it_announced_is_refused() {
    let seed = Seed::v1(info_dict(20_000));
    let peer = FakePeer::spawn(
        vec![seed.clone()],
        Accept::Both,
        Behavior::LieAboutSize(1000),
        vec![],
    )
    .await;
    let budget = TestBudget::new(1 << 20);
    let held = Arc::new(std::sync::Mutex::new(Vec::new()));
    let r = fetch(
        peer.addr(),
        InfoHash(seed.info_hash),
        &budget.opts(held.clone()),
    )
    .await;
    assert!(matches!(r, Err(Error::Protocol(_))), "{r:?}");
    let calls = budget.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].1 < 20_000,
        "reserved for the announced 1000: {calls:?}"
    );
    drop(held);
    assert_eq!(budget.reserved(), 0);
}

/// A minimal plaintext peer: it answers the handshakes, announcing
/// `size`, then hangs up.
async fn announcing_peer(
    info_hash: [u8; 20],
    size: usize,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut theirs = [0u8; message::HANDSHAKE_LEN];
        s.read_exact(&mut theirs).await.unwrap();
        s.write_all(&message::handshake(
            &info_hash,
            b"-RW0001-rawrawrawraw",
            true,
        ))
        .await
        .unwrap();
        s.write_all(&extended_frame(
            0,
            message::our_extended_handshake(Some(size)),
        ))
        .await
        .unwrap();
    });
    (addr, task)
}

/// One BEP 10 message on the wire.
fn extended_frame(ext_id: u8, body: Vec<u8>) -> Vec<u8> {
    let mut frame = ((body.len() + 2) as u32).to_be_bytes().to_vec();
    frame.extend([20, ext_id]);
    frame.extend(body);
    frame
}

/// Reads one message's body (length prefix stripped).
async fn read_frame<S: tokio::io::AsyncRead + Unpin>(s: &mut S) -> Vec<u8> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len).await.unwrap();
    let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
    s.read_exact(&mut body).await.unwrap();
    body
}

/// No piece is requested until the reservation is granted. Over an
/// in-memory stream, with no timing: the hook holds the grant on a
/// `Notify`. Once it has been called, anything the fetcher sent before it
/// is already in the stream's buffer, so a read that doesn't wait finds
/// nothing; after the grant, the first metadata request arrives.
#[tokio::test]
async fn no_piece_is_requested_before_the_reservation() {
    let hash = [7u8; 20];
    let (ours, mut peer) = tokio::io::duplex(1 << 20);
    let (called, grant) = (
        Arc::new(tokio::sync::Notify::new()),
        Arc::new(tokio::sync::Notify::new()),
    );
    let mut o = opts(Encryption::Disable);
    let (c, g) = (called.clone(), grant.clone());
    o.reserve = Some(Reserve(Arc::new(move |_, _| {
        let (c, g) = (c.clone(), g.clone());
        Box::pin(async move {
            c.notify_one();
            g.notified().await;
        })
    })));
    let fetching = tokio::spawn(async move {
        lodestar_peer_wire::fetch_on_stream(ours, &InfoHash(hash), &o, false).await
    });
    // The handshakes, announcing 50 KB.
    let mut theirs = [0u8; message::HANDSHAKE_LEN];
    peer.read_exact(&mut theirs).await.unwrap();
    peer.write_all(&message::handshake(&hash, b"-RW0001-rawrawrawraw", true))
        .await
        .unwrap();
    peer.write_all(&extended_frame(
        0,
        message::our_extended_handshake(Some(50_000)),
    ))
    .await
    .unwrap();
    // The fetcher's own extended handshake comes first.
    let hello = read_frame(&mut peer).await;
    assert_eq!(&hello[..2], &[20, 0]);
    tokio::time::timeout(Duration::from_secs(10), called.notified())
        .await
        .expect("the hook is called");
    // Held: nothing buffered. `timeout` polls the read once before its
    // zero deadline, so buffered bytes would be returned.
    let mut byte = [0u8; 1];
    let early = tokio::time::timeout(Duration::ZERO, peer.read(&mut byte)).await;
    assert!(early.is_err(), "a message before the grant: {early:?}");
    grant.notify_one();
    let request = tokio::time::timeout(Duration::from_secs(10), read_frame(&mut peer))
        .await
        .expect("a request after the grant");
    assert_eq!(&request[..2], &[20, message::OUR_UT_METADATA]);
    assert!(matches!(
        message::parse_metadata_message(&request[2..]),
        Ok(message::MetadataMessage::Request { piece: 0 })
    ));
    fetching.abort();
}

/// A peer that announces a size and hangs up: the fetch fails at once,
/// and its reservation is freed with it.
#[tokio::test]
async fn a_peer_that_announces_then_closes_frees_its_reservation() {
    let hash = [8u8; 20];
    let (addr, peer) = announcing_peer(hash, 300_000).await;
    let budget = TestBudget::new(1 << 20);
    let held = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut o = budget.opts(held.clone());
    o.timeout = Duration::from_secs(10);
    let r = fetch(addr, InfoHash(hash), &o).await;
    assert!(matches!(r, Err(Error::Io(_))), "{r:?}");
    peer.await.unwrap();
    assert_eq!(budget.calls.lock().unwrap().len(), 1, "it had reserved");
    // Its first window, not its 300 KB claim: it never sent a byte.
    assert!(budget.reserved() >= 64 << 10);
    assert!(budget.reserved() < 100_000);
    drop((o, held));
    assert_eq!(budget.reserved(), 0);
}

/// The reservation is the result's heap bound, not the raw announced
/// size: for a peer announcing exactly `max_metadata`, the size plus its
/// client name plus a full PEX list (200 addresses), and nothing more.
/// One byte over the cap is refused before anything is reserved.
#[tokio::test]
async fn a_peer_announcing_max_metadata_reserves_exactly_its_bound() {
    // Under the first window, so the claim is whole at the handshake.
    const MAX: usize = 48 << 10;
    let seed = Seed::v1(info_dict(20_000));
    let hash = InfoHash(seed.info_hash);
    let pex_bound = 200 * std::mem::size_of::<SocketAddr>();
    let client = "FakePeer 1.0".len();
    for (announced, reserves) in [(MAX, true), (MAX + 1, false)] {
        let peer = FakePeer::spawn(
            vec![seed.clone()],
            Accept::Both,
            Behavior::LieAboutSize(announced as i64),
            vec![],
        )
        .await;
        let budget = TestBudget::new(1 << 20);
        let held = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut o = budget.opts(held.clone());
        o.max_metadata = MAX;
        // The real pieces don't match the claimed size, so it fails after
        // the reservation; only the reservation matters here.
        let r = fetch(peer.addr(), hash, &o).await;
        assert!(r.is_err());
        let calls = budget.calls.lock().unwrap().clone();
        if reserves {
            let bound = MAX + client + pex_bound;
            assert_eq!(calls, vec![(bound, bound)]);
        } else {
            assert!(matches!(r, Err(Error::MetadataSize(_))), "{r:?}");
            assert!(calls.is_empty(), "nothing reserved over the cap");
        }
        drop((o, held));
        assert_eq!(budget.reserved(), 0);
    }
}

/// A peer that announces a large dict reserves only its first request
/// window (64 KiB) until it has sent that window: a peer that sends nothing
/// holds at most that. An honest peer's fetch grows to the whole bound
/// before it requests past the window.
#[tokio::test]
async fn a_large_claim_reserves_its_first_window_until_the_window_arrives() {
    const WINDOW: usize = 4 * 16 * 1024;
    let pex_bound = 200 * std::mem::size_of::<SocketAddr>();
    let client = "FakePeer 1.0".len();
    let first = WINDOW + client + pex_bound;
    let budget = TestBudget::new(16 << 20);
    // A peer that claims 8 MiB, then sends nothing.
    let seed = Seed::v1(info_dict(20_000));
    let liar = FakePeer::spawn(
        vec![seed.clone()],
        Accept::Both,
        Behavior::LieAboutSize(8 << 20),
        vec![],
    )
    .await;
    let held = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut o = budget.opts(held.clone());
    o.timeout = Duration::from_secs(3);
    // Its real pieces are the wrong length for an 8 MiB dict: refused.
    let r = fetch(liar.addr(), InfoHash(seed.info_hash), &o).await;
    assert!(r.is_err());
    assert_eq!(
        *budget.calls.lock().unwrap(),
        vec![(first, (8 << 20) + client + pex_bound)],
        "only the first window, never the claim"
    );
    assert!(budget.reserved() <= first);
    drop((o, held));
    budget.calls.lock().unwrap().clear();

    // An honest peer with a 3 MiB dict: the window, then the whole bound.
    let seed = Seed::v1(sized_dict(3 << 20));
    let honest = FakePeer::spawn(vec![seed.clone()], Accept::Both, Behavior::Honest, vec![]).await;
    let held = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut o = budget.opts(held.clone());
    o.timeout = Duration::from_secs(30);
    let got = fetch(honest.addr(), InfoHash(seed.info_hash), &o)
        .await
        .expect("fetched");
    let bound = seed.info.len() + client + pex_bound;
    assert_eq!(
        *budget.calls.lock().unwrap(),
        vec![(first, bound), (bound, bound)]
    );
    assert!(heap(&got) <= bound);
    drop((o, held));
    assert_eq!(budget.reserved(), 0);
}

/// A raw plaintext peer that announces `size` and answers the first
/// `serve` metadata requests with pieces of the right length, then goes
/// silent (but keeps the connection open) until the test ends.
async fn serving_peer(info_hash: [u8; 20], size: usize, serve: usize) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut theirs = [0u8; message::HANDSHAKE_LEN];
        s.read_exact(&mut theirs).await.unwrap();
        s.write_all(&message::handshake(
            &info_hash,
            b"-RW0001-rawrawrawraw",
            true,
        ))
        .await
        .unwrap();
        let send = |ext_id: u8, body: Vec<u8>| {
            let mut frame = ((body.len() + 2) as u32).to_be_bytes().to_vec();
            frame.extend([20, ext_id]);
            frame.extend(body);
            frame
        };
        s.write_all(&send(0, message::our_extended_handshake(Some(size))))
            .await
            .unwrap();
        let mut served = 0;
        loop {
            let mut len = [0u8; 4];
            if s.read_exact(&mut len).await.is_err() {
                return;
            }
            let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
            if s.read_exact(&mut body).await.is_err() {
                return;
            }
            if body.len() < 2 || body[0] != 20 || body[1] != message::OUR_UT_METADATA {
                continue;
            }
            let Ok(message::MetadataMessage::Request { piece }) =
                message::parse_metadata_message(&body[2..])
            else {
                continue;
            };
            if served == serve {
                continue;
            }
            served += 1;
            let piece = piece as usize;
            let start = piece * message::METADATA_PIECE;
            let len = (size - start).min(message::METADATA_PIECE);
            let reply = message::metadata_data(piece, size, &vec![0x55; len]);
            // The fetch reads it as `ut_metadata` with its own id.
            s.write_all(&send(1, reply)).await.unwrap();
        }
    });
    addr
}

/// A peer that sends part of the first window of a large claim never gets
/// the claim reserved: growth waits for the whole window (64 KiB). Once it
/// sends the whole window, the fetch reserves the rest.
#[tokio::test]
async fn growth_waits_for_the_whole_first_window() {
    const WINDOW_PIECES: usize = 4;
    let size = 4 << 20;
    for (serve, grows) in [
        (1, false),
        (WINDOW_PIECES - 1, false),
        (WINDOW_PIECES, true),
    ] {
        let hash = [9u8; 20];
        let addr = serving_peer(hash, size, serve).await;
        let budget = TestBudget::new(16 << 20);
        let held = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut o = budget.opts(held.clone());
        o.timeout = Duration::from_secs(3);
        let r = fetch(addr, InfoHash(hash), &o).await;
        assert!(matches!(r, Err(Error::Timeout)), "{serve}: {r:?}");
        let calls = budget.calls.lock().unwrap().clone();
        let total = calls[0].1;
        let grew = calls.iter().any(|&(now, _)| now == total);
        assert_eq!(grew, grows, "{serve} pieces served: {calls:?}");
        drop((o, held));
        assert_eq!(budget.reserved(), 0);
    }
}
