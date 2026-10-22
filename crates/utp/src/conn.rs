//! One uTP connection as a sans-IO state machine: packets and time go in,
//! packets to send and bytes for the application come out. The tokio
//! layer (`socket`) drives it; the tests drive it over a simulated lossy
//! network with a virtual clock.
//!
//! Memory is bounded by configuration, whatever the peer does: unread and
//! out-of-order bytes by twice the receive window we advertise (see
//! `receive`), unacknowledged
//! and unsent bytes by the send buffer, and the number of packets on
//! either side by fixed caps.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::time::{Duration, Instant};

use crate::ledbat::{INITIAL_RTO, Ledbat, Rtt};
use crate::packet::{self, Header, MAX_SACK_LEN, Packet, PacketType};

/// Payload bytes per packet. 1200 fits the IPv6 minimum MTU after the IP,
/// UDP and uTP headers, so it survives tunnels and VPNs without discovery.
pub(crate) const MSS: usize = 1200;
/// Our packets awaiting acknowledgement, at most.
const MAX_INFLIGHT_PACKETS: usize = 256;
/// Out-of-order packets kept, at most; also how far ahead of the next
/// expected packet one may be (the selective-ACK bitmask covers 256).
const MAX_OOO: u16 = (MAX_SACK_LEN * 8) as u16;
/// Consecutive timeouts of the same data before the connection is dead.
const MAX_TIMEOUTS: u32 = 6;
/// SYN retransmissions before connecting fails.
const MAX_SYN_RETRIES: u32 = 3;
const TIME_WAIT_MIN: Duration = Duration::from_secs(4);
/// How long an accepted connection waits for the initiator to answer our
/// SYN-ACK. Each repeated SYN restarts it, so it only has to outlast the
/// longest gap in the initiator's SYN backoff (1, 2, then 4 RTOs).
const HANDSHAKE_TIMEOUT: Duration = INITIAL_RTO.saturating_mul(8);
/// Duplicate ACKs (or packets selectively acknowledged past a hole) that
/// mean the hole was lost.
const DUP_ACK_THRESHOLD: u32 = 3;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    /// The most bytes we buffer for the application (unread plus
    /// out-of-order); advertised to the peer as our window.
    pub recv_window: u32,
    /// The most bytes we hold to send (unsent plus unacknowledged).
    pub send_buffer: usize,
    /// A connection with nothing in flight that hears nothing for this
    /// long is closed as timed out.
    pub idle_timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Closed {
    /// Both sides finished cleanly.
    Done,
    Reset,
    TimedOut,
    /// Closed locally without a handshake (abandoned before connecting).
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    SynSent,
    /// Accepted; we send nothing but ACKs until the initiator shows it
    /// got our SYN-ACK. Its sequence number syncs on that SYN-ACK, so data
    /// of ours arriving before it would be misread.
    SynRecv,
    Connected,
    Closed(Closed),
}

#[derive(Debug)]
struct Sent {
    seq: u16,
    ty: PacketType,
    payload: Vec<u8>,
    sent_at: Instant,
    transmissions: u32,
    acked: bool,
    /// Considered lost; goes out again when the window allows.
    lost: bool,
}

#[derive(Debug)]
pub(crate) struct Connection {
    state: State,
    pub recv_id: u16,
    pub send_id: u16,
    limits: Limits,
    epoch: Instant,

    /// The next sequence number we'll use.
    seq_nr: u16,
    /// The last packet received in order.
    ack_nr: u16,
    /// Our clock minus the peer's timestamp on its last packet.
    reply_micro: u32,
    ack_pending: bool,
    reset_pending: bool,
    last_adv_wnd: u32,

    // Sending.
    unsent: VecDeque<u8>,
    inflight: VecDeque<Sent>,
    /// Unacknowledged bytes in `inflight` (lost ones included).
    unacked_bytes: usize,
    /// Bytes on the wire: unacknowledged and not considered lost.
    flight: usize,
    peer_wnd: u32,
    cc: Ledbat,
    rtt: Rtt,
    timer: Option<Instant>,
    /// Whether any packet arrived since the timer was last armed: a
    /// timeout only counts towards giving up when the peer went silent
    /// (a live peer with a full window isn't dead).
    heard: bool,
    /// When the last packet from the peer arrived (or the connection was
    /// created), for the idle timeout.
    last_recv: Instant,
    /// An accepted connection must hear from the initiator by then.
    handshake_deadline: Option<Instant>,
    timeouts: u32,
    dup_acks: u32,
    fin_requested: bool,
    fin_sent: bool,
    fin_acked: bool,
    /// Both directions finished: linger to re-acknowledge retransmissions
    /// (our last ACK may be lost), then close. See `time_wait_until`.
    time_wait: bool,

