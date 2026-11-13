//! The state machine over a simulated network with a virtual clock: loss,
//! duplication, reordering and sequence-number wraparound.

use std::time::{Duration, Instant};

use proptest::prelude::*;
use proptest::test_runner::RngSeed;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

use super::*;
use crate::packet::parse;

const LIMITS: Limits = Limits {
    recv_window: 16 << 10,
    send_buffer: 16 << 10,
    idle_timeout: std::time::Duration::from_secs(60),
};

#[derive(Debug, Clone, Copy)]
struct Link {
    loss: f64,
    dup: f64,
    /// Extra random delay per packet, which reorders them.
    jitter_ms: u64,
    delay_ms: u64,
}

const CLEAN: Link = Link {
    loss: 0.0,
    dup: 0.0,
    jitter_ms: 0,
    delay_ms: 10,
};

struct Sim {
    now: Instant,
    t0: Instant,
    rng: StdRng,
    link: Link,
    /// (deliver at, to B?, datagram)
    wire: Vec<(Instant, bool, Vec<u8>)>,
    /// Drops in a row per direction; capped at one so the handshake and
    /// retransmission limits can't be exhausted by bad luck alone.
    dropped_in_row: [u32; 2],
    a: Connection,
    b: Option<Connection>,
    b_seq: u16,
    sent: [u64; 2],
}

impl Sim {
    fn new(seed: u64, link: Link, a_seq: u16, b_seq: u16) -> Sim {
        let t0 = Instant::now();
        Sim {
            now: t0,
            t0,
            rng: StdRng::seed_from_u64(seed),
            link,
            wire: Vec::new(),
            dropped_in_row: [0; 2],
            a: Connection::connect(100, a_seq, LIMITS, t0, t0),
            b: None,
            b_seq,
            sent: [0; 2],
        }
    }

    fn transmit(&mut self) {
        let mut out = Vec::new();
        for to_b in [true, false] {
            loop {
                let conn = if to_b {
                    Some(&mut self.a)
                } else {
                    self.b.as_mut()
                };
                let Some(conn) = conn else { break };
                if !conn.poll_transmit(self.now, &mut out) {
                    break;
                }
                self.sent[usize::from(to_b)] += 1;
                let dir = usize::from(to_b);
                if self.rng.random_bool(self.link.loss) && self.dropped_in_row[dir] == 0 {
                    self.dropped_in_row[dir] += 1;
                    continue;
                }
                self.dropped_in_row[dir] = 0;
                let copies = if self.rng.random_bool(self.link.dup) {
                    2
                } else {
                    1
                };
                for _ in 0..copies {
                    let jitter = self.rng.random_range(0..=self.link.jitter_ms);
                    let at = self.now + Duration::from_millis(self.link.delay_ms + jitter);
                    self.wire.push((at, to_b, out.clone()));
                }
            }
        }
    }

    /// Advances to the next event and handles it. False when nothing is
    /// left to happen.
    fn step(&mut self) -> bool {
        self.transmit();
        let next_packet = self.wire.iter().map(|w| w.0).min();
        let deadlines = [
            self.a.deadline(),
            self.b.as_ref().and_then(Connection::deadline),
        ];
        let Some(next) = deadlines.into_iter().flatten().chain(next_packet).min() else {
            return false;
        };
        self.now = self.now.max(next);
        let now = self.now;
        let (due, later): (Vec<_>, Vec<_>) = self.wire.drain(..).partition(|w| w.0 <= now);
        self.wire = later;
        let mut due = due;
        due.sort_by_key(|w| w.0);
        for (_, to_b, datagram) in due {
            let p = parse(&datagram).expect("we only send valid packets");
            if !to_b {
                self.a.on_packet(&p, now);
            } else if let Some(b) = self.b.as_mut().filter(|b| !b.is_closed()) {
                b.on_packet(&p, now);
            } else if p.header.ty == PacketType::Syn {
                // As the socket does: a closed connection is gone, and a
                // new SYN starts a new one.
                self.b = Some(Connection::accept(
                    &p.header, self.b_seq, LIMITS, self.t0, now,
                ));
            }
        }
        self.a.on_timeout(now);
        if let Some(b) = self.b.as_mut() {
            b.on_timeout(now);
        }
        true
    }
}

/// Sends `ab` from A to B and `ba` back, both closing when done; returns
/// what each side received.
fn exchange(sim: &mut Sim, ab: &[u8], ba: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let (mut a_off, mut b_off) = (0, 0);
    let (mut at_a, mut at_b) = (Vec::new(), Vec::new());
    let mut buf = [0u8; 4096];
    for _ in 0..2_000_000 {
        if a_off < ab.len() {
            a_off += sim.a.write(&ab[a_off..]).unwrap();
            if a_off == ab.len() {
                sim.a.close(sim.now);
            }
        }
        while let Ok(n @ 1..) = sim.a.read(&mut buf) {
            at_a.extend_from_slice(&buf[..n]);
        }
        let now = sim.now;
        if let Some(b) = sim.b.as_mut() {
            if b_off < ba.len() {
                b_off += b.write(&ba[b_off..]).unwrap();
                if b_off == ba.len() {
                    b.close(now);
                }
            } else if ba.is_empty() {
                b.close(now);
            }
            while let Ok(n @ 1..) = b.read(&mut buf) {
                at_b.extend_from_slice(&buf[..n]);
            }
        }
        if ab.is_empty() && sim.a.state() == State::Connected {
            sim.a.close(sim.now);
        }
        let done = sim.a.is_closed() && sim.b.as_ref().is_some_and(Connection::is_closed);
        if done || !sim.step() {
            break;
        }
    }
    let b = sim.b.as_ref().expect("B was created");
    // A side may time out waiting for the ACK of its FIN when every ACK
    // that would tell it was lost and the peer's TIME-WAIT ran out (the
    // two-generals limit; TCP has it too). That's acceptable only if the
    // peer really got the whole stream and the FIN.
    let finished = |c: &Connection, peer_got_all: bool| match c.state() {
        State::Closed(Closed::Done) => true,
        State::Closed(Closed::TimedOut) => peer_got_all,
        _ => false,
    };
    assert!(
        finished(&sim.a, at_b == ab && b.eof),
        "A ended {:?}",
        sim.a.state()
    );
    assert!(
        finished(b, at_a == ba && sim.a.eof),
        "B ended {:?}",
        b.state()
    );
    (at_a, at_b)
}

