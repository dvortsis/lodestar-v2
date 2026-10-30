//! Coordinated admission (CR-ADM, `docs/decisions/2026-10-30-CR-ADM.md`):
//! the pool asks a finder for peers only while it holds one of that
//! finder's permits, one per logical request however many address
//! families the finder runs it on. Keys due a lookup wait here, in two
//! FIFOs, instead of re-asking a full finder; a returned permit wakes the
//! pool. Waiters cost a queue slot and nothing else: no task, no future,
//! no socket.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, sleep_until};

/// Interactive grants in a row while background keys wait; the next one
/// goes to background, so it keeps at least 1/5 of the grants.
pub(super) const INTERACTIVE_BURST: u32 = 4;
/// After a refusal the finder isn't asked again for this long: without it
/// the returned permit would go straight back to the same refused waiter.
/// It doesn't escalate, and the next accepted request clears it.
pub(super) const REFUSAL_HOLD: Duration = Duration::from_secs(1);

/// Why a key is due a lookup (`fetch_admission_*{kind}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WaitKind {
    /// Discovered, or restored from the saved queue.
    Arrival = 0,
    /// Its back-off ended, or interest cut it short.
    Retry = 1,
    /// Sent once and refused, unacknowledged or dropped by a pause.
    Requeue = 2,
}

/// `fetch_admission_grants_total{class}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Class {
    Interactive = 0,
    Background = 1,
}

/// One queued copy of a waiting key. It's live while the entry is still
/// waiting at this generation; anything else makes the copy stale, and
/// stale copies are skipped.
#[derive(Debug, Clone, Copy)]
pub(super) struct Waiter {
    pub key: InfoHash,
    pub generation: u64,
    pub kind: WaitKind,
}

/// The permits a granted request carries, one per finder (`None` for a
/// closed one), and the queue it was granted from.
pub(super) struct Grant {
    pub permits: Vec<Option<OwnedSemaphorePermit>>,
    pub class: Class,
}

struct Credit {
    sem: Arc<Semaphore>,
    capacity: usize,
    /// Its channel closed: never woken for or asked again.
    closed: bool,
    hold_until: Option<Instant>,
    /// A permit a wakeup took that no request has used yet.
    held: Option<OwnedSemaphorePermit>,
}

/// What the pool's wakeup waits on: a finder, its permits, and the end of
/// its refusal hold.
pub(super) type WaitTarget = (usize, Arc<Semaphore>, Option<Instant>);

pub(super) struct Admission {
    credits: Vec<Credit>,
    /// Indexed by [`Class`].
    queues: [VecDeque<Waiter>; 2],
    /// Interactive grants since the last background one, while background
    /// waited.
    burst: u32,
}

impl Admission {
    pub fn new(finders: usize, capacity: usize) -> Admission {
        Admission {
            credits: (0..finders)
                .map(|_| Credit {
                    sem: Arc::new(Semaphore::new(capacity)),
                    capacity,
                    closed: false,
                    hold_until: None,
                    held: None,
                })
                .collect(),
            queues: [VecDeque::new(), VecDeque::new()],
            burst: 0,
        }
    }

    /// At least one finder is open; with none, keys take the ordinary
    /// no-finder path.
    pub fn active(&self) -> bool {
        self.credits.iter().any(|c| !c.closed)
    }

    pub fn push(&mut self, class: Class, waiter: Waiter, front: bool) {
        let q = &mut self.queues[class as usize];
        if front {
            q.push_front(waiter);
        } else {
            q.push_back(waiter);
        }
    }

    /// Drops stale copies once a queue holds more than twice the live
    /// waiters plus 1,024, the same bound the pool's admission order uses.
    pub fn compact(&mut self, waiting: usize, live: impl Fn(&Waiter) -> bool) {
        for q in &mut self.queues {
            if q.len() > 2 * waiting + 1024 {
                q.retain(&live);
            }
        }
    }

    pub fn has_waiters(&self) -> bool {
        self.queues.iter().any(|q| !q.is_empty())
    }

    /// Whether any live waiter is queued. Stale copies at the heads are
    /// dropped on the way, so afterwards a non-empty queue's head is live.
    pub fn has_live_waiter(&mut self, live: impl Fn(&Waiter) -> bool) -> bool {
        let mut any = false;
        for q in &mut self.queues {
            while q.front().is_some_and(|w| !live(w)) {
                q.pop_front();
            }
            any |= !q.is_empty();
        }
        any
    }

    /// The next live waiter: interactive first, but never more than
    /// [`INTERACTIVE_BURST`] in a row while background waits.
    pub fn pop(&mut self, live: impl Fn(&Waiter) -> bool) -> Option<(Waiter, Class)> {
        self.has_live_waiter(&live);
        let [interactive, background] = &mut self.queues;
        let class = match (interactive.is_empty(), background.is_empty()) {
            (true, true) => return None,
            (false, true) => {
                self.burst = 0;
                Class::Interactive
            }
            (true, false) => Class::Background,
            (false, false) if self.burst < INTERACTIVE_BURST => {
                self.burst += 1;
                Class::Interactive
            }
            (false, false) => Class::Background,
        };
        if class == Class::Background {
            self.burst = 0;
        }
        let waiter = self.queues[class as usize].pop_front().expect("live head");
        Some((waiter, class))
    }