    // Receiving.
    readable: VecDeque<u8>,
    /// Out-of-order packets by sequence number; `None` payload is a FIN.
    ooo: HashMap<u16, Option<Vec<u8>>>,
    ooo_bytes: usize,
    eof: bool,
    /// The application went away: discard what arrives, finish the close.
    abandoned: bool,

    /// Retransmissions since the counter was last taken (for metrics).
    pub retransmits: u64,
}

impl Connection {
    /// An outgoing connection: the SYN goes out on the first transmit.
    /// BEP 29 starts at sequence 1; tests start elsewhere to cover wraps.
    pub fn connect(
        recv_id: u16,
        initial_seq: u16,
        limits: Limits,
        epoch: Instant,
        now: Instant,
    ) -> Connection {
        let mut c = Connection::new(
            recv_id,
            recv_id.wrapping_add(1),
            initial_seq,
            limits,
            epoch,
            now,
        );
        c.state = State::SynSent;
        c.inflight.push_back(Sent {
            seq: initial_seq,
            ty: PacketType::Syn,
            payload: Vec::new(),
            sent_at: now,
            transmissions: 0,
            acked: false,
            lost: true,
        });
        c.seq_nr = initial_seq.wrapping_add(1);
        c
    }

    /// An incoming connection from its SYN (already routed here by the
    /// socket: the SYN's id plus one is our receive id).
    pub fn accept(
        syn: &Header,
        initial_seq: u16,
        limits: Limits,
        epoch: Instant,
        now: Instant,
    ) -> Connection {
        let mut c = Connection::new(
            syn.conn_id.wrapping_add(1),
            syn.conn_id,
            initial_seq,
            limits,
            epoch,
            now,
        );
        c.state = State::SynRecv;
        c.handshake_deadline = Some(now + HANDSHAKE_TIMEOUT);
        c.ack_nr = syn.seq_nr;
        c.peer_wnd = syn.wnd_size;
        c.reply_micro = c.micros(now).wrapping_sub(syn.timestamp_us);
        c.ack_pending = true;
        c
    }

