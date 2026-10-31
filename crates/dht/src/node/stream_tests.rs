//! CR-1a-S's actor side (docs/plan/2026-10-27-streaming-and-hints.md,
//! "Bounded peer delivery", "Independent lifetimes"): streaming sessions
//! against the real actor, on tokio's paused clock where time matters.

use super::*;
use crate::node::stream::{LookupSession, STREAM_BATCH};
use rand::SeedableRng;
use tokio::sync::Semaphore;

fn actor() -> (Actor, Vec<SendSocket>) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let addr = socket.local_addr().unwrap();
    let cfg = Config {
        bind: vec![addr],
        bootstrap: vec![],
        allow_private: true,
        sample_start_delay: Some(Duration::ZERO),
        ..Default::default()
    };
    let a = Actor::new(
        cfg,
        Arc::new(Stats::default()),
        vec![addr],
        None,
        vec![],
        None,
        Instant::now(),
        StdRng::seed_from_u64(3),
    );
    (a, vec![SendSocket::Udp(socket)])
}

/// A routing candidate per port, each with its own id.
fn nodes(a: &mut Actor, family: usize, ip: &str, ports: &[u16]) {
    for &port in ports {
        let mut id = [42; 20];
        id[19] = port as u8;
        let addr = SocketAddr::new(ip.parse().unwrap(), port);
        a.families[family].table.insert(
            NodeInfo {
                id: NodeId(id),
                addr,
            },
            Contact::Response,
            0,
        );
    }
}

fn peer(n: u16) -> SocketAddr {
    SocketAddr::from(([10, 0, 0, 1], 2000 + n))
}

/// Admits a streaming request through the actor's own admission and
/// starts it at once.
fn stream(
    a: &mut Actor,
    sockets: &[SendSocket],
    hash: InfoHash,
    permit: Option<PeerPermit>,
) -> LookupSession {
    let (tx, mut rx) = oneshot::channel::<Option<LookupSession>>();
    a.on_command(Command::RequestSession(hash, Box::new(tx), permit));
    let (hash, reply, permit) = a.get_peers_queue.pop_front().expect("admitted");
    a.start_peer_request(sockets, hash, reply, permit);
    rx.try_recv().unwrap().expect("a session")
}

/// Answers query `tid` with `values` and no nodes.
fn answer(a: &mut Actor, sockets: &[SendSocket], tid: u64, values: Vec<SocketAddr>) {
    let p = &a.pending[&tid];
    let (family, addr, id) = (p.family, p.to, p.id.unwrap());
    let bytes = tid.to_be_bytes();
    let response = Response {
        id,
        values,
        ..Default::default()
    };
    let msg = Message {
        transaction_id: &bytes,
        version: None,
        ip: None,
        read_only: false,
        body: Body::Response(response.clone()),
    };
    a.on_response(sockets, family, &msg, Some(&response), addr);
}

fn tids(a: &Actor) -> Vec<u64> {
    let mut t: Vec<_> = a.pending.keys().copied().collect();
    t.sort_unstable();
    t
}

fn terminals(a: &Actor, end: LookupEnd) -> u64 {
    a.stats.snapshot().peer_stream_terminals[end as usize]
}

fn drained(a: &Actor) {
    assert!(a.pending.is_empty());
    assert!(a.lookups.is_empty());
    assert!(a.stalled.is_empty());
    assert!(a.peer_requests.is_empty());
    assert!(a.get_peers_queue.is_empty());
    assert_eq!(a.active_get_peers, 0);
}

#[tokio::test(start_paused = true)]
async fn early_batch_arrives_while_the_lookup_still_owns_its_slot() {
    let (mut a, sockets) = actor();
    nodes(&mut a, 0, "127.0.0.1", &[65001, 65002]);
    let mut s = stream(&mut a, &sockets, InfoHash([1; 20]), None);
    let t = tids(&a);
    assert_eq!(t.len(), 2);
    answer(&mut a, &sockets, t[0], vec![peer(1), peer(2)]);
    // The batch is out before the lookup ends; the slot is still held.
    assert_eq!(s.batches.try_recv().unwrap(), vec![peer(1), peer(2)]);
    assert_eq!(
        s.terminal.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    );
    assert_eq!(a.active_get_peers, 1);
    // A repeat isn't sent twice; the last reply ends it.
    answer(&mut a, &sockets, t[1], vec![peer(2), peer(3)]);
    assert_eq!(s.terminal.try_recv(), Ok(LookupEnd::Complete));
    assert_eq!(s.batches.try_recv().unwrap(), vec![peer(3)]);
    assert!(s.batches.try_recv().is_err());
    drained(&a);
    let snap = a.stats.snapshot();
    assert_eq!((snap.peer_stream_batches, snap.lookups_completed), (2, 1));
    assert_eq!(terminals(&a, LookupEnd::Complete), 1);
}

