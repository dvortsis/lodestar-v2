//! Re-grants on random traffic: claims, outcomes, hand-backs (evictions,
//! reconciles, expiries, undelivered re-grants), rests and re-grants to
//! some of the workers, from fixed seeds. After every step the test
//! recounts what each worker holds and what waits, from the authority's
//! own answers, and checks that:
//!
//! - a remote worker's leases, held and waiting, stay within its cap;
//! - a resting worker gains no lease;
//! - a re-grant with no limit per worker leaves no waiting key that one of
//!   its targets could take;
//! - every key that stops being in flight was ended by the test or
//!   reported lost, once, and a worker whose key was dropped rests.
//!
//! No clock and no tasks: the authority is given every time it uses. A
//! failure prints the seed and the step.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use lodestar_cluster::{BatchAnswer, Claim, Lease, LeaseAuthority, LeaseEvents, Token, WorkerId};
use lodestar_metainfo::InfoHash;
use lodestar_store::SourceId;

const SEEDS: std::ops::RangeInclusive<u64> = 1..=256;
const STEPS: usize = 600;
const KEYS: u8 = 48;
const TTL_SECS: i64 = 600;
/// More than every key at once, so a re-grant with this limit is bounded
/// only by what its targets may take.
const NO_LIMIT: usize = 256;
const REMOTE: [WorkerId; 4] = [WorkerId(1), WorkerId(2), WorkerId(3), WorkerId(4)];

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

    fn below(&mut self, n: u64) -> u64 {
        (self.next() >> 11) % n
    }

    fn pick<T: Copy>(&mut self, of: &[T]) -> T {
        of[self.below(of.len() as u64) as usize]
    }

    fn chance(&mut self, one_in: u64) -> bool {
        self.below(one_in) == 0
    }
}

/// Keys reported lost, in order.
#[derive(Default)]
struct Lost(Mutex<Vec<(InfoHash, WorkerId)>>);

impl LeaseEvents for Lost {
    fn lost(&self, key: &InfoHash, lease: &Lease, _reason: u8) {
        self.0.lock().unwrap().push((*key, lease.holder));
    }
}

fn key(k: u8) -> InfoHash {
    let mut bytes = [k; 20];
    bytes[0] = 0xA5;
    InfoHash(bytes)
}

fn keys() -> impl Iterator<Item = InfoHash> {
    (0..KEYS).map(key)
}

/// Where a key is, by the authority's own answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum At {
    Held(WorkerId, Token),
    /// Waiting for a re-grant; the worker that held it last.
    Waiting(WorkerId),
}

struct Model {
    seed: u64,
    a: LeaseAuthority,
    lost: Arc<Lost>,
    caps: HashMap<WorkerId, usize>,
    now: i64,
    /// Each key in flight, as of the last recount.
    at: HashMap<InfoHash, At>,
    /// Re-grants not yet delivered or taken back.
    undelivered: Vec<(InfoHash, Token)>,
    session: u64,
}

impl Model {
    fn new(seed: u64, rng: &mut Rng) -> Model {
        let lost = Arc::new(Lost::default());
        let a = LeaseAuthority::new(7, TTL_SECS, lost.clone());
        let default_cap = 2 + rng.below(5) as usize;
        a.set_lease_cap(Some(default_cap));
        let mut caps: HashMap<WorkerId, usize> = REMOTE.iter().map(|w| (*w, default_cap)).collect();
        // One worker with a cap of its own.
        let own = rng.pick(&REMOTE);
        let own_cap = 1 + rng.below(8) as usize;
        a.set_worker_cap(own, Some(own_cap));
        caps.insert(own, own_cap);
        Model {
            seed,
            a,
            lost,
            caps,
            now: 1,
            at: HashMap::new(),
            undelivered: Vec::new(),
            session: 1,
        }
    }