fn payload(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

#[test]
fn clean_link_both_directions() {
    let mut sim = Sim::new(1, CLEAN, 1, 5000);
    let ab = payload(100_000, 1);
    let ba = payload(30_000, 2);
    let (at_a, at_b) = exchange(&mut sim, &ab, &ba);
    assert_eq!(at_b, ab);
    assert_eq!(at_a, ba);
    assert_eq!(sim.a.retransmits, 0, "nothing lost, nothing resent");
}

#[test]
fn handshake_follows_bep29() {
    let t0 = Instant::now();
    let mut a = Connection::connect(7000, 1, LIMITS, t0, t0);
    let mut out = Vec::new();
    assert!(a.poll_transmit(t0, &mut out));
    let syn = parse(&out).unwrap();
    assert_eq!(syn.header.ty, PacketType::Syn);
    assert_eq!(syn.header.conn_id, 7000, "the SYN carries our receive id");
    assert_eq!(syn.header.seq_nr, 1);
    let syn_h = syn.header;
    let mut b = Connection::accept(&syn_h, 300, LIMITS, t0, t0);
    assert_eq!((b.recv_id, b.send_id), (7001, 7000));
    assert!(b.poll_transmit(t0, &mut out));
    let ack = parse(&out).unwrap();
    assert_eq!(ack.header.ty, PacketType::State);
    assert_eq!(ack.header.conn_id, 7000);
    assert_eq!(ack.header.ack_nr, 1);
    assert_eq!(ack.header.seq_nr, 300, "a STATE doesn't consume a number");
    a.on_packet(&ack, t0);
    assert_eq!(a.state(), State::Connected);
    a.write(b"hi").unwrap();
    assert!(a.poll_transmit(t0, &mut out));
    let data = parse(&out).unwrap();
    assert_eq!(data.header.conn_id, 7001, "sends with receive id + 1");
    assert_eq!(data.header.seq_nr, 2);
    assert_eq!(data.header.ack_nr, 299);
    b.on_packet(&data, t0);
    let mut buf = [0u8; 8];
    assert_eq!(b.read(&mut buf).unwrap(), 2);
    // B's first data packet is the one A expects next.
    b.write(b"yo").unwrap();
    assert!(b.poll_transmit(t0, &mut out));
    let reply = parse(&out).unwrap();
    assert_eq!(reply.header.seq_nr, 300);
    a.on_packet(&reply, t0);
    assert_eq!(a.read(&mut buf).unwrap(), 2);
}

#[test]
fn reset_closes_and_reports() {
    let mut sim = Sim::new(2, CLEAN, 1, 1);
    while sim.b.is_none() || sim.a.state() == State::SynSent {
        assert!(sim.step());
    }
    sim.b.as_mut().unwrap().reset();
    while sim.step() {}
    assert_eq!(sim.a.state(), State::Closed(Closed::Reset));
    let mut buf = [0u8; 4];
    assert_eq!(
        sim.a.read(&mut buf).unwrap_err().kind(),
        io::ErrorKind::ConnectionReset
    );
    assert_eq!(
        sim.a.write(b"x").unwrap_err().kind(),
        io::ErrorKind::ConnectionReset
    );
}

#[test]
fn syn_retries_with_backoff_then_gives_up() {
    let t0 = Instant::now();
    let mut a = Connection::connect(1, 1, LIMITS, t0, t0);
    let mut out = Vec::new();
    let mut now = t0;
    let mut sends = Vec::new();
    while !a.is_closed() {
        while a.poll_transmit(now, &mut out) {
            sends.push(now - t0);
        }
        now = a.deadline().expect("armed while waiting");
        a.on_timeout(now);
    }
    assert_eq!(a.state(), State::Closed(Closed::TimedOut));
    let secs: Vec<u64> = sends.iter().map(Duration::as_secs).collect();
    assert_eq!(secs, [0, 1, 3, 7], "1 s, then doubling");
}

#[test]
fn dead_peer_times_out_with_exponential_backoff() {
    let mut sim = Sim::new(3, CLEAN, 1, 1);
    while sim.a.state() != State::Connected {
        assert!(sim.step());
    }
    sim.a.write(&[1; 5000]).unwrap();
    sim.link.loss = 1.0;
    // Lose everything from here: dropped_in_row stops capping.
    sim.dropped_in_row = [0; 2];
    let mut fired = Vec::new();
    let mut out = Vec::new();
    while !sim.a.is_closed() {
        while sim.a.poll_transmit(sim.now, &mut out) {}
        let d = sim
            .a
            .deadline()
            .expect("data in flight keeps the timer armed");
        fired.push(d - sim.now);
        sim.now = d;
        sim.a.on_timeout(d);
    }
    assert_eq!(sim.a.state(), State::Closed(Closed::TimedOut));
    assert_eq!(fired.len() as u32, MAX_TIMEOUTS + 1);
    for w in fired.windows(2).skip(1) {
        assert!(
            w[1] >= w[0] * 2 || w[1] == crate::ledbat::MAX_RTO,
            "{fired:?}"
        );
    }
    assert!(sim.a.retransmits >= u64::from(MAX_TIMEOUTS));
}

#[test]
fn receive_window_holds_the_sender_back() {
    let mut sim = Sim::new(4, CLEAN, 1, 1);
    while sim.b.is_none() || sim.a.state() == State::SynSent {
        assert!(sim.step());
    }
    let data = payload(200_000, 3);
    let mut off = 0;
    // B never reads: A may fill B's window and no more.
    for _ in 0..10_000 {
        if off < data.len() {
            off += sim.a.write(&data[off..]).unwrap();
        }
        if !sim.step() || sim.now - sim.t0 > Duration::from_secs(30) {
            break;
        }
    }
    let b = sim.b.as_mut().unwrap();
    assert!(
        b.buffered() <= LIMITS.recv_window as usize,
        "{}",
        b.buffered()
    );
    assert!(sim.a.buffered() <= LIMITS.send_buffer);
    assert!(off < data.len(), "the sender was held back");
    // Reading reopens the window and the transfer finishes.
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    for _ in 0..100_000 {
        if off < data.len() {
            off += sim.a.write(&data[off..]).unwrap();
        }
        while let Ok(n @ 1..) = sim.b.as_mut().unwrap().read(&mut buf) {
            got.extend_from_slice(&buf[..n]);
        }
        if got.len() == data.len() || !sim.step() {
            break;
        }
    }
    assert_eq!(got, data);
}

#[test]
fn acks_for_unsent_packets_are_ignored() {
    let mut sim = Sim::new(5, CLEAN, 1, 1);
    while sim.b.is_none() || sim.a.state() == State::SynSent {
        assert!(sim.step());
    }
    sim.a.write(&[9; 3000]).unwrap();
    let mut out = Vec::new();
    while sim.a.poll_transmit(sim.now, &mut out) {}
    let before = sim.a.buffered();
    let forged = Header {
        ty: PacketType::State,
        conn_id: sim.a.recv_id,
        timestamp_us: 0,
        timestamp_diff_us: 0,
        wnd_size: 1 << 20,
        seq_nr: 0,
        ack_nr: sim.a.next_seq().wrapping_add(1000),
    };
    let mut bytes = Vec::new();
    packet::encode(&forged, Some(&[0xff; 32]), &[], &mut bytes);
    sim.a.on_packet(&parse(&bytes).unwrap(), sim.now);
    assert_eq!(sim.a.buffered(), before, "nothing acknowledged");
}

#[test]
fn selective_acks_trigger_fast_retransmit() {
    let t0 = Instant::now();
    let mut sim = Sim::new(6, CLEAN, 1, 1);
    while sim.b.is_none() || sim.a.state() == State::SynSent {
        assert!(sim.step());
    }
    let mut b = sim.b.take().unwrap();
    let mut a = std::mem::replace(&mut sim.a, Connection::connect(9, 1, LIMITS, t0, t0));
    // Grow the window past three packets (one slow-start round).
    for _ in 0..3 {
        a.cc.on_ack(MSS, 1000, sim.now);
    }
    a.write(&payload(5 * MSS, 7)).unwrap();
    let mut packets = Vec::new();
    let mut out = Vec::new();
    while a.poll_transmit(sim.now, &mut out) {
        packets.push(out.clone());
    }
    assert!(packets.len() >= 4, "{} packets", packets.len());
    // The first is lost; the next three arrive and B acknowledges them
    // selectively.
    for p in &packets[1..4] {
        b.on_packet(&parse(p).unwrap(), sim.now);
    }
    assert!(b.poll_transmit(sim.now, &mut out));
    let ack = parse(&out).unwrap();
    assert_eq!(ack.sack, Some(&[0b0000_0111, 0, 0, 0][..]));
    a.on_packet(&ack, sim.now);
    // The hole goes out again at once, without waiting for the timer.
    assert!(a.poll_transmit(sim.now, &mut out));
    let resent = parse(&out).unwrap();
    assert_eq!(
        resent.header.seq_nr,
        parse(&packets[0]).unwrap().header.seq_nr
    );
    assert_eq!(a.retransmits, 1);
    b.on_packet(&resent, sim.now);
    let mut buf = vec![0u8; 5 * MSS];
    let n = b.read(&mut buf).unwrap();
    assert_eq!(n, 4 * MSS, "the hole filled and released the rest");
}

#[test]
fn duplicate_syn_is_acked_again() {
    let t0 = Instant::now();
    let mut a = Connection::connect(50, 1, LIMITS, t0, t0);
    let mut out = Vec::new();
    a.poll_transmit(t0, &mut out);
    let syn = out.clone();
    let mut b = Connection::accept(&parse(&syn).unwrap().header, 9, LIMITS, t0, t0);
    assert!(b.poll_transmit(t0, &mut out));
    assert!(!b.poll_transmit(t0, &mut out));
    b.on_packet(&parse(&syn).unwrap(), t0);
    assert!(
        b.poll_transmit(t0, &mut out),
        "the lost SYN-ACK is repeated"
    );
    assert_eq!(parse(&out).unwrap().header.ack_nr, 1);
}

fn link() -> impl Strategy<Value = Link> {
    (0.0..0.25f64, 0.0..0.2f64, 0u64..80, 1u64..40).prop_map(|(loss, dup, jitter_ms, delay_ms)| {
        Link {
            loss,
            dup,
            jitter_ms,
            delay_ms,
        }
    })
}

proptest! {
    // A fixed seed keeps the simulation reproducible run to run.
    #![proptest_config(ProptestConfig {
        cases: 48,
        rng_seed: RngSeed::Fixed(0x7574_7029),
        ..ProptestConfig::default()
    })]

    /// Whatever the link does, both byte streams arrive intact and in
    /// order, and both sides close cleanly. Starting sequence numbers near
    /// 65535 cover wraparound.
    #[test]
    fn streams_survive_a_bad_link(
        seed in any::<u64>(),
        link in link(),
        a_seq in prop_oneof![Just(1u16), Just(0xfff0), any::<u16>()],
        b_seq in prop_oneof![Just(0xffffu16), Just(0xff80), any::<u16>()],
        ab_len in 0usize..60_000,
        ba_len in 0usize..20_000,
    ) {
        let mut sim = Sim::new(seed, link, a_seq, b_seq);
        let ab = payload(ab_len, 11);
        let ba = payload(ba_len, 13);
        let (at_a, at_b) = exchange(&mut sim, &ab, &ba);
        prop_assert_eq!(at_b, ab);
        prop_assert_eq!(at_a, ba);
    }
}