#[tokio::test(start_paused = true)]
async fn a_full_channel_holds_one_batch_drops_the_rest_and_still_terminates() {
    let (mut a, sockets) = actor();
    nodes(&mut a, 0, "127.0.0.1", &[65001, 65002]);
    let mut s = stream(&mut a, &sockets, InfoHash([2; 20]), None);
    let t = tids(&a);
    // Nobody reads: two batches queue, one waits, the rest is dropped.
    let many: Vec<_> = (0..30).map(peer).collect();
    answer(&mut a, &sockets, t[0], many);
    let snap = a.stats.snapshot();
    assert_eq!(snap.peer_stream_batches, 2);
    assert_eq!(snap.peer_stream_backpressure, 30 - 3 * STREAM_BATCH as u64);
    // The terminal doesn't wait for room; the held batch is counted lost.
    answer(&mut a, &sockets, t[1], vec![]);
    assert_eq!(s.terminal.try_recv(), Ok(LookupEnd::Complete));
    assert_eq!(
        a.stats.snapshot().peer_stream_backpressure,
        30 - 2 * STREAM_BATCH as u64
    );
    // The two queued batches are still there to drain.
    assert_eq!(s.batches.try_recv().unwrap().len(), STREAM_BATCH);
    assert_eq!(s.batches.try_recv().unwrap().len(), STREAM_BATCH);
    assert!(s.batches.try_recv().is_err());
    drained(&a);
}

#[tokio::test(start_paused = true)]
async fn the_tick_sends_a_held_batch_once_there_is_room() {
    let (mut a, sockets) = actor();
    nodes(&mut a, 0, "127.0.0.1", &[65001, 65002]);
    let mut s = stream(&mut a, &sockets, InfoHash([3; 20]), None);
    let t = tids(&a);
    answer(&mut a, &sockets, t[0], (0..24).map(peer).collect());
    assert_eq!(a.stats.snapshot().peer_stream_batches, 2);
    s.batches.try_recv().unwrap();
    a.maintain(&sockets);
    assert_eq!(a.stats.snapshot().peer_stream_batches, 3);
    assert_eq!(a.stats.snapshot().peer_stream_backpressure, 0);
}

#[tokio::test(start_paused = true)]
async fn the_deadline_ends_a_stream_as_deadline_not_complete() {
    let (mut a, sockets) = actor();
    nodes(&mut a, 0, "127.0.0.1", &[65001]);
    let mut s = stream(&mut a, &sockets, InfoHash([4; 20]), None);
    tokio::time::advance(LOOKUP_DEADLINE).await;
    a.scrape_tick(&sockets, Instant::now());
    assert_eq!(s.terminal.try_recv(), Ok(LookupEnd::Deadline));
    drained(&a);
}