    fn held_by(&self, w: WorkerId) -> usize {
        self.at
            .values()
            .filter(|at| matches!(at, At::Held(h, _) if *h == w))
            .count()
    }

    fn waiting_of(&self, w: WorkerId) -> usize {
        self.at
            .values()
            .filter(|at| matches!(at, At::Waiting(h) if *h == w))
            .count()
    }

    /// New keys `w` may take under its cap; `None`: no cap.
    fn room(&self, w: WorkerId) -> Option<usize> {
        self.caps
            .get(&w)
            .map(|cap| cap.saturating_sub(self.held_by(w) + self.waiting_of(w)))
    }

    fn tokens_of(&self, w: WorkerId) -> Vec<(InfoHash, Token)> {
        let mut out: Vec<(InfoHash, Token)> = self
            .at
            .iter()
            .filter_map(|(k, at)| match at {
                At::Held(h, t) if *h == w => Some((*k, *t)),
                _ => None,
            })
            .collect();
        out.sort_by_key(|(k, _)| k.0);
        out
    }

    /// Recounts every key and checks what holds after any step. `ended`
    /// are the keys the step itself ended (a commit).
    fn recount(&mut self, step: usize, what: &str, ended: &HashSet<InfoHash>) {
        let ctx = format!("seed {} step {step} ({what})", self.seed);
        let before = std::mem::take(&mut self.at);
        let resting_then: Vec<(WorkerId, usize)> = REMOTE
            .iter()
            .filter(|w| self.a.resting(**w, self.now))
            .map(|w| {
                let held = before
                    .values()
                    .filter(|at| matches!(at, At::Held(h, _) if h == w))
                    .count();
                (*w, held)
            })
            .collect();
        for k in keys() {
            if let Some(lease) = self.a.get(&k) {
                self.at.insert(k, At::Held(lease.holder, lease.token));
            } else if self.a.in_flight(&k) {
                let last = match before.get(&k) {
                    Some(At::Held(w, _) | At::Waiting(w)) => *w,
                    None => panic!("{ctx}: a key waits that was never held"),
                };
                self.at.insert(k, At::Waiting(last));
            }
        }
        // The authority's counts agree with the recount.
        let held = self
            .at
            .values()
            .filter(|at| matches!(at, At::Held(..)))
            .count();
        assert_eq!(self.a.len(), held, "{ctx}: leases held");
        assert_eq!(
            self.a.pending(),
            self.at.len() - held,
            "{ctx}: keys waiting"
        );
        for w in REMOTE {
            assert_eq!(self.a.held_by(w), self.held_by(w), "{ctx}: {w:?} holds");
            let (total, cap) = (self.held_by(w) + self.waiting_of(w), self.caps[&w]);
            assert!(
                total <= cap,
                "{ctx}: {w:?} has {total} leases held and waiting, cap {cap}"
            );
        }
        // A worker resting through the step gained nothing. (A step that
        // moves the clock grants nothing, so "resting now" is enough.)
        for (w, then) in resting_then {
            let now = self.held_by(w);
            assert!(
                now <= then,
                "{ctx}: {w:?} rests and went from {then} to {now}"
            );
        }
        // Every key that left was ended by the step or reported lost, once.
        let lost: Vec<(InfoHash, WorkerId)> = std::mem::take(&mut *self.lost.0.lock().unwrap());
        let mut reported: HashSet<InfoHash> = HashSet::new();
        for (k, holder) in &lost {
            assert!(reported.insert(*k), "{ctx}: a key was reported lost twice");
            assert!(
                before.contains_key(k) && !self.at.contains_key(k),
                "{ctx}: a key reported lost was not in flight, or still is"
            );
            assert!(
                self.a.resting(*holder, self.now),
                "{ctx}: {holder:?} lost a key over its allowance and does not rest"
            );
        }
        for k in before.keys() {
            if !self.at.contains_key(k) {
                assert!(
                    ended.contains(k) != reported.contains(k),
                    "{ctx}: a key left without an outcome, or with two"
                );
            }
        }
        // Tokens that are no longer current can't be delivered or taken
        // back; forget them.
        let a = &self.a;
        self.undelivered.retain(|(k, t)| a.current(k, *t).is_some());
    }