#[test]
fn wraparound_over_a_long_transfer() {
    // 100k packets' worth of sequence numbers wrap more than once.
    let mut sim = Sim::new(
        8,
        Link {
            loss: 0.05,
            dup: 0.05,
            jitter_ms: 20,
            delay_ms: 5,
        },
        0xff00,
        0xfffe,
    );
    let ab = payload(MSS * 70_000, 21);
    let (_, at_b) = exchange(&mut sim, &ab, b"");
    assert!(at_b == ab, "{} of {} bytes", at_b.len(), ab.len());
    assert!(sim.sent[1] > 70_000);
}

#[test]
fn accepted_side_waits_for_the_initiator() {
    // B queues data and a FIN at once, and the link reorders them ahead
    // of B's SYN-ACK. A must still read the stream from its start.
    let mut sim = Sim::new(
        6225048111680754099,
        Link {
            loss: 0.0,
            dup: 0.19,
            jitter_ms: 1,
            delay_ms: 1,
        },
        1,
        65535,
    );
    let (at_a, at_b) = exchange(&mut sim, b"", b"x");
    assert_eq!(at_b, b"");
    assert_eq!(at_a, b"x");
}

#[test]
fn half_open_accepted_connection_times_out() {
    let t0 = Instant::now();
    let mut a = Connection::connect(70, 1, LIMITS, t0, t0);
    let mut out = Vec::new();
    a.poll_transmit(t0, &mut out);
    // The initiator vanishes after its SYN: nothing ever follows our SYN-ACK.
    let mut b = Connection::accept(&parse(&out).unwrap().header, 9, LIMITS, t0, t0);
    while b.poll_transmit(t0, &mut out) {}
    let d = b.deadline().expect("a handshake deadline is set");
    assert_eq!(d - t0, HANDSHAKE_TIMEOUT);
    b.on_timeout(d - Duration::from_millis(1));
    assert_eq!(b.state(), State::SynRecv);
    b.on_timeout(d);
    assert_eq!(b.state(), State::Closed(Closed::TimedOut));
}

