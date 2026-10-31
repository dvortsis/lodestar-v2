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

use std::net::SocketAddr;

use tokio::sync::{mpsc, oneshot};

use super::Stats;
use super::transport::Set;

/// Endpoints per batch. Candidate default (§2.19), from the spec.
pub const STREAM_BATCH: usize = 8;
/// Batches queued per session before the actor holds one back.
/// Candidate default (§2.19), from the spec.
pub const STREAM_QUEUE: usize = 2;
/// Distinct endpoints a session remembers; once full, further endpoints
/// for that session are ignored. Candidate default (§2.19), from the spec.
pub const STREAM_SEEN: usize = 32;

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
    /// public-endpoint policy. New ones are batched and sent if there's
    /// room; past the pending batch they're dropped and counted.
    pub(crate) fn offer(&mut self, peers: &[SocketAddr], stats: &Stats) {
        if self.is_closed() {
            return;
        }
        for &p in peers {
            if self.seen.contains(&p) {
                continue;
            }
            if self.seen.len() >= STREAM_SEEN {
                Stats::inc(&stats.peer_stream_seen_full);
                continue;
            }
            self.seen.insert(p);
            if self.pending.len() >= STREAM_BATCH {
                self.flush(stats);
            }
            if self.pending.len() >= STREAM_BATCH {
                Stats::inc(&stats.peer_stream_backpressure);
            } else {
                self.pending.push(p);
            }
        }
        self.flush(stats);
    }

    /// Sends the pending batch if the channel has room. Never waits.
    pub(crate) fn flush(&mut self, stats: &Stats) {
        if self.pending.is_empty() {
            return;
        }
        let batch = std::mem::replace(&mut self.pending, Vec::with_capacity(STREAM_BATCH));
        match self.tx.try_send(batch) {
            Ok(()) => {
                self.sent = true;
                Stats::inc(&stats.peer_stream_batches);
            }
            Err(mpsc::error::TrySendError::Full(batch)) => self.pending = batch,
            // The caller went; cancellation reclaims the request.
            Err(mpsc::error::TrySendError::Closed(_)) => {}
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
        s.offer(&peers, &stats);
        assert_eq!(s.buffered(), (STREAM_BATCH, 30));
        assert_eq!(stats.peer_stream_batches.load(Ordering::Relaxed), 2);
        assert_eq!(stats.peer_stream_backpressure.load(Ordering::Relaxed), 6);
        // Duplicates aren't new.
        s.offer(&peers[..4], &stats);
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
            s.offer(chunk, &stats);
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
        s.offer(&(0..24).map(peer).collect::<Vec<_>>(), &stats);
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
        s.offer(&(0..24).map(peer).collect::<Vec<_>>(), &stats);
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

    #[test]
    fn admission_to_a_gone_caller_allocates_nothing() {
        let (tx, rx) = oneshot::channel::<Option<LookupSession>>();
        drop(rx);
        assert!(SessionAdmit::is_closed(&tx));
        assert!(Stream::admit(Box::new(tx)).is_none());
    }
}