    fn claim(&mut self, rng: &mut Rng) {
        let w = rng.pick(&REMOTE);
        let n = 1 + rng.below(4);
        let claims: Vec<Claim> = (0..n)
            .map(|_| Claim {
                key: key(rng.below(u64::from(KEYS)) as u8),
                source: SourceId::new(rng.below(4) as u8).unwrap(),
                strategy: 0,
                at: self.now,
            })
            .collect();
        let room = self.room(w).unwrap_or(usize::MAX);
        let rests = self.a.resting(w, self.now);
        let answers = self.a.claim_batch(w, &claims, self.now, None, None);
        // New leases stay within the claimer's room, and a resting worker
        // gets none.
        let mut new: HashSet<InfoHash> = HashSet::new();
        for (c, answer) in claims.iter().zip(&answers) {
            let was_held = matches!(self.at.get(&c.key), Some(At::Held(..)));
            if matches!(answer, BatchAnswer::Claimed { .. }) && !was_held {
                new.insert(c.key);
            }
        }
        assert!(
            new.len() <= room && !(rests && !new.is_empty()),
            "seed {}: {w:?} got {} new leases with room for {room} (resting: {rests})",
            self.seed,
            new.len()
        );
    }

    /// Ends a held key's lease as committed; returns the key.
    fn finish(&mut self, rng: &mut Rng) -> Option<InfoHash> {
        let mut held: Vec<(InfoHash, WorkerId, Token)> = self
            .at
            .iter()
            .filter_map(|(k, at)| match at {
                At::Held(w, t) => Some((*k, *w, *t)),
                At::Waiting(_) => None,
            })
            .collect();
        if held.is_empty() {
            return None;
        }
        held.sort_by_key(|(k, ..)| k.0);
        let (k, w, t) = rng.pick(&held);
        let done = if rng.chance(2) {
            self.a.commit(w, &k, t)
        } else {
            self.a.release(&k, t)
        };
        assert!(
            done.is_some(),
            "seed {}: a current token was refused",
            self.seed
        );
        Some(k)
    }

    fn reconcile(&mut self, rng: &mut Rng) {
        let w = rng.pick(&REMOTE);
        // The worker lists some of what it holds.
        let listed: HashSet<Token> = self
            .tokens_of(w)
            .into_iter()
            .filter(|_| rng.chance(2))
            .map(|(_, t)| t)
            .collect();
        self.session += 1;
        let _ = self.a.reconcile(w, self.session, &listed);
    }

    fn tick(&mut self, rng: &mut Rng) {
        // Short steps mostly; now and then past the TTL, or a whole
        // hand-back window.
        self.now += match rng.below(8) {
            0 => TTL_SECS + 1 + rng.below(200) as i64,
            1 => lodestar_cluster::RETURN_WINDOW_SECS + rng.below(100) as i64,
            _ => 1 + rng.below(120) as i64,
        };
        for w in REMOTE {
            if !rng.chance(3) {
                self.a.heartbeat(w, self.now);
            }
        }
        let _ = self.a.expire(self.now);
    }