#[test]
fn completed_handshake_clears_its_deadline() {
    let mut sim = Sim::new(9, CLEAN, 1, 1);
    while sim.b.as_ref().is_none_or(|b| b.state() != State::Connected) {
        assert!(sim.step());
    }
    let b = sim.b.as_mut().unwrap();
    assert!(b.handshake_deadline.is_none());
    // Idle from here on: the idle timeout is what remains.
    let d = b.deadline().expect("idle deadline");
    assert_eq!(d, b.last_recv + LIMITS.idle_timeout);
}

#[test]
fn idle_connection_times_out_and_traffic_postpones_it() {
    let mut sim = Sim::new(10, CLEAN, 1, 1);
    while sim.b.is_none() || sim.a.state() != State::Connected {
        assert!(sim.step());
    }
    // A sends a little; once it's acknowledged A is idle.
    sim.a.write(b"ping").unwrap();
    while sim.a.buffered() > 0 {
        assert!(sim.step());
    }
    let first = sim
        .a
        .deadline()
        .expect("idle deadline while nothing is in flight");
    // Fresh traffic from the peer moves A's idle deadline later. (Step
    // only until it arrives: stepping an idle pair jumps to the deadline.)
    sim.now += Duration::from_secs(5);
    sim.b.as_mut().unwrap().write(b"pong").unwrap();
    let mut buf = [0u8; 8];
    while sim.a.read(&mut buf).is_err() {
        assert!(sim.step());
    }
    let later = sim.a.deadline().expect("idle deadline");
    assert!(later > first, "{later:?} vs {first:?}");
    // Then silence: both sides close as timed out, not before the timeout.
    while !sim.a.is_closed() {
        assert!(sim.step());
    }
    assert_eq!(sim.a.state(), State::Closed(Closed::TimedOut));
    assert!(sim.now - sim.t0 >= LIMITS.idle_timeout);
}

#[test]
fn data_in_flight_is_governed_by_retransmission_not_idle() {
    let t0 = Instant::now();
    let mut a = Connection::connect(80, 1, LIMITS, t0, t0);
    let mut out = Vec::new();
    a.poll_transmit(t0, &mut out);
    let mut b = Connection::accept(&parse(&out).unwrap().header, 9, LIMITS, t0, t0);
    b.poll_transmit(t0, &mut out);
    a.on_packet(&parse(&out).unwrap(), t0);
    a.write(&[1; 100]).unwrap();
    while a.poll_transmit(t0, &mut out) {}
    // The data's retransmission timer (≈1 s), not the 60 s idle deadline.
    let d = a.deadline().unwrap();
    assert!(d - t0 <= Duration::from_secs(2), "{:?}", d - t0);
}

/// Two connected endpoints taken out of a clean simulation, to be driven
/// by hand.
fn connected(seed: u64) -> (Connection, Connection, Instant) {
    let mut sim = Sim::new(seed, CLEAN, 1, 1);
    while sim.b.is_none() || sim.a.state() != State::Connected {
        assert!(sim.step());
    }
    // B learns the handshake finished from A's confirming ACK.
    while sim.b.as_ref().unwrap().state() != State::Connected {
        assert!(sim.step());
    }
    let t0 = sim.t0;
    let b = sim.b.take().unwrap();
    let a = std::mem::replace(&mut sim.a, Connection::connect(9, 1, LIMITS, t0, t0));
    (a, b, sim.now)
}

fn drain(c: &mut Connection, now: Instant) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut packets = Vec::new();
    while c.poll_transmit(now, &mut out) {
        packets.push(out.clone());
    }
    packets
}

