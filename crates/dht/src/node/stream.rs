//! Streaming peer batches (CR-1a-S, `docs/plan/2026-10-27-streaming-and-hints.md`,
//! "Bounded peer delivery").
//!
//! An admitted streaming request hands its caller a [`LookupSession`]: a
//! bounded batch receiver and a separate terminal one-shot. The actor keeps
//! one [`Stream`] per owned request. Peers go out in batches of at most
//! [`STREAM_BATCH`] as lookup replies bring them; on a full channel they
//! merge into one pending batch, and past that they're dropped and counted.
//! Termination never waits on batch-channel space.
//!
//! The caller's own session type needn't be this crate's: the actor talks
//! to it through [`SessionAdmit`] and [`SessionEnd`], so the fetch pool's
//! crate needn't depend on this one (the pipeline adapts the two).

use std::net::{IpAddr, SocketAddr};

use tokio::sync::{mpsc, oneshot};

use super::Stats;
use super::limits::{Network, source};
use super::transport::Set;

/// Endpoints per batch. Candidate default (§2.19), from the spec.
pub const STREAM_BATCH: usize = 8;
/// Batches queued per session before the actor holds one back.
/// Candidate default (§2.19), from the spec.
pub const STREAM_QUEUE: usize = 2;
/// Distinct endpoints a session remembers; once full, further endpoints
/// for that session are ignored. Candidate default (§2.19), from the spec.
pub const STREAM_SEEN: usize = 32;
/// Distinct endpoints one source (a responder's IPv4 /24 or IPv6 /48, as
/// [`source`] keys it) may add to a session, so at least four sources
/// share [`STREAM_SEEN`].
pub const STREAM_SOURCE_SHARE: usize = STREAM_SEEN / 4;

/// The share a reply from `ip` counts against; `None` for loopback, which
/// has no share (as in [`super::limits`]): no packet from off the host
/// carries a loopback source, and a local test network runs many nodes
/// on it.
pub(crate) fn stream_source(ip: IpAddr) -> Option<Network> {
    (!ip.to_canonical().is_loopback()).then(|| source(ip))
}

/// How a streamed lookup ended, as the actor reports it. It carries no
/// peers: they went out as batches. The caller reads a dropped terminal
/// sender as a source failure, never as success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupEnd {
    /// Every family leg finished: converged, or out of queries.
    Complete,
    /// The request's deadline ran out first.
    Deadline,
    /// The caller went; nobody reads this.
    Cancelled,
}

impl LookupEnd {
    pub const ALL: [LookupEnd; 3] = [
        LookupEnd::Complete,
        LookupEnd::Deadline,
        LookupEnd::Cancelled,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LookupEnd::Complete => "complete",
            LookupEnd::Deadline => "deadline",
            LookupEnd::Cancelled => "cancelled",
        }
    }
}

/// The caller's side of a streaming admission.
pub trait SessionAdmit: Send {
    /// The caller has gone: the request is refused as caller-closed.
    fn is_closed(&self) -> bool;
    /// Admitted: hand the caller `batches` and a fresh terminal channel.
    /// Returns the terminal's sending side, or `None` if the caller went
    /// before it heard (the acknowledgment failed).
    fn admit(
        self: Box<Self>,
        batches: mpsc::Receiver<Vec<SocketAddr>>,
    ) -> Option<Box<dyn SessionEnd>>;
    /// Refused: tell the caller so (not the same as dropping it, which
    /// reads as an unacknowledged request).
    fn refuse(self: Box<Self>);
}

/// The terminal's sending side.
pub trait SessionEnd: Send {
    fn end(self: Box<Self>, end: LookupEnd);
}

/// This crate's own session, for callers that use it directly (and tests).
#[derive(Debug)]
pub struct LookupSession {
    pub batches: mpsc::Receiver<Vec<SocketAddr>>,
    pub terminal: oneshot::Receiver<LookupEnd>,
}

/// A streaming admission with this crate's session type.
pub type SessionAdmission = oneshot::Sender<Option<LookupSession>>;

impl SessionAdmit for SessionAdmission {
    fn is_closed(&self) -> bool {
        oneshot::Sender::is_closed(self)
    }