    fn new(
        recv_id: u16,
        send_id: u16,
        seq_nr: u16,
        limits: Limits,
        epoch: Instant,
        now: Instant,
    ) -> Connection {
        Connection {
            state: State::SynSent,
            recv_id,
            send_id,
            limits,
            epoch,
            seq_nr,
            ack_nr: 0,
            reply_micro: 0,
            ack_pending: false,
            reset_pending: false,
            last_adv_wnd: limits.recv_window,
            unsent: VecDeque::new(),
            inflight: VecDeque::new(),
            unacked_bytes: 0,
            flight: 0,
            peer_wnd: MSS as u32,
            cc: Ledbat::new(MSS, limits.send_buffer.max(MSS)),
            rtt: Rtt::new(),
            timer: None,
            heard: false,
            last_recv: now,
            handshake_deadline: None,
            timeouts: 0,
            dup_acks: 0,
            fin_requested: false,
            fin_sent: false,
            fin_acked: false,
            time_wait: false,
            readable: VecDeque::new(),
            ooo: HashMap::new(),
            ooo_bytes: 0,
            eof: false,
            abandoned: false,
            retransmits: 0,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn is_closed(&self) -> bool {
        matches!(self.state, State::Closed(_))
    }

    fn micros(&self, now: Instant) -> u32 {
        // Truncation is the point: uTP timestamps wrap every ~71 minutes.
        now.saturating_duration_since(self.epoch).as_micros() as u32
    }

    fn close_with(&mut self, why: Closed) {
        if !self.is_closed() {
            self.state = State::Closed(why);
            self.timer = None;
            self.handshake_deadline = None;
            self.unsent = VecDeque::new();
            self.inflight = VecDeque::new();
            self.ooo = HashMap::new();
            self.ooo_bytes = 0;
            self.unacked_bytes = 0;
            self.flight = 0;
        }
    }

    fn recv_window_left(&self) -> u32 {
        let used = self.readable.len() + self.ooo_bytes;
        (self.limits.recv_window as usize).saturating_sub(used) as u32
    }

    // ---- The application side -------------------------------------------

    /// Queues bytes to send; returns how many fit (0 when the buffer is
    /// full and the caller should wait).
    pub fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match self.state {
            State::Closed(why) => return Err(closed_error(why, true)),
            _ if self.fin_requested => return Err(io::ErrorKind::BrokenPipe.into()),
            _ => {}
        }
        let room = self
            .limits
            .send_buffer
            .saturating_sub(self.unsent.len() + self.unacked_bytes);
        let n = room.min(data.len());
        self.unsent.extend(&data[..n]);
        Ok(n)
    }

    /// Reads buffered bytes. `Ok(0)` for a non-empty `buf` is end of
    /// stream; `WouldBlock` means wait.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.readable.is_empty() {
            let n = buf.len().min(self.readable.len());
            for (dst, b) in buf.iter_mut().zip(self.readable.drain(..n)) {
                *dst = b;
            }
            // Reopening a window that had closed below one packet: tell
            // the peer, or it waits for its own timeout to probe.
            let wnd = self.recv_window_left();
            if self.last_adv_wnd < MSS as u32 && wnd >= MSS as u32 {
                self.ack_pending = true;
            }
            return Ok(n);
        }
        if self.eof || buf.is_empty() {
            return Ok(0);
        }
        match self.state {
            State::Closed(Closed::Done) => Ok(0),
            State::Closed(why) => Err(closed_error(why, false)),
            _ => Err(io::ErrorKind::WouldBlock.into()),
        }
    }

    /// Finish sending: a FIN follows the queued bytes.
    pub fn close(&mut self, now: Instant) {
        self.fin_requested = true;
        self.maybe_done(now);
    }

    /// The application dropped its handle: stop buffering incoming bytes
    /// and close, so the connection can finish on its own.
    pub fn abandon(&mut self, now: Instant) {
        self.abandoned = true;
        self.readable = VecDeque::new();
        if self.state == State::SynSent {
            // Nothing was ever established; just go.
            self.close_with(Closed::Aborted);
        } else {
            self.close(now);
        }
    }

    /// Gives up without a handshake (a dropped stream's linger ran out).
    pub fn expire(&mut self) {
        self.close_with(Closed::Aborted);
    }

    /// Abort: a RESET goes out on the next transmit.
    pub fn reset(&mut self) {
        if !self.is_closed() {
            self.reset_pending = matches!(self.state, State::Connected | State::SynRecv);
            self.close_with(Closed::Reset);
        }
    }

    fn maybe_done(&mut self, now: Instant) {
        if self.fin_acked && (self.eof || self.abandoned) {
            // Every packet restarts the wait, as TCP restarts 2MSL.
            self.time_wait = true;
            self.timer = Some(self.time_wait_until(now));
        }
    }

    // ---- Time ------------------------------------------------------------

    pub fn deadline(&self) -> Option<Instant> {
        if self.is_closed() {
            return None;
        }
        [self.timer, self.handshake_deadline, self.idle_deadline()]
            .into_iter()
            .flatten()
            .min()
    }

    /// Set while the connection is open with nothing of ours in flight
    /// (data in flight has the retransmission timer instead).
    fn idle_deadline(&self) -> Option<Instant> {
        (self.state == State::Connected && self.inflight.is_empty() && !self.time_wait)
            .then(|| self.last_recv + self.limits.idle_timeout)
    }

    pub fn on_timeout(&mut self, now: Instant) {
        if self.is_closed() {
            return;
        }
        // A half-open accepted connection, or a silent idle one.
        if self.handshake_deadline.is_some_and(|d| now >= d)
            || self.idle_deadline().is_some_and(|d| now >= d)
        {
            self.close_with(Closed::TimedOut);
            return;
        }
        let Some(t) = self.timer else { return };
        if now < t {
            return;
        }
        if self.time_wait {
            self.close_with(Closed::Done);
            return;
        }
        if !self.heard {
            self.timeouts += 1;
        }
        let limit = if self.state == State::SynSent {
            MAX_SYN_RETRIES
        } else {
            MAX_TIMEOUTS
        };
        if self.timeouts > limit {
            self.close_with(Closed::TimedOut);
            return;
        }
        for p in self.inflight.iter_mut().filter(|p| !p.acked && !p.lost) {
            p.lost = true;
            self.flight -= p.payload.len();
        }
        self.cc.on_timeout();
        self.rtt.back_off();
        self.dup_acks = 0;
        // Re-armed when the retransmission goes out.
        self.timer = None;
    }

    // ---- Incoming packets ------------------------------------------------

    pub fn on_packet(&mut self, p: &Packet<'_>, now: Instant) {
        let h = &p.header;
        if self.is_closed() {
            return;
        }
        if h.ty == PacketType::Reset {
            self.close_with(Closed::Reset);
            return;
        }
        if h.ty == PacketType::Syn {
            // A repeated SYN: our acknowledgement was lost.
            if matches!(self.state, State::SynRecv | State::Connected) && h.conn_id == self.send_id
            {
                self.ack_pending = true;
                if self.state == State::SynRecv {
                    // The initiator is alive and still waiting for us.
                    self.handshake_deadline = Some(now + HANDSHAKE_TIMEOUT);
                }
            }
            return;
        }
        if self.state == State::SynSent {
            let syn_seq = self.inflight.front().map(|s| s.seq);
            if Some(h.ack_nr) != syn_seq {
                return;
            }
            self.state = State::Connected;
            // The peer's first DATA carries the sequence number of this
            // packet (a STATE doesn't consume one).
            self.ack_nr = h.seq_nr.wrapping_sub(1);
            // Confirm at once, so the peer may start sending.
            self.ack_pending = true;
            self.rtt.reset_backoff();
        } else if self.state == State::SynRecv {
            self.state = State::Connected;
            self.handshake_deadline = None;
        }
        self.heard = true;
        self.last_recv = now;
        self.reply_micro = self.micros(now).wrapping_sub(h.timestamp_us);
        let reopened = self.peer_wnd < MSS as u32 && h.wnd_size >= MSS as u32;
        self.peer_wnd = h.wnd_size;
        self.process_ack(p, now);
        // The peer's window reopened: what it dropped for lack of room
        // goes again now rather than after a backed-off timeout.
        if reopened
            && let Some(front) = self.inflight.front_mut()
            && !front.acked
            && !front.lost
        {
            front.lost = true;
            self.flight -= front.payload.len();
        }
        match h.ty {
            PacketType::Data => self.receive(h.seq_nr, Some(p.payload)),
            PacketType::Fin => self.receive(h.seq_nr, None),
            _ => {}
        }
        self.maybe_done(now);
    }

    fn process_ack(&mut self, p: &Packet<'_>, now: Instant) {
        let ack = p.header.ack_nr;
        let Some(una) = self.inflight.front().map(|s| s.seq) else {
            return;
        };
        let len = self.inflight.len();
        // How many of ours this acknowledges cumulatively; an ACK for a
        // packet we never sent (or one long gone) acknowledges nothing.
        let upto = ack.wrapping_sub(una).wrapping_add(1) as usize;
        let cumulative = if upto <= len { upto } else { 0 };
        let mut acked_bytes = 0;
        let mut newest_sample = None;
        let mut mark = |s: &mut Sent, unacked: &mut usize, flight: &mut usize| {
            if s.acked {
                return;
            }
            s.acked = true;
            *unacked -= s.payload.len();
            if !s.lost {
                *flight -= s.payload.len();
            }
            acked_bytes += s.payload.len();
            if s.transmissions == 1 {
                newest_sample = Some(now.saturating_duration_since(s.sent_at));
            }
        };
        for s in self.inflight.iter_mut().take(cumulative) {
            mark(s, &mut self.unacked_bytes, &mut self.flight);
        }
        let mut sacked_past_hole = 0;
        if let Some(mask) = p.sack {
            for (j, byte) in mask.iter().enumerate() {
                for bit in 0..8 {
                    if byte & (1 << bit) == 0 {
                        continue;
                    }
                    let seq = ack.wrapping_add(2 + (j * 8 + bit) as u16);
                    let idx = seq.wrapping_sub(una) as usize;
                    if idx < len {
                        mark(
                            &mut self.inflight[idx],
                            &mut self.unacked_bytes,
                            &mut self.flight,
                        );
                        sacked_past_hole += 1;
                    }
                }
            }
        }
        if let Some(r) = newest_sample {
            self.rtt.sample(r);
        }
        let progressed = cumulative > 0;
        while self.inflight.front().is_some_and(|s| s.acked) {
            let s = self.inflight.pop_front().expect("front exists");
            if s.ty == PacketType::Fin {
                self.fin_acked = true;
            }
        }
        if acked_bytes > 0 || progressed {
            self.cc.on_ack(acked_bytes, p.header.timestamp_diff_us, now);
            self.timeouts = 0;
            self.dup_acks = 0;
            if !self.time_wait {
                self.timer = None;
                if !self.inflight.is_empty() {
                    self.arm(now);
                }
            }
        } else if p.header.ty == PacketType::State
            && ack == una.wrapping_sub(1)
            && p.header.wnd_size >= MSS as u32
        {
            // (Repeated ACKs from a full receiver are flow control, not loss.)
            self.dup_acks += 1;
        }
        // Fast retransmit: the oldest packet is presumed lost once three
        // later packets got through, or three duplicate ACKs arrive.
        if let Some(front) = self.inflight.front_mut()
            && !front.acked
            && !front.lost
            && front.transmissions == 1
            && (sacked_past_hole >= DUP_ACK_THRESHOLD || self.dup_acks >= DUP_ACK_THRESHOLD)
        {
            front.lost = true;
            self.flight -= front.payload.len();
            self.dup_acks = 0;
            self.cc.on_loss(now, self.rtt.srtt());
        }
    }

    /// A DATA (`Some`) or FIN (`None`) packet with sequence `seq`.
    fn receive(&mut self, seq: u16, payload: Option<&[u8]>) {
        self.ack_pending = true;
        if self.eof {
            return;
        }
        let dist = seq.wrapping_sub(self.ack_nr.wrapping_add(1));
        if dist >= 0x8000 {
            return; // already have it: the ACK above repeats ours
        }
        if dist > MAX_OOO || self.ooo.contains_key(&seq) {
            return;
        }
        let len = payload.map_or(0, <[u8]>::len);
        if dist > 0 {
            if (len as u32) > self.recv_window_left() || self.ooo.len() >= usize::from(MAX_OOO) {
                return;
            }
            self.ooo.insert(seq, payload.map(<[u8]>::to_vec));
            self.ooo_bytes += len;
            return;
        }
        // The packet that fills the hole is judged against unread bytes
        // only, so out-of-order data can't wedge the window shut. Buffered
        // bytes therefore stay under twice the window.
        let unread_room = (self.limits.recv_window as usize).saturating_sub(self.readable.len());
        if len > unread_room && !self.abandoned {
            return; // no room: the peer will retransmit
        }
        self.deliver(payload);
        self.ack_nr = seq;
        while let Some(next) = self.ooo.remove(&self.ack_nr.wrapping_add(1)) {
            self.ooo_bytes -= next.as_ref().map_or(0, Vec::len);
            self.ack_nr = self.ack_nr.wrapping_add(1);
            self.deliver(next.as_deref());
            if self.eof {
                self.ooo = HashMap::new();
                self.ooo_bytes = 0;
                break;
            }
        }
    }

    fn deliver(&mut self, payload: Option<&[u8]>) {
        match payload {
            None => self.eof = true,
            Some(_) if self.abandoned => {}
            Some(data) => self.readable.extend(data),
        }
    }

    // ---- Outgoing packets ------------------------------------------------

    /// Writes the next packet to send into `out` (cleared first). Returns
    /// false when there's nothing to send right now.
    pub fn poll_transmit(&mut self, now: Instant, out: &mut Vec<u8>) -> bool {
        out.clear();
        if self.reset_pending {
            self.reset_pending = false;
            let h = self.header(PacketType::Reset, self.seq_nr, now);
            packet::encode(&h, None, &[], out);
            return true;
        }
        if self.is_closed() {
            return false;
        }
        let window = self.cc.window().min(self.peer_wnd as usize);
        // Retransmissions first, oldest first.
        let flight = self.flight;
        if let Some(i) = self.inflight.iter().position(|p| p.lost && !p.acked) {
            let len = self.inflight[i].payload.len();
            if flight == 0 || flight + len <= window {
                let s = &mut self.inflight[i];
                s.lost = false;
                s.transmissions += 1;
                s.sent_at = now;
                if s.transmissions > 1 {
                    self.retransmits += 1;
                }
                let (seq, ty) = (s.seq, s.ty);
                self.flight += len;
                self.emit(ty, seq, i, now, out);
                return true;
            }
        }
        // New data.
        if !self.unsent.is_empty()
            && self.state == State::Connected
            && self.inflight.len() < MAX_INFLIGHT_PACKETS
        {
            let len = self.unsent.len().min(MSS);
            if self.flight == 0 || self.flight + len <= window {
                let payload: Vec<u8> = self.unsent.drain(..len).collect();
                self.push_sent(PacketType::Data, payload, now);
                let i = self.inflight.len() - 1;
                self.emit(PacketType::Data, self.inflight[i].seq, i, now, out);
                return true;
            }
        }
        if self.fin_requested
            && !self.fin_sent
            && self.unsent.is_empty()
            && self.state == State::Connected
            && self.inflight.len() < MAX_INFLIGHT_PACKETS
        {
            self.fin_sent = true;
            self.push_sent(PacketType::Fin, Vec::new(), now);
            let i = self.inflight.len() - 1;
            self.emit(PacketType::Fin, self.inflight[i].seq, i, now, out);
            return true;
        }
        if self.ack_pending {
            let h = self.header(PacketType::State, self.seq_nr, now);
            self.finish(h, &[], out);
            return true;
        }
        false
    }

    fn push_sent(&mut self, ty: PacketType, payload: Vec<u8>, now: Instant) {
        self.unacked_bytes += payload.len();
        self.flight += payload.len();
        self.inflight.push_back(Sent {
            seq: self.seq_nr,
            ty,
            payload,
            sent_at: now,
            transmissions: 1,
            acked: false,
            lost: false,
        });
        self.seq_nr = self.seq_nr.wrapping_add(1);
    }

    fn emit(&mut self, ty: PacketType, seq: u16, i: usize, now: Instant, out: &mut Vec<u8>) {
        let mut h = self.header(ty, seq, now);
        if ty == PacketType::Syn {
            // The SYN names the id the peer should send to us with.
            h.conn_id = self.recv_id;
            h.ack_nr = 0;
        }
        if self.timer.is_none() {
            self.arm(now);
        }
        let payload = std::mem::take(&mut self.inflight[i].payload);
        self.finish(h, &payload, out);
        self.inflight[i].payload = payload;
    }

    /// Long enough to outlast the peer's backed-off retransmission of
    /// whatever our lost ACK covered. A dropped stream's linger
    /// (`Config::linger`) cuts it short in the socket.
    fn time_wait_until(&self, now: Instant) -> Instant {
        now + (self.rtt.rto() * 4).clamp(TIME_WAIT_MIN, crate::ledbat::MAX_RTO)
    }

    fn arm(&mut self, now: Instant) {
        self.timer = Some(now + self.rtt.rto());
        self.heard = false;
    }

    fn header(&self, ty: PacketType, seq_nr: u16, now: Instant) -> Header {
        Header {
            ty,
            conn_id: self.send_id,
            timestamp_us: self.micros(now),
            timestamp_diff_us: self.reply_micro,
            wnd_size: self.recv_window_left(),
            seq_nr,
            ack_nr: self.ack_nr,
        }
    }

    fn finish(&mut self, h: Header, payload: &[u8], out: &mut Vec<u8>) {
        let mut mask = [0u8; MAX_SACK_LEN];
        let mut sack_len = 0;
        if h.ty != PacketType::Syn {
            let first = self.ack_nr.wrapping_add(2);
            for &seq in self.ooo.keys() {
                let bit = usize::from(seq.wrapping_sub(first));
                if bit < MAX_SACK_LEN * 8 {
                    mask[bit / 8] |= 1 << (bit % 8);
                    sack_len = sack_len.max((bit / 32 + 1) * 4);
                }
            }
        }
        let sack = (sack_len > 0).then(|| &mask[..sack_len]);
        packet::encode(&h, sack, payload, out);
        self.ack_pending = false;
        self.last_adv_wnd = h.wnd_size;
    }

    /// The sequence number the next packet will carry (for tests).
    #[cfg(test)]
    pub fn next_seq(&self) -> u16 {
        self.seq_nr
    }

    /// Bytes held for this connection's buffers (for the memory bound).
    pub fn buffered(&self) -> usize {
        self.unsent.len() + self.unacked_bytes + self.readable.len() + self.ooo_bytes
    }
}

fn closed_error(why: Closed, writing: bool) -> io::Error {
    match why {
        Closed::Reset => io::ErrorKind::ConnectionReset.into(),
        Closed::TimedOut => io::ErrorKind::TimedOut.into(),
        Closed::Done | Closed::Aborted if writing => io::ErrorKind::BrokenPipe.into(),
        Closed::Done | Closed::Aborted => io::ErrorKind::NotConnected.into(),
    }
}

#[cfg(test)]
mod tests;