fn deliver(to: &mut Connection, packets: &[Vec<u8>], now: Instant) {
    for p in packets {
        to.on_packet(&parse(p).unwrap(), now);
    }
}

#[test]
fn a_lost_retransmission_is_fast_retransmitted_again() {
    let (mut a, mut b, now) = connected(11);
    for _ in 0..8 {
        a.cc.on_ack(MSS, 1000, now);
    }
    a.write(&payload(5 * MSS, 3)).unwrap();
    let first = drain(&mut a, now);
    assert_eq!(first.len(), 5);
    let hole = parse(&first[0]).unwrap().header.seq_nr;
    // The first is lost; three later ones get through: fast retransmit.
    deliver(&mut b, &first[1..4], now);
    deliver(&mut a, &drain(&mut b, now), now);
    let resend = drain(&mut a, now);
    assert_eq!(parse(&resend[0]).unwrap().header.seq_nr, hole);
    assert_eq!(a.retransmits, 1);
    // The resend is lost too. A packet sent before it arriving says
    // nothing about it, and nor do duplicate ACKs: they may answer
    // packets sent before the resend.
    deliver(&mut b, &first[4..5], now);
    let acks = drain(&mut b, now);
    // (The first copy is progress: it newly acknowledges that packet.)
    for _ in 0..=DUP_ACK_THRESHOLD {
        deliver(&mut a, &acks, now);
    }
    assert!(a.dup_acks >= DUP_ACK_THRESHOLD);
    assert!(drain(&mut a, now).is_empty(), "no new evidence yet");
    // Three packets sent after the resend do.
    a.write(&payload(3 * MSS, 4)).unwrap();
    let later = drain(&mut a, now);
    assert_eq!(later.len(), 3, "the halved window still has room");
    for p in &later {
        deliver(&mut b, std::slice::from_ref(p), now);
        deliver(&mut a, &drain(&mut b, now), now);
    }
    let again = drain(&mut a, now);
    assert_eq!(
        parse(&again[0]).unwrap().header.seq_nr,
        hole,
        "resent again without waiting for the timer"
    );
    assert_eq!(a.retransmits, 2);
    deliver(&mut b, &again, now);
    let mut buf = vec![0u8; 8 * MSS];
    assert_eq!(b.read(&mut buf).unwrap(), 8 * MSS);
}

/// One-packet messages sent one after another once the round trip
/// jumps from 20 ms to `rtt`; returns the resends they cost.
fn resends_after_a_round_trip_jump(rtt: Duration, messages: u8) -> u64 {
    let mut sim = Sim::new(13, CLEAN, 1, 1);
    while sim.b.is_none() || sim.a.state() != State::Connected {
        assert!(sim.step());
    }
    sim.link.delay_ms = (rtt / 2).as_millis() as u64;
    let mut buf = [0u8; 256];
    for i in 0..messages {
        sim.a.write(&[i; 200]).unwrap();
        let mut got = Vec::new();
        for _ in 0..10_000 {
            if let Some(b) = sim.b.as_mut()
                && let Ok(n @ 1..) = b.read(&mut buf)
            {
                got.extend_from_slice(&buf[..n]);
            }
            if got.len() == 200 && sim.a.inflight.is_empty() {
                break;
            }
            assert!(sim.step());
        }
        assert_eq!(got, [i; 200], "message {i} at {rtt:?}");
    }
    sim.a.retransmits
}

#[test]
fn a_round_trip_jump_costs_few_retransmissions() {
    // After the round trip jumps well past the 500 ms timeout, the backoff
    // is kept until an ACK gives a sample: a first transmission that
    // outlasts the round trip, or a resent one whose ACK echoes the copy
    // it answers (the timeout was spurious). The messages after that
    // aren't resent. Resetting the backoff on any progress resends every
    // message about twice, and capping its growth across progress traps it
    // below twice the round trip, so every message is resent for good.
    for ms in [1_800, 2_500, 5_000, 10_000] {
        let rtt = Duration::from_millis(ms);
        let resends = resends_after_a_round_trip_jump(rtt, 20);
        assert!(resends <= 8, "{resends} resends at {rtt:?}");
    }
}

#[test]
fn an_ack_whose_hole_isnt_our_oldest_says_nothing_about_it() {
    let (mut a, mut b, now) = connected(16);
    for _ in 0..8 {
        a.cc.on_ack(MSS, 1000, now);
    }
    a.write(&payload(5 * MSS, 6)).unwrap();
    let sent = drain(&mut a, now);
    assert_eq!(sent.len(), 5);
    // An ACK one behind what B really has, with three bits set past its
    // (wrong) hole: its mask doesn't describe our oldest packet.
    deliver(&mut b, &sent[1..4], now);
    let acks = drain(&mut b, now);
    let mut h = parse(&acks[0]).unwrap().header;
    h.ack_nr = h.ack_nr.wrapping_sub(1);
    let mut stale = Vec::new();
    packet::encode(&h, Some(&[0b0000_1110, 0, 0, 0]), &[], &mut stale);
    a.on_packet(&parse(&stale).unwrap(), now);
    assert!(drain(&mut a, now).is_empty(), "no fast retransmit");
    assert_eq!(a.retransmits, 0);
}

#[test]
fn a_peer_that_talks_but_never_acknowledges_is_given_up_on() {
    let (mut a, mut b, mut now) = connected(14);
    a.write(&payload(3 * MSS, 5)).unwrap();
    let mut fired = 0;
    while !a.is_closed() {
        assert!(fired <= MAX_TIMEOUTS + 1, "never gave up");
        // A's data is lost every time; its ACKs of B's data get through.
        for p in drain(&mut a, now) {
            if parse(&p).unwrap().header.ty != PacketType::Data {
                deliver(&mut b, &[p], now);
            }
        }
        // B keeps sending, with an open window, acknowledging nothing new.
        let d = a.deadline().expect("data in flight keeps the timer armed");
        now = d - Duration::from_millis(1);
        b.write(&[7; 100]).unwrap();
        deliver(&mut a, &drain(&mut b, now), now);
        now = d;
        a.on_timeout(now);
        fired += 1;
    }
    assert_eq!(a.state(), State::Closed(Closed::TimedOut));
    assert_eq!(fired, MAX_TIMEOUTS + 1);
}