    fn admit(
        self: Box<Self>,
        batches: mpsc::Receiver<Vec<SocketAddr>>,
    ) -> Option<Box<dyn SessionEnd>> {
        let (tx, terminal) = oneshot::channel();
        self.send(Some(LookupSession { batches, terminal }))
            .ok()
            .map(|()| Box::new(tx) as Box<dyn SessionEnd>)
    }

    fn refuse(self: Box<Self>) {
        let _ = self.send(None);
    }
}

impl SessionEnd for oneshot::Sender<LookupEnd> {
    fn end(self: Box<Self>, end: LookupEnd) {
        let _ = self.send(end);
    }
}

/// The actor's side of one session. Every buffer is bounded: the channel
/// holds [`STREAM_QUEUE`] batches, `pending` one more, `seen` [`STREAM_SEEN`].
pub(crate) struct Stream {
    tx: mpsc::Sender<Vec<SocketAddr>>,
    end: Box<dyn SessionEnd>,
    pending: Vec<SocketAddr>,
    seen: Set<SocketAddr>,
    /// Per source, the endpoints of `seen` it added; at most
    /// [`STREAM_SEEN`] entries, as each holds at least one.
    shares: Vec<(Network, usize)>,
    /// A batch has gone out; [`Stream::first_sent`] reports it once.
    sent: bool,
    first_reported: bool,
}

impl Stream {
    /// Allocates the session: called only once the actor has admitted it.
    pub(crate) fn admit(admit: Box<dyn SessionAdmit>) -> Option<Stream> {
        let (tx, rx) = mpsc::channel(STREAM_QUEUE);
        let end = admit.admit(rx)?;
        Some(Stream {
            tx,
            end,
            pending: Vec::with_capacity(STREAM_BATCH),
            seen: Set::default(),
            shares: Vec::new(),
            sent: false,
            first_reported: false,
        })
    }

    /// True once, after the first batch has gone out.
    pub(crate) fn first_sent(&mut self) -> bool {
        let first = self.sent && !self.first_reported;
        self.first_reported |= first;
        first
    }

    /// The caller dropped its receivers. Seen even with the channel full.
    pub(crate) fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    /// Takes a reply's peers, already normalized and checked against the
    /// public-endpoint policy, from a responder in `from` (its
    /// [`stream_source`]). New ones are batched and sent if there's room;
    /// past the pending batch they're dropped and counted, and stay unseen,
    /// so a later reply can still bring them. A source adds at most
    /// [`STREAM_SOURCE_SHARE`] of the session's [`STREAM_SEEN`]; the rest
    /// of its peers are ignored.
    pub(crate) fn offer(&mut self, from: Option<Network>, peers: &[SocketAddr], stats: &Stats) {
        if self.is_closed() {
            return;
        }
        for &p in peers {
            if self.seen.contains(&p) {
                continue;
            }
            if from.is_some_and(|net| self.share(net) >= STREAM_SOURCE_SHARE) {
                continue;
            }
            if self.seen.len() >= STREAM_SEEN {
                Stats::inc(&stats.peer_stream_seen_full);
                continue;
            }
            if self.pending.len() >= STREAM_BATCH {
                self.flush(stats);
            }
            if self.pending.len() >= STREAM_BATCH {
                Stats::inc(&stats.peer_stream_backpressure);
                continue;
            }
            self.seen.insert(p);
            if let Some(net) = from {
                match self.shares.iter_mut().find(|(n, _)| *n == net) {
                    Some((_, held)) => *held += 1,
                    None => self.shares.push((net, 1)),
                }
            }
            self.pending.push(p);
        }
        self.flush(stats);
    }

    /// The endpoints source `net` has added.
    fn share(&self, net: Network) -> usize {
        self.shares
            .iter()
            .find(|(n, _)| *n == net)
            .map_or(0, |(_, held)| *held)
    }

    /// Sends the pending batch if the channel has room. Never waits.
    pub(crate) fn flush(&mut self, stats: &Stats) {
        if self.pending.is_empty() {
            return;
        }
        // Reserve first, so a full channel costs no allocation.
        match self.tx.try_reserve() {
            Ok(permit) => {
                let batch = std::mem::replace(&mut self.pending, Vec::with_capacity(STREAM_BATCH));
                permit.send(batch);
                self.sent = true;
                Stats::inc(&stats.peer_stream_batches);
            }
            Err(mpsc::error::TrySendError::Full(())) => {}
            // The caller went; cancellation reclaims the request.
            Err(mpsc::error::TrySendError::Closed(())) => {}
        }
    }