#[tokio::test(start_paused = true)]
async fn a_dual_stack_session_is_one_request_ended_by_its_last_leg() {
    use std::io::Write;
    let (mut a, mut sockets) = actor();
    let Ok(v6) = std::net::UdpSocket::bind("[::1]:0") else {
        let _ = writeln!(
            std::io::stderr(),
            "a_dual_stack_session_is_one_request_ended_by_its_last_leg: skipped, no IPv6 loopback"
        );
        return;
    };
    v6.set_nonblocking(true).unwrap();
    sockets.push(SendSocket::Udp(v6));
    a.families.push(Family {
        table: RoutingTable::new(a.ids[0], true),
        ipv6: true,
    });
    nodes(&mut a, 0, "127.0.0.1", &[65001]);
    nodes(&mut a, 1, "::1", &[65001]);
    let sem = Arc::new(Semaphore::new(1));
    let permit = sem.clone().try_acquire_owned().unwrap();
    let mut s = stream(&mut a, &sockets, InfoHash([5; 20]), Some(permit));
    let t = tids(&a);
    assert_eq!(t.len(), 2, "one leg per family");
    // One leg ends: the request, its slot and its permit stay.
    answer(&mut a, &sockets, t[0], vec![peer(1)]);
    assert_eq!(s.batches.try_recv().unwrap(), vec![peer(1)]);
    assert_eq!(
        s.terminal.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    );
    assert_eq!((a.active_get_peers, sem.available_permits()), (1, 0));
    answer(&mut a, &sockets, t[1], vec![]);
    assert_eq!(s.terminal.try_recv(), Ok(LookupEnd::Complete));
    assert_eq!(sem.available_permits(), 1);
    drained(&a);

    // Cancelled with both legs out: both close, the permit returns once.
    let permit = sem.clone().try_acquire_owned().unwrap();
    let s = stream(&mut a, &sockets, InfoHash([6; 20]), Some(permit));
    assert_eq!(tids(&a).len(), 2);
    drop(s);
    a.scrape_tick(&sockets, Instant::now());
    drained(&a);
    assert_eq!(sem.available_permits(), 1);
    let snap = a.stats.snapshot();
    assert_eq!(
        snap.peer_permit_returns[PermitReturn::CancelledActive as usize],
        1
    );
    assert_eq!(snap.peer_permit_returns.iter().sum::<u64>(), 2);
    assert_eq!(terminals(&a, LookupEnd::Cancelled), 1);
}

#[tokio::test(start_paused = true)]
async fn streaming_admission_returns_each_permit_once_at_every_boundary() {
    let (mut a, sockets) = actor();
    let sem = Arc::new(Semaphore::new(64));
    let permit = || Some(sem.clone().try_acquire_owned().unwrap());
    let ask = |a: &mut Actor, p| {
        let (tx, mut rx) = oneshot::channel::<Option<LookupSession>>();
        a.on_command(Command::RequestSession(InfoHash([7; 20]), Box::new(tx), p));
        rx.try_recv()
    };
    // Caller closed: refused, permit back.
    let (tx, rx) = oneshot::channel::<Option<LookupSession>>();
    drop(rx);
    a.on_command(Command::RequestSession(
        InfoHash([7; 20]),
        Box::new(tx),
        permit(),
    ));
    // Paused: refused with an answer, then the permit.
    a.on_command(Command::SetPaused(true));
    assert!(matches!(ask(&mut a, permit()), Ok(None)));
    a.on_command(Command::SetPaused(false));
    // Admitted then dropped by a pause: the caller reads a source failure.
    let mut queued = ask(&mut a, permit()).unwrap().unwrap();
    a.on_command(Command::SetPaused(true));
    assert_eq!(
        queued.terminal.try_recv(),
        Err(oneshot::error::TryRecvError::Closed)
    );
    a.on_command(Command::SetPaused(false));
    // Capacity.
    let held: Vec<_> = (0..MAX_GET_PEERS)
        .map(|_| ask(&mut a, permit()).unwrap().unwrap())
        .collect();
    assert!(matches!(ask(&mut a, permit()), Ok(None)));
    // Cancelled while queued.
    drop(held);
    a.maintain(&sockets);
    drained(&a);
    let snap = a.stats.snapshot();
    let r = |why: PermitReturn| snap.peer_permit_returns[why as usize];
    assert_eq!(r(PermitReturn::Refused), 3);
    assert_eq!(r(PermitReturn::Paused), 1);
    assert_eq!(r(PermitReturn::CancelledQueued), MAX_GET_PEERS as u64);
    assert_eq!(sem.available_permits(), 64);
}

#[tokio::test(start_paused = true)]
async fn a_handoff_failure_closes_the_session_admission_before_the_permit() {
    let (tx, cmds) = mpsc::channel(1);
    let stats = Arc::new(Stats::default());
    let handle = crate::node::NodeHandle {
        stats: stats.clone(),
        commands: tx,
    };
    drop(cmds);
    let sem = Arc::new(Semaphore::new(1));
    let (admit, mut rx) = oneshot::channel::<Option<LookupSession>>();
    let permit = sem.clone().try_acquire_owned().unwrap();
    assert!(!handle.request_session(InfoHash([8; 20]), Box::new(admit), Some(permit)));
    assert!(matches!(
        rx.try_recv(),
        Err(oneshot::error::TryRecvError::Closed)
    ));
    assert_eq!(sem.available_permits(), 1);
    assert_eq!(
        stats.snapshot().peer_permit_returns[PermitReturn::HandoffFailed as usize],
        1
    );
}