/// `packet` re-encoded with the advertised window replaced.
fn with_window(packet: &[u8], wnd_size: u32) -> Vec<u8> {
    let p = parse(packet).unwrap();
    let mut h = p.header;
    h.wnd_size = wnd_size;
    let mut out = Vec::new();
    packet::encode(&h, p.sack, p.payload, &mut out);
    out
}

#[test]
fn a_live_peer_with_a_closed_window_is_probed_not_given_up_on() {
    let (mut a, mut b, mut now) = connected(15);
    a.write(&[1; 100]).unwrap();
    for _ in 0..MAX_TIMEOUTS + 10 {
        // The probe doesn't fit, so the receiver drops it...
        for p in drain(&mut a, now) {
            if parse(&p).unwrap().header.ty != PacketType::Data {
                deliver(&mut b, &[p], now);
            }
        }
        // ...but keeps talking, with a window smaller than our packet.
        let d = a.deadline().expect("the probe keeps the timer armed");
        now = d - Duration::from_millis(1);
        b.write(&[7; 100]).unwrap();
        for p in drain(&mut b, now) {
            deliver(&mut a, &[with_window(&p, 50)], now);
        }
        now = d;
        a.on_timeout(now);
        assert!(!a.is_closed(), "a full receiver isn't dead");
    }
    // A window that fits the packet makes the timeouts count again.
    let mut fired = 0;
    while !a.is_closed() {
        assert!(fired <= MAX_TIMEOUTS + 1, "never gave up");
        drain(&mut a, now);
        let d = a.deadline().unwrap();
        now = d - Duration::from_millis(1);
        b.write(&[7; 100]).unwrap();
        for p in drain(&mut b, now) {
            deliver(&mut a, &[with_window(&p, 100)], now);
        }
        now = d;
        a.on_timeout(now);
        fired += 1;
    }
    assert_eq!(a.state(), State::Closed(Closed::TimedOut));
}

/// `packet` re-encoded with its timestamp and timestamp difference
/// replaced: it then echoes `timestamp_us - diff` (nothing, for `diff` 0).
fn with_timestamps(packet: &[u8], timestamp_us: u32, diff: u32) -> Vec<u8> {
    let p = parse(packet).unwrap();
    let mut h = p.header;
    h.timestamp_us = timestamp_us;
    h.timestamp_diff_us = diff;
    let mut out = Vec::new();
    packet::encode(&h, p.sack, p.payload, &mut out);
    out
}

/// The echo `packet` carries, in its sender's peer's clock.
fn echo_of(packet: &[u8]) -> u32 {
    let h = parse(packet).unwrap().header;
    h.timestamp_us.wrapping_sub(h.timestamp_diff_us)
}

/// A connected pair where A's one data packet timed out and went again:
/// returns them with the first copy, the resend and when each went out.
fn resent_once(seed: u64) -> (Connection, Connection, Vec<u8>, Vec<u8>, Instant, Instant) {
    let (mut a, b, now) = connected(seed);
    a.write(&[5; 100]).unwrap();
    let first = drain(&mut a, now);
    assert_eq!(first.len(), 1);
    let d = a.deadline().unwrap();
    a.on_timeout(d);
    let resend = drain(&mut a, d);
    assert_eq!(resend.len(), 1);
    assert_eq!(a.retransmits, 1);
    (a, b, first[0].clone(), resend[0].clone(), now, d)
}

/// What one more sample `r` makes of `before`'s smoothed round trip.
fn smoothed(before: Duration, r: Duration) -> Duration {
    before * 7 / 8 + r / 8
}

#[test]
fn an_ack_of_the_first_copy_makes_the_timeout_spurious() {
    let (mut a, mut b, first, _resend, sent, resent) = resent_once(20);
    let srtt = a.rtt.srtt();
    assert_eq!(a.rtt.backoff(), 1);
    // The first copy was only slow: it arrives after the resend went out,
    // and B's ACK echoes it.
    let at_b = resent + Duration::from_millis(300);
    deliver(&mut b, &[first], at_b);
    let ack = drain(&mut b, at_b);
    let at_a = at_b + Duration::from_millis(300);
    deliver(&mut a, &ack, at_a);
    assert!(a.inflight.is_empty());
    assert_eq!(
        a.rtt.srtt(),
        smoothed(srtt, at_a - sent),
        "sampled from the first copy"
    );
    assert_eq!(a.rtt.backoff(), 0, "the backoff is undone");
}

#[test]
fn an_ack_of_the_resend_samples_the_resend() {
    let (mut a, mut b, _first, resend, _sent, resent) = resent_once(21);
    let srtt = a.rtt.srtt();
    assert_eq!(a.rtt.backoff(), 1);
    // The first copy was lost; the resend got through.
    let at_b = resent + Duration::from_millis(40);
    deliver(&mut b, &[resend], at_b);
    let ack = drain(&mut b, at_b);
    let at_a = at_b + Duration::from_millis(40);
    deliver(&mut a, &ack, at_a);
    assert_eq!(a.rtt.srtt(), smoothed(srtt, at_a - resent));
    assert_eq!(a.rtt.backoff(), 0);
}

/// A forged ACK: its timestamp, its delay, and when it reaches A, from
/// our clock (`us`), the first copy's send time, the resend's and the
/// ACK's honest arrival.
type Forge = fn(&dyn Fn(Instant) -> u32, Instant, Instant, Instant) -> (u32, u32, Instant);

