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
            } else if let Some(b) = self.b.as_mut() {
                b.on_packet(&p, now);
            } else if p.header.ty == PacketType::Syn {
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