    /// A permit from every open finder, or none at all: what's taken is
    /// kept back for the next try when any finder has none to give.
    pub fn take_permits(&mut self, now: Instant) -> Option<Vec<Option<OwnedSemaphorePermit>>> {
        let mut permits = Vec::with_capacity(self.credits.len());
        let mut complete = true;
        for c in &mut self.credits {
            if c.closed {
                permits.push(None);
                continue;
            }
            if c.hold_until.is_some_and(|t| t <= now) {
                c.hold_until = None;
            }
            // A held permit waits out the hold too.
            let permit = if c.hold_until.is_some() {
                None
            } else {
                c.held
                    .take()
                    .or_else(|| c.sem.clone().try_acquire_owned().ok())
            };
            complete &= permit.is_some();
            permits.push(permit);
        }
        if complete {
            return Some(permits);
        }
        for (c, p) in self.credits.iter_mut().zip(permits) {
            if p.is_some() {
                c.held = p;
            }
        }
        None
    }

    /// The finder the pool's wakeup should wait on: the first open one it
    /// holds no permit from.
    pub fn wait_target(&self) -> Option<WaitTarget> {
        self.credits
            .iter()
            .enumerate()
            .find(|(_, c)| !c.closed && c.held.is_none())
            .map(|(i, c)| (i, c.sem.clone(), c.hold_until))
    }

    pub fn on_woken(&mut self, finder: usize, permit: OwnedSemaphorePermit) {
        self.credits[finder].held = Some(permit);
    }

    /// Gives back permits no request used; returns how many.
    pub fn drop_held(&mut self) -> u64 {
        let mut n = 0;
        for c in &mut self.credits {
            if c.held.take().is_some() {
                n += 1;
            }
        }
        n
    }

    /// Rests a finder until `until`. A permit kept back from it goes back
    /// now (returns 1, else 0), so the wakeup waits out the hold on that
    /// finder and takes a permit after it.
    pub fn hold(&mut self, finder: usize, until: Instant) -> u64 {
        let c = &mut self.credits[finder];
        c.hold_until = Some(until);
        u64::from(c.held.take().is_some())
    }

    pub fn clear_hold(&mut self, finder: usize) {
        self.credits[finder].hold_until = None;
    }

    pub fn close(&mut self, finder: usize) {
        let c = &mut self.credits[finder];
        c.closed = true;
        c.held = None;
    }

    /// Permits that requests hold now (not those a wakeup holds).
    pub fn in_use(&self) -> usize {
        self.credits
            .iter()
            .map(|c| {
                c.capacity
                    .saturating_sub(c.sem.available_permits())
                    .saturating_sub(usize::from(c.held.is_some()))
            })
            .sum()
    }
}

/// Resolves when the target finder has a permit for the pool, after its
/// refusal hold if it has one. Never resolves without a target.
pub(super) async fn acquire(target: Option<WaitTarget>) -> (usize, OwnedSemaphorePermit) {
    let Some((finder, sem, hold)) = target else {
        return std::future::pending().await;
    };
    if let Some(at) = hold {
        sleep_until(at).await;
    }
    match sem.acquire_owned().await {
        Ok(permit) => (finder, permit),
        // The pool never closes its semaphores.
        Err(_) => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(n: u8) -> Waiter {
        Waiter {
            key: InfoHash([n; 20]),
            generation: 1,
            kind: WaitKind::Arrival,
        }
    }

    #[test]
    fn interactive_bursts_are_bounded_while_background_waits() {
        let mut a = Admission::new(1, 32);
        for n in 0..10 {
            a.push(Class::Interactive, w(n), false);
        }
        for n in 100..103 {
            a.push(Class::Background, w(n), false);
        }
        let order: Vec<Class> = std::iter::from_fn(|| a.pop(|_| true).map(|(_, c)| c)).collect();
        use Class::{Background as B, Interactive as I};
        assert_eq!(order, [I, I, I, I, B, I, I, I, I, B, I, I, B]);
    }

    #[test]
    fn stale_copies_are_skipped_and_never_granted() {
        let mut a = Admission::new(1, 32);
        a.push(Class::Background, w(1), false);
        a.push(Class::Background, w(2), false);
        let live = |x: &Waiter| x.key != InfoHash([1; 20]);
        assert!(a.has_live_waiter(live));
        assert_eq!(a.pop(live).unwrap().0.key, InfoHash([2; 20]));
        assert!(!a.has_live_waiter(live));
        assert!(!a.has_waiters());
    }

    #[test]
    fn permits_are_all_or_nothing_and_holds_block_until_they_end() {
        let now = Instant::now();
        let mut a = Admission::new(2, 1);
        let both = a.take_permits(now).unwrap();
        assert_eq!(a.in_use(), 2);
        assert!(a.take_permits(now).is_none());
        drop(both);
        assert_eq!(a.hold(1, now + REFUSAL_HOLD), 0);
        assert!(a.take_permits(now).is_none(), "finder 1 is on hold");
        // Finder 0's permit is kept back, not returned or counted in use.
        assert_eq!(a.in_use(), 0);
        assert_eq!(a.wait_target().unwrap().0, 1);
        let granted = a.take_permits(now + REFUSAL_HOLD).unwrap();
        assert!(granted.iter().all(Option::is_some));
        drop(granted);
        a.close(0);
        let one = a.take_permits(now + REFUSAL_HOLD).unwrap();
        assert!(one[0].is_none() && one[1].is_some());
        assert!(a.active());
        a.close(1);
        assert!(!a.active());
    }
}