#[test]
fn an_echo_that_matches_no_transmission_gives_no_sample() {
    let cases: [(&str, Forge); 4] = [
        // It would match the first copy, but with no delay measured the
        // peer had nothing to echo.
        ("no echo", |us, sent, _, at| (us(sent) + 1, 0, at)),
        ("before the first copy", |us, sent, _, at| (us(sent), 1, at)),
        ("in the future", |us, _, _, at| {
            (us(at).wrapping_add(10_000_000), 1, at)
        }),
        // Echoing the resend the instant it went out: a zero round trip.
        ("a zero round trip", |us, _, resent, _| {
            (us(resent) + 1, 1, resent)
        }),
    ];
    for (case, forge) in cases {
        let (mut a, mut b, first, _resend, sent, resent) = resent_once(22);
        let srtt = a.rtt.srtt();
        let backed_off = a.rtt.rto();
        let at = resent + Duration::from_millis(300);
        deliver(&mut b, &[first], at);
        let ack = drain(&mut b, at);
        let epoch = a.epoch;
        let us = move |t: Instant| t.saturating_duration_since(epoch).as_micros() as u32;
        let (ts, diff, arrive) = forge(&us, sent, resent, at);
        deliver(&mut a, &[with_timestamps(&ack[0], ts, diff)], arrive);
        assert!(a.inflight.is_empty(), "{case}: still acknowledged");
        assert_eq!(a.rtt.srtt(), srtt, "{case}: Karn holds");
        assert_eq!(a.rtt.rto(), backed_off, "{case}: still backed off");
        assert_eq!(a.rtt.backoff(), 1, "{case}");
    }
}

#[test]
fn a_transmission_past_the_echo_horizon_gives_no_sample() {
    // A packet unacknowledged for longer than the horizon (closed-window
    // probing doesn't count towards giving up): even an echo that matches
    // its latest copy gives nothing, since the wrapping clock can no
    // longer order our transmissions.
    let epoch = Instant::now();
    let mut s = sent_at(epoch, 0, [0, 0]);
    s.sent_at = epoch + ECHO_HORIZON;
    let now = s.sent_at + Duration::from_millis(100);
    let us = |t: Instant| t.duration_since(epoch).as_micros() as u32;
    assert_eq!(echoed_sample(&s, us(s.sent_at), epoch, now), None);
    // Just inside it, the same echo matches.
    s.sent_at = epoch + ECHO_HORIZON - Duration::from_millis(200);
    let now = s.sent_at + Duration::from_millis(100);
    assert_eq!(
        echoed_sample(&s, us(s.sent_at), epoch, now),
        Some(Duration::from_millis(100))
    );
}

#[test]
fn an_echo_across_the_clock_wrap_still_matches() {
    // Our microsecond clock wraps every ~71.6 minutes: start just before.
    let (mut a, mut b, now) = connected(23);
    let wrap = Duration::from_micros(1 << 32);
    let mut now = now.max(a.epoch + wrap - Duration::from_millis(200));
    // A fresh exchange so the peer's echo state follows the new time.
    a.write(&[1; 10]).unwrap();
    deliver(&mut b, &drain(&mut a, now), now);
    deliver(&mut a, &drain(&mut b, now), now);
    now += Duration::from_millis(10);
    a.write(&[2; 10]).unwrap();
    let first = drain(&mut a, now);
    let sent = now;
    let d = a.deadline().unwrap();
    a.on_timeout(d);
    drain(&mut a, d);
    let srtt = a.rtt.srtt();
    let at = d + Duration::from_millis(50);
    assert!(at.duration_since(a.epoch) > wrap, "the clock wrapped");
    deliver(&mut b, &first, at);
    let ack = drain(&mut b, at);
    assert!(
        echo_of(&ack[0]) > a.micros(at),
        "the echo is from before the wrap, the ACK after it"
    );
    deliver(&mut a, &ack, at);
    assert_eq!(a.rtt.srtt(), smoothed(srtt, at - sent));
}

#[test]
fn a_peer_that_only_acknowledges_later_packets_is_given_up_on() {
    let (mut a, mut b, mut now) = connected(24);
    let oldest = a.next_seq();
    a.write(&[3; 100]).unwrap();
    let lost = drain(&mut a, now);
    assert_eq!(parse(&lost[0]).unwrap().header.seq_nr, oldest);
    let mut fired = 0;
    for _ in 0..100 {
        if a.is_closed() {
            break;
        }
        // Every copy of our oldest packet is lost; a new one each round
        // gets through and B acknowledges it selectively.
        a.write(&[4; 100]).unwrap();
        for p in drain(&mut a, now) {
            if parse(&p).unwrap().header.seq_nr != oldest {
                deliver(&mut b, &[p], now);
            }
        }
        let acks = drain(&mut b, now);
        assert!(acks.iter().any(|p| parse(p).unwrap().sack.is_some()));
        deliver(&mut a, &acks, now);
        now = a.deadline().expect("data in flight keeps the timer armed");
        a.on_timeout(now);
        fired += 1;
    }
    assert_eq!(a.state(), State::Closed(Closed::TimedOut));
    assert_eq!(fired, MAX_TIMEOUTS + 1);
}

#[test]
fn progress_resets_the_timeout_count() {
    let (mut a, mut b, mut now) = connected(25);
    // Each message times out once, then gets through: never given up on.
    for i in 0..3 * MAX_TIMEOUTS {
        a.write(&[i as u8; 100]).unwrap();
        drain(&mut a, now);
        now = a.deadline().unwrap();
        a.on_timeout(now);
        assert_eq!(a.timeouts, 1);
        deliver(&mut b, &drain(&mut a, now), now);
        deliver(&mut a, &drain(&mut b, now), now);
        assert_eq!(a.timeouts, 0, "progress");
        assert!(a.inflight.is_empty());
    }
    assert_eq!(a.state(), State::Connected);
}