    /// The last flush, then the terminal: one nonblocking try, discards
    /// counted, the batch sender closed, and only then the terminal sent.
    pub(crate) fn finish(mut self, end: LookupEnd, stats: &Stats) {
        let end = if self.is_closed() {
            LookupEnd::Cancelled
        } else {
            end
        };
        self.flush(stats);
        if !self.pending.is_empty() && !self.is_closed() {
            stats.peer_stream_backpressure.fetch_add(
                self.pending.len() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        drop(self.tx);
        Stats::inc(&stats.peer_stream_terminals[end as usize]);
        self.end.end(end);
    }

    #[cfg(test)]
    pub(crate) fn buffered(&self) -> (usize, usize) {
        (self.pending.len(), self.seen.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn peer(n: u16) -> SocketAddr {
        SocketAddr::from(([203, 0, 113, 1], 1000 + n))
    }

    fn session() -> (Stream, LookupSession) {
        let (tx, mut rx) = oneshot::channel();
        let stream = Stream::admit(Box::new(tx)).expect("caller waiting");
        (stream, rx.try_recv().unwrap().unwrap())
    }

    #[test]
    fn batches_are_bounded_and_backpressure_is_counted() {
        let stats = Stats::default();
        let (mut s, mut session) = session();
        let peers: Vec<_> = (0..30).map(peer).collect();
        // Two batches fill the channel, the third waits, the rest drop.
        s.offer(None, &peers, &stats);
        // The dropped ones stay unseen.
        assert_eq!(s.buffered(), (STREAM_BATCH, 3 * STREAM_BATCH));
        assert_eq!(stats.peer_stream_batches.load(Ordering::Relaxed), 2);
        assert_eq!(stats.peer_stream_backpressure.load(Ordering::Relaxed), 6);
        // Duplicates aren't new.
        s.offer(None, &peers[..4], &stats);
        assert_eq!(stats.peer_stream_backpressure.load(Ordering::Relaxed), 6);
        let first = session.batches.try_recv().unwrap();
        assert_eq!(first, peers[..8]);
        // Room again: the pending batch goes on the next flush.
        s.flush(&stats);
        assert_eq!(s.buffered().0, 0);
        assert_eq!(session.batches.try_recv().unwrap(), peers[8..16]);
        assert_eq!(session.batches.try_recv().unwrap(), peers[16..24]);
    }

    #[test]
    fn the_seen_set_is_capped_and_ignores_the_rest() {
        let stats = Stats::default();
        let (mut s, mut session) = session();
        for chunk in (0..40).map(peer).collect::<Vec<_>>().chunks(4) {
            s.offer(None, chunk, &stats);
            while session.batches.try_recv().is_ok() {}
        }
        assert_eq!(s.buffered().1, STREAM_SEEN);
        assert_eq!(stats.peer_stream_seen_full.load(Ordering::Relaxed), 8);
        assert_eq!(stats.peer_stream_backpressure.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn termination_flushes_once_counts_discards_and_closes_before_the_terminal() {
        let stats = Stats::default();
        let (mut s, mut session) = session();
        s.offer(None, &(0..24).map(peer).collect::<Vec<_>>(), &stats);
        assert_eq!(s.buffered().0, STREAM_BATCH);
        s.finish(LookupEnd::Complete, &stats);
        assert_eq!(stats.peer_stream_backpressure.load(Ordering::Relaxed), 8);
        assert_eq!(session.terminal.try_recv(), Ok(LookupEnd::Complete));
        // Both queued batches are still there to drain, then the channel ends.
        assert!(session.batches.try_recv().is_ok());
        assert!(session.batches.try_recv().is_ok());
        assert_eq!(
            session.batches.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        );
    }

    #[test]
    fn a_closed_caller_is_cancelled_even_with_a_full_channel() {
        let stats = Stats::default();
        let (mut s, session) = session();
        s.offer(None, &(0..24).map(peer).collect::<Vec<_>>(), &stats);
        drop(session);
        assert!(s.is_closed());
        s.finish(LookupEnd::Complete, &stats);
        assert_eq!(
            stats.peer_stream_terminals[LookupEnd::Cancelled as usize].load(Ordering::Relaxed),
            1
        );
        // Discards to a caller that went aren't backpressure.
        assert_eq!(stats.peer_stream_backpressure.load(Ordering::Relaxed), 0);
    }

    fn net(b: u8) -> Option<Network> {
        Some(Network(4, u64::from(b)))
    }

    /// Drains whatever batches are queued.
    fn drain(session: &mut LookupSession) -> Vec<SocketAddr> {
        let mut got = Vec::new();
        while let Ok(batch) = session.batches.try_recv() {
            got.extend(batch);
        }
        got
    }

    /// X-sec-S1-18: each source adds at most its share of the session.
    #[test]
    fn a_source_adds_at_most_its_share() {
        let stats = Stats::default();
        let (mut s, mut session) = session();
        s.offer(net(1), &(0..32).map(peer).collect::<Vec<_>>(), &stats);
        assert_eq!(s.buffered().1, STREAM_SOURCE_SHARE);
        // A second reply from the same source adds nothing new.
        s.offer(net(1), &(32..40).map(peer).collect::<Vec<_>>(), &stats);
        assert_eq!(drain(&mut session), (0..8).map(peer).collect::<Vec<_>>());
        // Other sources still get in, up to the session's cap.
        for b in 2..=4 {
            let first = 100 * u16::from(b);
            s.offer(
                net(b),
                &(first..first + 20).map(peer).collect::<Vec<_>>(),
                &stats,
            );
            assert_eq!(drain(&mut session).len(), STREAM_SOURCE_SHARE);
        }
        assert_eq!(s.buffered().1, STREAM_SEEN);
        // No share for loopback (`None`), but the session cap holds.
        s.offer(None, &(900..910).map(peer).collect::<Vec<_>>(), &stats);
        assert_eq!(s.buffered().1, STREAM_SEEN);
        assert_eq!(stats.peer_stream_seen_full.load(Ordering::Relaxed), 10);
    }

    /// X-sec-S1-18: a peer dropped while the channel is full isn't seen,
    /// so a later reply delivers it.
    #[test]
    fn a_peer_dropped_for_backpressure_can_come_again() {
        let stats = Stats::default();
        let (mut s, mut session) = session();
        let peers: Vec<_> = (0..30).map(peer).collect();
        s.offer(None, &peers, &stats);
        assert_eq!(stats.peer_stream_backpressure.load(Ordering::Relaxed), 6);
        assert_eq!(drain(&mut session), peers[..16]);
        s.offer(None, &peers[24..], &stats);
        let got = drain(&mut session);
        assert_eq!(got, peers[16..]);
    }

    proptest::proptest! {
        /// Whatever the replies, the session sees at most [`STREAM_SEEN`]
        /// peers, each source at most its share, and every peer the caller
        /// gets is distinct.
        #[test]
        fn shares_and_the_seen_cap_hold(
            replies in proptest::collection::vec((0u8..6, proptest::collection::vec(0u16..60, 0..40), proptest::bool::ANY), 0..30),
        ) {
            let stats = Stats::default();
            let (mut s, mut session) = session();
            let mut got = Vec::new();
            for (b, ports, read) in replies {
                let from = if b == 0 { None } else { net(b) };
                s.offer(from, &ports.iter().map(|p| peer(*p)).collect::<Vec<_>>(), &stats);
                if read {
                    got.extend(drain(&mut session));
                }
                proptest::prop_assert!(s.buffered().1 <= STREAM_SEEN);
                proptest::prop_assert!(s.shares.iter().all(|(_, held)| *held <= STREAM_SOURCE_SHARE));
                proptest::prop_assert!(s.shares.iter().map(|(_, held)| held).sum::<usize>() <= s.buffered().1);
            }
            got.extend(drain(&mut session));
            let n = got.len();
            got.sort_unstable();
            got.dedup();
            proptest::prop_assert_eq!(got.len(), n);
        }
    }

    #[test]
    fn admission_to_a_gone_caller_allocates_nothing() {
        let (tx, rx) = oneshot::channel::<Option<LookupSession>>();
        drop(rx);
        assert!(SessionAdmit::is_closed(&tx));
        assert!(Stream::admit(Box::new(tx)).is_none());
    }
}