    fn regrant(&mut self, rng: &mut Rng, step: usize) {
        let ctx = format!("seed {} step {step} (re-grant)", self.seed);
        // Some of the workers, never none; the built-in one now and then.
        let mut targets: Vec<WorkerId> = REMOTE.iter().copied().filter(|_| rng.chance(2)).collect();
        if targets.is_empty() {
            targets.push(rng.pick(&REMOTE));
        }
        if rng.chance(12) {
            targets.push(WorkerId::LOCAL);
        }
        let per_target = if rng.chance(2) {
            NO_LIMIT
        } else {
            1 + rng.below(3) as usize
        };
        let resting: HashSet<WorkerId> = targets
            .iter()
            .copied()
            .filter(|w| self.a.resting(*w, self.now))
            .collect();
        let out = self.a.regrant_pending(&targets, per_target, self.now);
        let mut seen: HashSet<WorkerId> = HashSet::new();
        for (w, got) in &out {
            assert!(
                targets.contains(w) && seen.insert(*w),
                "{ctx}: {w:?} was no target"
            );
            assert!(
                !resting.contains(w),
                "{ctx}: {w:?} rests and was given keys"
            );
            assert!(
                !got.is_empty() && got.len() <= per_target,
                "{ctx}: {w:?} got {}",
                got.len()
            );
            for (k, t) in got {
                assert!(
                    matches!(self.at.get(k), Some(At::Waiting(_))),
                    "{ctx}: a key that did not wait was granted again"
                );
                assert!(
                    self.a.current(k, *t).is_some_and(|l| l.holder == *w),
                    "{ctx}: a re-grant's token is not the key's lease"
                );
            }
            self.undelivered.extend(got.iter().copied());
        }
        self.recount(step, "re-grant", &HashSet::new());
        if per_target != NO_LIMIT {
            return;
        }
        // With no limit, a key still waiting is one no target can take: the
        // target rests, or has no room and didn't hold the key last.
        for (k, at) in &self.at {
            let At::Waiting(last) = at else { continue };
            for t in &targets {
                let rests = self.a.resting(*t, self.now);
                let can = !rests && (*last == *t || self.room(*t) != Some(0));
                assert!(
                    !can,
                    "{ctx}: a key {last:?} held last still waits, and {t:?} could take it \
                     (room {:?}, key byte {})",
                    self.room(*t),
                    k.0[1]
                );
            }
        }
    }

    fn settle(&mut self, rng: &mut Rng) {
        if self.undelivered.is_empty() {
            return;
        }
        let i = rng.below(self.undelivered.len() as u64) as usize;
        let (k, t) = self.undelivered.swap_remove(i);
        match rng.below(3) {
            0 => {
                let holder = self.a.current(&k, t).map(|l| l.holder);
                if let Some(w) = holder {
                    self.a.delivered(w, self.session, &[(k, t)]);
                }
            }
            1 => assert!(self.a.requeue(&k, t), "seed {}: a current token", self.seed),
            _ => assert!(
                self.a.requeue_back(&k, t),
                "seed {}: a current token",
                self.seed
            ),
        }
    }
}

fn run(seed: u64) {
    let mut rng = Rng::new(seed);
    let mut m = Model::new(seed, &mut rng);
    let none = HashSet::new();
    for step in 0..STEPS {
        match rng.below(16) {
            0..=4 => {
                m.claim(&mut rng);
                m.recount(step, "claim", &none);
            }
            5 | 6 => {
                let ended: HashSet<InfoHash> = m.finish(&mut rng).into_iter().collect();
                m.recount(step, "outcome", &ended);
            }
            7 => {
                let _ = m.a.evict(rng.pick(&REMOTE));
                m.recount(step, "eviction", &none);
            }
            8 => {
                m.reconcile(&mut rng);
                m.recount(step, "reconcile", &none);
            }
            9 | 10 => {
                m.tick(&mut rng);
                m.recount(step, "expiry", &none);
            }
            11 => {
                let until = m.now + 1 + rng.below(300) as i64;
                m.a.rest(rng.pick(&REMOTE), until);
                m.recount(step, "rest", &none);
            }
            12..=14 => m.regrant(&mut rng, step),
            _ => {
                m.settle(&mut rng);
                m.recount(step, "undelivered re-grant", &none);
            }
        }
    }
}

#[test]
fn regrants_respect_each_workers_cap_and_strand_no_key_a_target_can_take() {
    for seed in SEEDS {
        run(seed);
    }
}