fn sent_at(epoch: Instant, first_us: u64, gaps: [u64; 2]) -> Sent {
    let first = epoch + Duration::from_micros(first_us);
    let prev = first + Duration::from_micros(gaps[0]);
    Sent {
        seq: 1,
        ty: PacketType::Data,
        payload: Vec::new(),
        sent_at: prev + Duration::from_micros(gaps[1]),
        first_sent_at: first,
        prev_sent_at: prev,
        order: 0,
        transmissions: 3,
        acked: false,
        lost: false,
    }
}

/// An acknowledgement of whatever B has, with arbitrary timestamps.
#[derive(Debug, Clone)]
enum Step {
    /// Time passes; A's timer fires if due.
    Wait(u64),
    /// One of A's packets so far reaches B (indexed modulo their number).
    ToB(usize),
    /// One of B's packets so far reaches A, its timestamps optionally
    /// replaced.
    ToA(usize, Option<(u32, u32)>),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0u64..3_000).prop_map(Step::Wait),
        any::<usize>().prop_map(Step::ToB),
        (any::<usize>(), proptest::option::of(any::<(u32, u32)>()))
            .prop_map(|(i, f)| Step::ToA(i, f)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 512,
        rng_seed: RngSeed::Fixed(0x6563_686f),
        ..ProptestConfig::default()
    })]

    /// Whatever the echo, a sample is either refused or measured from one
    /// of our own transmissions, at or before the echo, so it is never
    /// shorter than the round trip the echo claims, and never zero.
    #[test]
    fn an_echoed_sample_comes_from_our_own_transmissions(
        first_us in prop_oneof![0u64..1 << 33, ((1u64 << 32) - 10_000_000)..(1 << 32)],
        gaps in [0u64..120_000_000, 0u64..120_000_000],
        // Some waits run past the echo horizon.
        wait in prop_oneof![0u64..120_000_000, 2_000_000_000u64..5_000_000_000],
        echo in prop_oneof![
            any::<u32>(),
            (0u64..400_000_000).prop_map(|o| o as u32),
        ],
        echo_from_first in any::<bool>(),
    ) {
        let epoch = Instant::now();
        let s = sent_at(epoch, first_us, gaps);
        let now = s.sent_at + Duration::from_micros(wait);
        let us = |t: Instant| t.duration_since(epoch).as_micros() as u32;
        // Half the cases put the echo near our transmissions.
        let echo = if echo_from_first { us(s.first_sent_at).wrapping_add(echo) } else { echo };
        if let Some(r) = echoed_sample(&s, echo, epoch, now) {
            prop_assert!(!r.is_zero());
            let from = now - r;
            prop_assert!([s.first_sent_at, s.prev_sent_at, s.sent_at].contains(&from));
            // The echo lies between that transmission and now.
            let e = echo.wrapping_sub(us(from));
            prop_assert!(u64::from(e) <= r.as_micros() as u64, "{e} µs past {r:?}");
            // And it's the latest transmission the echo could answer.
            for t in [s.first_sent_at, s.prev_sent_at, s.sent_at] {
                if t > from {
                    prop_assert!(echo.wrapping_sub(us(from)) < us(t).wrapping_sub(us(from)));
                }
            }
        } else {
            // Refused only when the echo is outside [first, now], the
            // transmission is too old to match, or it'd be a zero sample.
            let e = echo.wrapping_sub(us(s.first_sent_at));
            let span = us(now).wrapping_sub(us(s.first_sent_at));
            let latest = us(s.sent_at).wrapping_sub(us(s.first_sent_at));
            prop_assert!(
                e > span
                    || now.duration_since(s.first_sent_at) >= ECHO_HORIZON
                    || (now == s.sent_at && e >= latest),
                "refused an echo {e} µs into a {span} µs span"
            );
        }
    }

    /// Arbitrary timestamps on B's acknowledgements, delivered in any
    /// order and interleaved with timeouts, never panic and keep the timer
    /// within its bounds and the buffers consistent.
    #[test]
    fn forged_echoes_and_reordered_acks_keep_the_timer_sane(
        seed in 30u64..40,
        steps in proptest::collection::vec(step(), 1..80),
    ) {
        let (mut a, mut b, mut now) = connected(seed);
        for _ in 0..8 {
            a.cc.on_ack(MSS, 1000, now);
        }
        a.write(&payload(6 * MSS, 9)).unwrap();
        let mut to_b = drain(&mut a, now);
        let mut to_a: Vec<Vec<u8>> = Vec::new();
        for st in steps {
            match st {
                Step::Wait(ms) => {
                    now += Duration::from_millis(ms);
                    a.on_timeout(now);
                    to_b.extend(drain(&mut a, now));
                }
                Step::ToB(i) if !to_b.is_empty() => {
                    let p = to_b[i % to_b.len()].clone();
                    deliver(&mut b, &[p], now);
                    to_a.extend(drain(&mut b, now));
                }
                Step::ToA(i, forge) if !to_a.is_empty() => {
                    let mut p = to_a[i % to_a.len()].clone();
                    if let Some((ts, diff)) = forge {
                        p = with_timestamps(&p, ts, diff);
                    }
                    deliver(&mut a, &[p], now);
                    to_b.extend(drain(&mut a, now));
                }
                _ => {}
            }
            let rto = a.rtt.rto();
            prop_assert!((crate::ledbat::MIN_RTO..=crate::ledbat::MAX_RTO).contains(&rto), "{rto:?}");
            prop_assert!(a.rtt.srtt() <= crate::ledbat::MAX_RTO);
            prop_assert!(a.buffered() <= LIMITS.send_buffer);
            let unacked: usize = a.inflight.iter().filter(|s| !s.acked).map(|s| s.payload.len()).sum();
            prop_assert_eq!(unacked, a.unacked_bytes);
            prop_assert!(a.flight <= a.unacked_bytes);
            prop_assert!(a.timeouts <= MAX_TIMEOUTS + 1);
        }
    }
}
