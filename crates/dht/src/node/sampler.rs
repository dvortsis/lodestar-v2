//! Bounded BEP 51 admission. Capacity belongs to a request before it is sent.
//! An indexed expiry set has exactly one record per cooldown: no stale heap
//! records, compaction pauses, or per-packet scans of the address map.
//!
//! With `dht.sampler_cooldown_split` on (X-sampler-cooldown-split), a full
//! table no longer stops admission: the cooldown that ends soonest moves to
//! [`spill::Spill`], a bounded filter of fingerprints and end minutes that
//! can only answer "maybe cooling" for longer than the truth, never
//! shorter.
//!
//! With `dht.sample_backoff_escalate` on (CR-SAMPLE-BACKOFF-ESCALATE), a
//! node's first failed sample in a row cools for [`FIRST_FAILURE_BACKOFF`]
//! and starts a streak that lasts as long as the full cooldown it replaces,
//! holding that cooldown's table place and shares; a second failure within
//! it cools in full. A node that holds a table record waits for its sample
//! apart from the frontier and goes first (X-escalate-full-table): its
//! sample replaces its own record, so a full table doesn't hold it up.

use std::collections::{BTreeSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tokio::time::Instant;

use super::CoolingReason;
use super::limits::{Network, allocation, network};
use super::transport::{Map, Set};

use crate::compact::NodeInfo;

const MAX_FRONTIER: usize = 20_000;
const MAX_KNOWN: usize = crate::restart::MAX_ENTRIES;
const RECLAIM_PER_TICK: usize = 1024;
/// Cooldowns tried, soonest first, when one must leave a full table for the
/// spill filter.
const SPILL_TRIES: usize = 16;
/// The most addresses one IP may hold across the frontier and the table:
/// the fleet's DHT nodes per IP (`MAX_DHT_PER_IP`), so a host running
/// several nodes still has each sampled.
pub(crate) const MAX_PER_IP: u32 = 8;
/// The most addresses one IPv4 /24 may hold across the frontier and the
/// table, so no block takes more than a small share of either.
pub(crate) const MAX_PER_V4_NETWORK: u32 = 64;
/// The same for one IPv6 /64...
pub(crate) const MAX_PER_V6_NETWORK: u32 = 16;
/// ...and for one IPv6 /48, an ordinary site's allocation, so filling the
/// table takes as many allocations on IPv6 as on IPv4.
pub(crate) const MAX_PER_V6_ALLOCATION: u32 = 64;
pub(crate) const UNSUPPORTED_BACKOFF: Duration = Duration::from_secs(6 * 3600);
pub const MIN_INTERVAL: Duration = Duration::from_secs(60);
pub const MAX_INTERVAL: Duration = Duration::from_secs(6 * 3600);
/// CR-SAMPLE-BACKOFF-ESCALATE (`dht.sample_backoff_escalate`): the cooldown
/// after a node's first failed sample in a row; every later failure in a
/// row cools for the full backoff. Chosen from the sim screen within the
/// runner's bounds (the sim's 300 s sample interval to an hour):
/// `docs/decisions/2026-11-21-CR-SAMPLE-BACKOFF-ESCALATE.md`.
pub const FIRST_FAILURE_BACKOFF: Duration = Duration::from_secs(FIRST_FAILURE_SECS);
const FIRST_FAILURE_SECS: u64 = 300;
/// The largest BEP 51 share a node takes (WK-10e): the fleet planner's
/// `MAX_DHT_PER_IP`, so a cooldown is at most `MAX_INTERVAL` times this
/// (48 h).
pub const MAX_SCALE: u8 = 8;
/// Tokens for records the restart proof made (CR-0d): restored at start or
/// sealed at shutdown. Wire transaction ids start in the lower half and
/// only count up, so they never reach this bit.
const PROOF_TOKEN: u64 = 1 << 63;
/// Within proof tokens: restored from a snapshot (for the counters).
const RESTORED: u64 = 1 << 62;

#[derive(Debug, Clone, Copy)]
enum State {
    /// `streak`: the node's last sample failed and escalation is on, so a
    /// failure of this one cools for the full backoff; it holds the
    /// streak's `forget`, which an abort restores.
    InFlight { token: u64, streak: Option<Instant> },
    /// `struck`: a first failure's short cooldown (escalation on); when it
    /// ends the record becomes [`State::Struck`] instead of leaving. It's in
    /// `short` and `struck`, not `expiry`.
    Cooling {
        until: Instant,
        token: u64,
        struck: bool,
        /// Why it cools (`dht_sample_cooling_reason`).
        reason: CoolingReason,
    },
    /// A first failure's short cooldown ended: the node may be sampled, and
    /// the streak is remembered until `forget`, the failure plus the full
    /// backoff, when the switch-off cooldown would have ended (escalation
    /// on only). Not a restriction, so the snapshot doesn't list it; it
    /// holds its table place and shares until `forget`, as that cooldown
    /// would have, and a full split table moves it to the spill filter as a
    /// cooldown to `forget`.
    Struck { forget: Instant, token: u64 },
}

#[derive(Debug)]
pub(crate) struct Sampler {
    states: Map<SocketAddr, State>,
    expiry: BTreeSet<(Instant, u64, SocketAddr)>,
    frontier: VecDeque<NodeInfo>,
    /// Candidates that hold a table record, escalation on only
    /// (X-escalate-full-table): a streak whose short cooldown ended, or a
    /// cooldown not yet pruned. Each one's sample replaces its own record,
    /// so it needs no free place: [`Sampler::next`] hands these out first,
    /// a full table included. At most [`MAX_FRONTIER`], apart from the
    /// frontier's own bound. Always empty with the switch off.
    held: VecDeque<NodeInfo>,
    /// Every queued address, in `frontier` or `held`.
    queued: Set<SocketAddr>,
    /// How many addresses each IP and each block holds, queued or in
    /// `states` (an address in both counts once).
    per_ip: Map<IpAddr, u32>,
    per_network: Map<Network, u32>,
    /// IPv6 /48s ([`allocation`]).
    per_allocation: Map<Network, u32>,
    capacity: usize,
    next_proof: u64,
    /// Restored records still cooling, and the running totals of
    /// candidates they turned away and of those that expired unsampled.
    restored_live: usize,
    restored_refused: u64,
    restored_honoured: u64,
    /// `dht.sampler_cooldown_split`.
    split: bool,
    /// `dht.sample_backoff_escalate`.
    escalate: bool,
    /// Streaks (escalation on only): `short` holds each first failure's
    /// short cooldown by its end; `struck` holds every streak, cooling or
    /// [`State::Struck`], by its `forget`. A plain cooldown is in `expiry`
    /// alone.
    short: BTreeSet<(Instant, u64, SocketAddr)>,
    struck: BTreeSet<(Instant, u64, SocketAddr)>,
    /// Cooldowns moved out of a full table: made at the first move, so
    /// `None` until then and always with the switch off.
    spill: Option<spill::Spill>,
    /// Time the sampler couldn't admit while sampling was allowed, and
    /// when that was last checked ([`Sampler::full_time`]).
    full: Duration,
    full_checked: Option<Instant>,
    /// Cooling records by [`CoolingReason`], kept by [`Sampler::put`] and
    /// [`Sampler::take`], the only ways `states` changes.
    cooling_by: [usize; CoolingReason::ALL.len()],
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new(false)
    }
}

impl Sampler {
    /// `split` is `dht.sampler_cooldown_split`; off is today's sampler.
    pub fn new(split: bool) -> Self {
        Self {
            states: Map::default(),
            expiry: BTreeSet::new(),
            frontier: VecDeque::new(),
            held: VecDeque::new(),
            queued: Set::default(),
            per_ip: Map::default(),
            per_network: Map::default(),
            per_allocation: Map::default(),
            capacity: initial_capacity(),
            next_proof: 0,
            restored_live: 0,
            restored_refused: 0,
            restored_honoured: 0,
            split,
            escalate: false,
            short: BTreeSet::new(),
            struck: BTreeSet::new(),
            spill: None,
            full: Duration::ZERO,
            full_checked: None,
            cooling_by: [0; CoolingReason::ALL.len()],
        }
    }

    /// `escalate` is `dht.sample_backoff_escalate`; off is today's
    /// sampler, which never makes a [`State::Struck`] record.
    pub fn with_escalation(self, escalate: bool) -> Self {
        Self { escalate, ..self }
    }

    #[cfg(test)]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            ..Self::default()
        }
    }

    /// The split sampler with a small table, for tests.
    #[cfg(test)]
    pub fn split_with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            ..Self::new(true)
        }
    }

    /// Whether the table lets `addr` be sampled now.
    fn table_eligible(&self, addr: &SocketAddr, now: Instant) -> bool {
        match self.states.get(addr) {
            None => true,
            Some(State::Cooling { until, .. }) => *until <= now,
            Some(State::Struck { .. }) => true,
            Some(State::InFlight { .. }) => false,
        }
    }

    /// Whether the spill filter may hold a live cooldown for `addr`.
    fn spilled(&self, addr: &SocketAddr, now: Instant) -> bool {
        self.spill.as_ref().is_some_and(|s| s.contains(addr, now))
    }

    fn eligible(&self, addr: &SocketAddr, now: Instant) -> bool {
        self.table_eligible(addr, now) && !self.spilled(addr, now)
    }

    /// Frees table places until one is free, by moving the cooldowns that
    /// end soonest into the spill filter (switch on only). False when the
    /// table is still full: the switch is off, nothing is cooling, or the
    /// filter is at its bound. A moved cooldown is never shortened: the
    /// filter keeps it to the end of its minute. With escalation on, a
    /// streak moves as the cooldown it stands in for: to its `forget`, the
    /// end its switch-off cooldown would have had, in that order among the
    /// others. That lengthens a short first cooldown back to the switch-off
    /// one (never shorter), and the streak itself is forgotten.
    fn make_room(&mut self, now: Instant) -> bool {
        while self.states.len() >= self.capacity {
            if !self.split {
                return false;
            }
            // The soonest of each index, merged by the end each would be
            // moved with.
            let mut tries: Vec<(Instant, u64, SocketAddr)> = self
                .expiry
                .iter()
                .take(SPILL_TRIES)
                .chain(self.struck.iter().take(SPILL_TRIES))
                .copied()
                .collect();
            tries.sort_unstable();
            // The filter's clock starts at its first move.
            let spill = self.spill.get_or_insert_with(|| spill::Spill::new(now));
            // The soonest that fits: a refusal (both its groups full) is
            // rare, so a few tries find room if there is any.
            let Some(&(end, token, addr)) = tries
                .iter()
                .take(SPILL_TRIES)
                .find(|(end, _, addr)| spill.insert(addr, *end, now))
            else {
                return false;
            };
            if self.struck.remove(&(end, token, addr)) {
                if let Some(State::Cooling { until, .. }) = self.states.get(&addr) {
                    self.short.remove(&(*until, token, addr));
                }
                self.take(&addr);
                self.recount(&addr, true);
                continue;
            }
            self.expiry.remove(&(end, token, addr));
            if matches!(self.states.get(&addr), Some(State::Cooling { token: t, .. }) if *t == token)
            {
                self.take(&addr);
                self.recount(&addr, true);
                // A restored record that moves is honoured: the filter
                // keeps it to its end at least (monitoring, P5).
                self.retire(token);
            }
        }
        true
    }

    /// Removes a record's index entries (not the record): `expiry` for a
    /// plain cooldown, `short` and `struck` for a streak. Returns whether
    /// it was a streak.
    fn unindex(&mut self, addr: SocketAddr, state: State) -> bool {
        match state {
            State::Cooling {
                until,
                token,
                struck: true,
                ..
            } => {
                self.short.remove(&(until, token, addr));
                self.struck.remove(&(forget_of(until), token, addr));
                true
            }
            State::Cooling { until, token, .. } => {
                self.expiry.remove(&(until, token, addr));
                false
            }
            State::Struck { forget, token } => {
                self.struck.remove(&(forget, token, addr));
                true
            }
            State::InFlight { streak, .. } => streak.is_some(),
        }
    }

    /// A record's streak end (`forget`), if it has one.
    fn streak_of(state: State) -> Option<Instant> {
        match state {
            State::Cooling {
                until,
                struck: true,
                ..
            } => Some(forget_of(until)),
            State::Struck { forget, .. } => Some(forget),
            State::InFlight { streak, .. } => streak,
            State::Cooling { .. } => None,
        }
    }

    fn tracked(&self, addr: &SocketAddr) -> bool {
        self.queued.contains(addr) || self.states.contains_key(addr)
    }

    /// Whether a new address fits its IP's and its block's shares.
    /// Loopback is never a remote node (the public policy refuses it), so
    /// it has no share: a local test network runs many nodes on it.
    fn has_room(&self, addr: &SocketAddr, now: Instant) -> bool {
        let ip = addr.ip().to_canonical();
        if ip.is_loopback() {
            return true;
        }
        let net_cap = if ip.is_ipv4() {
            MAX_PER_V4_NETWORK
        } else {
            MAX_PER_V6_NETWORK
        };
        // With the split on, moved cooldowns still hold their shares.
        let (si, sn, sa) = self
            .spill
            .as_ref()
            .map_or((0, 0, 0), |s| s.shares(addr, now));
        count(&self.per_ip, ip) + si < MAX_PER_IP
            && count(&self.per_network, network(ip)) + sn < net_cap
            && allocation(ip)
                .is_none_or(|a| count(&self.per_allocation, a) + sa < MAX_PER_V6_ALLOCATION)
    }

    /// Updates the shares after `addr` was (`was`) or wasn't tracked
    /// before a change.
    fn recount(&mut self, addr: &SocketAddr, was: bool) {
        let now = self.tracked(addr);
        if was == now {
            return;
        }
        let ip = addr.ip().to_canonical();
        let net = network(ip);
        let alloc = allocation(ip);
        if now {
            *self.per_ip.entry(ip).or_default() += 1;
            *self.per_network.entry(net).or_default() += 1;
            if let Some(a) = alloc {
                *self.per_allocation.entry(a).or_default() += 1;
            }
        } else {
            decrement(&mut self.per_ip, ip);
            decrement(&mut self.per_network, net);
            if let Some(a) = alloc {
                decrement(&mut self.per_allocation, a);
            }
        }
    }

    /// Queues `node` on the frontier if it's eligible, not already queued,
    /// and a new address fits its IP's and block's shares. With escalation
    /// on, a node that holds a table record goes to the held queue instead,
    /// under that queue's own bound (when it's full, the node is refused
    /// even if the frontier has room). True when it was queued (CR-1b counts
    /// these as new).
    pub fn offer(&mut self, mut node: NodeInfo, now: Instant) -> bool {
        node.addr = lodestar_network_policy::normalize(node.addr);
        if matches!(self.states.get(&node.addr), Some(State::Cooling { until, token, .. })
            if *until > now && token & (PROOF_TOKEN | RESTORED) == PROOF_TOKEN | RESTORED)
        {
            self.restored_refused += 1;
        }
        let was = self.tracked(&node.addr);
        // X-escalate-full-table: with escalation on, a node that holds a
        // record waits apart, so neither a full frontier nor a full table
        // holds it up. It's already tracked, so its shares don't change.
        if self.escalate && self.states.contains_key(&node.addr) {
            if self.held.len() < MAX_FRONTIER
                && !self.queued.contains(&node.addr)
                && self.table_eligible(&node.addr, now)
                && !self.spilled(&node.addr, now)
            {
                self.queued.insert(node.addr);
                self.held.push_back(node);
                return true;
            }
            return false;
        }
        if self.frontier.len() < MAX_FRONTIER
            && !self.queued.contains(&node.addr)
            && self.table_eligible(&node.addr, now)
            && (was || self.has_room(&node.addr, now))
            && !self.spilled(&node.addr, now)
        {
            self.queued.insert(node.addr);
            self.frontier.push_back(node);
            self.recount(&node.addr, was);
            return true;
        }
        false
    }

    pub fn next(&mut self, now: Instant) -> Option<NodeInfo> {
        // A node that holds a record goes first, full table or not: its
        // sample replaces that record (`reserve`), so it needs no place.
        if let Some(n) = self.next_held(now) {
            return Some(n);
        }
        // Saturation leaves candidates queued. A later maintenance tick
        // reclaims capacity; callers never spin looking for a free slot.
        // With the split on, a full table first moves cooldowns out.
        if self.states.len() >= self.capacity && (self.frontier.is_empty() || !self.make_room(now))
        {
            return None;
        }
        while let Some(n) = self.frontier.pop_front() {
            self.queued.remove(&n.addr);
            self.recount(&n.addr, true);
            if self.eligible(&n.addr, now) {
                return Some(n);
            }
        }
        None
    }

    /// The first held candidate that still holds a record and may be
    /// sampled (X-escalate-full-table). One whose record left while it
    /// waited (its streak ended, or it moved to the spill filter) is a
    /// newcomer again: it's offered as one, to the frontier. One that may
    /// not be sampled is dropped, as the frontier drops one.
    fn next_held(&mut self, now: Instant) -> Option<NodeInfo> {
        while let Some(n) = self.held.pop_front() {
            self.queued.remove(&n.addr);
            if self.states.contains_key(&n.addr) {
                // Still tracked by its record: its shares don't change.
                if self.eligible(&n.addr, now) {
                    return Some(n);
                }
                continue;
            }
            self.recount(&n.addr, true);
            self.offer(n, now);
        }
        None
    }

    pub fn reserve(&mut self, addr: SocketAddr, token: u64, now: Instant) -> bool {
        if !self.eligible(&addr, now) {
            return false;
        }
        let was = self.tracked(&addr);
        // The streak carries into the sample, so its failure escalates.
        let streak = match self.take(&addr) {
            Some(state @ State::Cooling { token, .. }) => {
                self.unindex(addr, state);
                self.retire(token);
                Self::streak_of(state)
            }
            Some(state) => {
                self.unindex(addr, state);
                Self::streak_of(state)
            }
            None => None,
        }
        // A streak that has ended but wasn't pruned yet (an abort can bring
        // one back after its `forget`) is no streak.
        .filter(|forget| *forget > now);
        if self.states.len() >= self.capacity {
            self.make_room(now);
        }
        if self.states.len() >= self.capacity || !(was || self.has_room(&addr, now)) {
            self.recount(&addr, was);
            return false;
        }
        self.put(addr, State::InFlight { token, streak });
        self.recount(&addr, was);
        true
    }

    /// A reserved sample wasn't sent: the record goes, as with the switch
    /// off, except that a streak it carried stays (it's no restriction),
    /// so a failed send can't reset one.
    pub fn abort(&mut self, addr: SocketAddr, token: u64) -> bool {
        let Some(State::InFlight { token: t, streak }) = self.states.get(&addr).copied() else {
            return false;
        };
        if t != token {
            return false;
        }
        if let Some(forget) = streak {
            self.put(addr, State::Struck { forget, token });
            self.struck.insert((forget, token, addr));
        } else {
            self.take(&addr);
            self.recount(&addr, true);
        }
        true
    }

    /// [`complete_scaled`](Self::complete_scaled) at scale 1. The actor
    /// always names the scale; tests use this.
    #[cfg(test)]
    pub fn complete(&mut self, addr: SocketAddr, token: u64, wait: Duration, now: Instant) -> bool {
        self.complete_scaled(addr, token, wait, 1, now)
    }

    /// The node answered that it doesn't do BEP 51 (a reply without its
    /// fields, or a KRPC 204): [`UNSUPPORTED_BACKOFF`], as
    /// [`complete`](Self::complete) would give it, counted as
    /// [`CoolingReason::Unsupported`].
    pub fn complete_unsupported(&mut self, addr: SocketAddr, token: u64, now: Instant) -> bool {
        self.complete_as(
            addr,
            token,
            UNSUPPORTED_BACKOFF,
            1,
            CoolingReason::Unsupported,
            now,
        )
    }

    /// [`complete`](Self::complete), cooling `scale` times as long (WK-10e,
    /// plan Q9): the node's own interval is bounded first, then multiplied,
    /// so N same-IP members together sample it no faster than one install.
    /// `scale` is taken as `1..=MAX_SCALE`; the multiply saturates.
    pub fn complete_scaled(
        &mut self,
        addr: SocketAddr,
        token: u64,
        wait: Duration,
        scale: u8,
        now: Instant,
    ) -> bool {
        self.complete_as(addr, token, wait, scale, CoolingReason::Interval, now)
    }

    fn complete_as(
        &mut self,
        addr: SocketAddr,
        token: u64,
        wait: Duration,
        scale: u8,
        reason: CoolingReason,
        now: Instant,
    ) -> bool {
        // A sample's token is a wire transaction id, always below the
        // proof's bit (the actor never hands out one at or past it), which
        // is how a snapshot tells a sample's cooldown from a seal.
        debug_assert!(token & PROOF_TOKEN == 0, "a wire token below PROOF_TOKEN");
        if !matches!(self.states.get(&addr), Some(State::InFlight { token: t, .. }) if *t == token)
        {
            return false;
        }
        let scale = scale.clamp(1, MAX_SCALE);
        let until = now
            + wait
                .clamp(MIN_INTERVAL, MAX_INTERVAL)
                .saturating_mul(u32::from(scale));
        // Any completion ends a streak: an answer resets it, and a full
        // backoff needs no memory after it.
        self.cool(addr, token, until, false, reason);
        true
    }

    /// A sample failed: it timed out, or the reply was an error or
    /// inconsistent. With escalation off it cools for `backoff`, as
    /// [`complete`](Self::complete) does. With escalation on, a node's
    /// first failure in a row cools for [`FIRST_FAILURE_BACKOFF`] (when
    /// `backoff` is longer) and starts a streak that lasts until the
    /// failure plus `backoff`, the end of the cooldown it replaces; a
    /// failure within the streak cools for `backoff` and ends it, as an
    /// answer does.
    ///
    /// This is a timeout ([`CoolingReason::Timeout`]);
    /// [`fail_reply`](Self::fail_reply) is the same for a reply.
    pub fn fail(&mut self, addr: SocketAddr, token: u64, backoff: Duration, now: Instant) -> bool {
        self.fail_as(addr, token, backoff, CoolingReason::Timeout, now)
    }

    /// [`fail`](Self::fail) for an error reply (not a 204) or an
    /// inconsistent one ([`CoolingReason::Error`]).
    pub fn fail_reply(
        &mut self,
        addr: SocketAddr,
        token: u64,
        backoff: Duration,
        now: Instant,
    ) -> bool {
        self.fail_as(addr, token, backoff, CoolingReason::Error, now)
    }

    fn fail_as(
        &mut self,
        addr: SocketAddr,
        token: u64,
        backoff: Duration,
        cause: CoolingReason,
        now: Instant,
    ) -> bool {
        debug_assert!(token & PROOF_TOKEN == 0, "a wire token below PROOF_TOKEN");
        let Some(State::InFlight { token: t, streak }) = self.states.get(&addr).copied() else {
            return false;
        };
        if t != token {
            return false;
        }
        let struck = streak.is_some();
        let backoff = backoff.clamp(MIN_INTERVAL, MAX_INTERVAL);
        // A streak's end is derived from its short cooldown's, so only the
        // full backoff starts one (every caller passes it).
        if self.escalate && !struck && backoff == MAX_INTERVAL {
            self.cool(
                addr,
                token,
                now + FIRST_FAILURE_BACKOFF,
                true,
                CoolingReason::FirstFailure,
            );
        } else {
            self.cool(addr, token, now + backoff, false, cause);
        }
        true
    }

    fn cool(
        &mut self,
        addr: SocketAddr,
        token: u64,
        until: Instant,
        struck: bool,
        reason: CoolingReason,
    ) {
        self.put(
            addr,
            State::Cooling {
                until,
                token,
                struck,
                reason,
            },
        );
        if struck {
            self.short.insert((until, token, addr));
            self.struck.insert((forget_of(until), token, addr));
        } else {
            self.expiry.insert((until, token, addr));
        }
    }

    /// Sets `addr`'s record, keeping [`Sampler::cooling_by`] exact.
    fn put(&mut self, addr: SocketAddr, state: State) -> Option<State> {
        if let State::Cooling { reason, .. } = state {
            self.cooling_by[reason as usize] += 1;
        }
        let old = self.states.insert(addr, state);
        if let Some(old) = old {
            self.uncount(old);
        }
        old
    }

    /// Removes `addr`'s record, keeping [`Sampler::cooling_by`] exact.
    fn take(&mut self, addr: &SocketAddr) -> Option<State> {
        let old = self.states.remove(addr);
        if let Some(old) = old {
            self.uncount(old);
        }
        old
    }

    fn uncount(&mut self, old: State) {
        if let State::Cooling { reason, .. } = old {
            let n = &mut self.cooling_by[reason as usize];
            // Only `put` adds Cooling records, so this never runs dry.
            debug_assert!(*n > 0, "{reason:?} released more than counted");
            *n = n.saturating_sub(1);
        }
    }

    pub fn prune(&mut self, now: Instant) {
        if let Some(spill) = &mut self.spill {
            spill.sweep(now);
        }
        for _ in 0..RECLAIM_PER_TICK {
            let Some(&(until, token, addr)) = self.expiry.first() else {
                break;
            };
            if until > now {
                break;
            }
            self.expiry.pop_first();
            if matches!(self.states.get(&addr), Some(State::Cooling { token: t, .. }) if *t == token)
            {
                self.take(&addr);
                self.recount(&addr, true);
                self.retire(token);
            }
        }
        // A first failure's short cooldown ended: the node may be sampled,
        // and the streak stays until its `forget`.
        for _ in 0..RECLAIM_PER_TICK {
            let Some(&(until, token, addr)) = self.short.first() else {
                break;
            };
            if until > now {
                break;
            }
            self.short.pop_first();
            if matches!(self.states.get(&addr), Some(State::Cooling { token: t, struck: true, .. }) if *t == token)
            {
                let forget = forget_of(until);
                self.put(addr, State::Struck { forget, token });
            }
        }
        for _ in 0..RECLAIM_PER_TICK {
            let Some(&(forget, token, addr)) = self.struck.first() else {
                break;
            };
            if forget > now {
                break;
            }
            self.struck.pop_first();
            match self.states.get(&addr).copied() {
                Some(State::Struck { token: t, .. }) if t == token => {}
                // Its short cooldown ends before its `forget`, so this
                // needs `short` to lag by nearly the full backoff; if it
                // ever did, the record still leaves with both entries.
                Some(State::Cooling {
                    until,
                    token: t,
                    struck: true,
                    ..
                }) if t == token => {
                    self.short.remove(&(until, token, addr));
                }
                _ => continue,
            }
            self.take(&addr);
            self.recount(&addr, true);
        }
    }

    fn proof_token(&mut self, restored: bool) -> u64 {
        self.next_proof += 1;
        PROOF_TOKEN | if restored { RESTORED } else { 0 } | self.next_proof
    }

    /// An expired record left the table: count a restored one as honoured.
    fn retire(&mut self, token: u64) {
        if token & (PROOF_TOKEN | RESTORED) == PROOF_TOKEN | RESTORED {
            self.restored_live -= 1;
            self.restored_honoured += 1;
        }
    }

    /// Seeds a restriction restored by the restart proof (S2). It is never
    /// refused for capacity: dropping one would shorten it, and a snapshot
    /// holds at most the table's bound. An existing later record wins.
    /// Sampling is gated until seeding is done, so nothing is in flight.
    pub fn restore(&mut self, addr: SocketAddr, until: Instant) {
        let addr = lodestar_network_policy::normalize(addr);
        match self.states.get(&addr).copied() {
            // Can't happen (sampling waits for seeding), but if it did, the
            // restored restriction wins: the in-flight sample's completion
            // then fails its token check and can't shorten it.
            Some(State::InFlight { .. }) => {
                debug_assert!(false, "a sample was in flight before seeding");
            }
            // Can't happen either (nothing was sampled yet); the
            // restriction replaces the streak.
            Some(state @ State::Struck { .. }) => {
                self.unindex(addr, state);
            }
            Some(State::Cooling { until: u, .. }) if u >= until => return,
            Some(state @ State::Cooling { token, .. }) => {
                self.unindex(addr, state);
                if token & (PROOF_TOKEN | RESTORED) == PROOF_TOKEN | RESTORED {
                    self.restored_live -= 1;
                }
            }
            None => {}
        }
        let was = self.tracked(&addr);
        let token = self.proof_token(true);
        self.cool(addr, token, until, false, CoolingReason::Restored);
        self.restored_live += 1;
        // Never refused for its shares, as for capacity; it still counts.
        self.recount(&addr, was);
    }

    /// S8: every sample still in flight becomes a cooldown ending at
    /// `until` under a fresh token, so a late reply or timeout for the old
    /// transaction fails `complete` and can't shorten it (P6).
    pub fn seal_in_flight(&mut self, until: Instant) {
        let flying: Vec<SocketAddr> = self
            .states
            .iter()
            .filter(|(_, s)| matches!(s, State::InFlight { .. }))
            .map(|(a, _)| *a)
            .collect();
        for addr in flying {
            let token = self.proof_token(false);
            self.cool(addr, token, until, false, CoolingReason::Sealed);
        }
    }

    /// Every live cooldown, for the snapshot (S10). Call after
    /// [`Sampler::seal_in_flight`]: in-flight records aren't listed.
    ///
    /// A cooldown from a sample's answer (a wire token, below
    /// `PROOF_TOKEN`) is listed as at most `MAX_INTERVAL` from `now`,
    /// within the restart proof's horizon (WK-10e). Unscaled, it ends by
    /// then anyway; scaled (up to 48 h), only this member's extra share is
    /// cut, since the node's own interval (≤ `MAX_INTERVAL`) ran from a
    /// sample before `now`. The live cooldown keeps its full length, and a
    /// sealed or restored record (a proof token) keeps its own end.
    pub fn restrictions(&self, now: Instant) -> impl Iterator<Item = (SocketAddr, Instant)> + '_ {
        let bound = now + MAX_INTERVAL;
        self.states.iter().filter_map(move |(addr, s)| match s {
            State::Cooling { until, token, .. } if *until > now => {
                let until = if token & PROOF_TOKEN == 0 {
                    (*until).min(bound)
                } else {
                    *until
                };
                Some((*addr, until))
            }
            _ => None,
        })
    }

    /// The spill filter for the restart snapshot (X-split-resume-gate):
    /// every live cooldown in it, with the minutes left after `mono`,
    /// capped at `horizon` as the listed ones are. `None` when nothing in
    /// it is live (always with the switch off).
    pub fn spill_export(
        &self,
        mono: Instant,
        horizon: Duration,
    ) -> Option<crate::restart::Spilled> {
        self.spill.as_ref().and_then(|s| s.export(mono, horizon))
    }

    /// Rebuilds the spill filter from the restart snapshot's
    /// (X-split-resume-gate), whatever the switch: with it off, the filter
    /// only refuses its restored cooldowns and holds their shares until
    /// they end, and nothing new moves in. Sampling waits for seeding, so
    /// the filter is still empty; if it ever weren't, or the section
    /// couldn't be rebuilt, the restored cooldowns can't be kept one by
    /// one: this returns the latest of their ends, and no sample may go to
    /// anyone before it (the hold the snapshot used to carry).
    pub fn restore_spill(
        &mut self,
        seeded: &crate::restart::SeededSpill,
        now: Instant,
    ) -> Option<Instant> {
        let empty = self.spill.as_ref().is_none_or(|s| s.used() == 0);
        match (empty, spill::Spill::restore(seeded, now)) {
            (true, Some(filter)) => {
                self.spill = Some(filter);
                None
            }
            _ => seeded.slots().map(|(_, _, until)| until).max(),
        }
    }

    /// Restored records still cooling, candidates they turned away, and
    /// those that expired unsampled. Monitoring only (P5).
    pub fn restored_counts(&self) -> (usize, u64, u64) {
        (
            self.restored_live,
            self.restored_refused,
            self.restored_honoured,
        )
    }

    /// Queued candidates: the frontier and the held ones.
    pub fn frontier_len(&self) -> usize {
        self.frontier.len() + self.held.len()
    }
    pub fn known(&self) -> usize {
        self.states.len()
    }
    pub fn cooling(&self) -> usize {
        self.expiry.len() + self.short.len()
    }
    pub fn inflight(&self) -> usize {
        self.known() - self.cooling() - self.struck()
    }
    /// Cooling records by [`CoolingReason`], in [`CoolingReason::ALL`]'s
    /// order: they sum to [`cooling`](Self::cooling). Spilled cooldowns and
    /// [`struck`](Self::struck) records aren't cooling records.
    pub fn cooling_by_reason(&self) -> [usize; CoolingReason::ALL.len()] {
        debug_assert_eq!(self.cooling_by.iter().sum::<usize>(), self.cooling());
        self.cooling_by
    }
    /// The cooling records by reason, counted from scratch, for the tests
    /// that check [`cooling_by_reason`](Self::cooling_by_reason).
    #[cfg(test)]
    pub fn recount_cooling(&self) -> [usize; CoolingReason::ALL.len()] {
        let mut by = [0; CoolingReason::ALL.len()];
        for s in self.states.values() {
            if let State::Cooling { reason, .. } = s {
                by[*reason as usize] += 1;
            }
        }
        by
    }
    /// Remembered streaks whose short cooldown ended (escalation on only).
    pub fn struck(&self) -> usize {
        // Every short cooldown is also a streak (`cool`, `prune`).
        debug_assert!(self.short.len() <= self.struck.len());
        self.struck.len().saturating_sub(self.short.len())
    }
    /// Whether the sampler can't admit anyone: the table is full and,
    /// with the split on, no cooldown can move to the filter. With the
    /// split on, "likely room" is judged by the filter's fill; if all
    /// [`SPILL_TRIES`] inserts are refused below that fill (very rare),
    /// `next` admits no one that tick without this saying so. A held
    /// candidate (escalation on) needs no free place, so while any waits
    /// the sampler isn't saturated. If every held one lost its record
    /// meanwhile, `next` offers them again as newcomers, and on a full
    /// table admits no one that call.
    pub fn saturated(&self) -> bool {
        self.held.is_empty()
            && self.known() >= self.capacity
            && !(self.split
                && !(self.expiry.is_empty() && self.struck.is_empty())
                && self.spill.as_ref().is_none_or(spill::Spill::has_room))
    }

    /// The running total of time spent [`saturated`](Self::saturated)
    /// while `sampling` (the actor would send samples). Each call adds the
    /// time since the last one if the sampler is saturated now.
    pub fn full_time(&mut self, now: Instant, sampling: bool) -> Duration {
        if let Some(last) = self.full_checked
            && sampling
            && self.saturated()
        {
            self.full += now.saturating_duration_since(last);
        }
        self.full_checked = Some(now);
        self.full
    }

    /// Spill filter slots in use (0 with the switch off), for the gauge.
    pub fn spill_used(&self) -> usize {
        self.spill.as_ref().map_or(0, spill::Spill::used)
    }

    /// Spill slots in use and the filter's heap bytes (0, 0 with the
    /// switch off).
    #[cfg(test)]
    pub fn spill_counts(&self) -> (usize, usize) {
        self.spill.as_ref().map_or((0, 0), spill::Spill::counts)
    }
}

/// The table's bound for a new sampler: [`MAX_KNOWN`], or the simulator's
/// override on this thread (feature `testing` only).
fn initial_capacity() -> usize {
    #[cfg(feature = "testing")]
    if let Some(c) = TEST_CAPACITY.with(std::cell::Cell::get) {
        return c;
    }
    MAX_KNOWN
}

#[cfg(feature = "testing")]
thread_local! {
    static TEST_CAPACITY: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// The simulator's seam (feature `testing`, never in a production build):
/// samplers made on this thread while the guard lives hold at most
/// `capacity` records, so a run of a few thousand nodes can fill the
/// table. The simulator runs the node on its own current-thread runtime,
/// on the test's thread.
#[cfg(feature = "testing")]
#[derive(Debug)]
#[must_use = "the capacity holds only while the guard lives"]
pub struct TestCapacity(Option<usize>);

#[cfg(feature = "testing")]
impl TestCapacity {
    pub fn set(capacity: usize) -> TestCapacity {
        TestCapacity(TEST_CAPACITY.with(|c| c.replace(Some(capacity))))
    }
}

#[cfg(feature = "testing")]
impl Drop for TestCapacity {
    fn drop(&mut self) {
        TEST_CAPACITY.with(|c| c.set(self.0));
    }
}

/// A streak's `forget`, from its short cooldown's end: the failure plus the
/// full backoff, when the switch-off cooldown would have ended.
fn forget_of(until: Instant) -> Instant {
    until - FIRST_FAILURE_BACKOFF + MAX_INTERVAL
}

/// A share's count; zero when the key holds nothing.
fn count<K: Ord + std::hash::Hash>(map: &Map<K, u32>, key: K) -> u32 {
    map.get(&key).copied().unwrap_or(0)
}

/// Lowers a share's count, dropping the key at zero so the maps hold only
/// live IPs and blocks.
fn decrement<K: Ord + std::hash::Hash>(map: &mut Map<K, u32>, key: K) {
    let Some(n) = map.get_mut(&key) else {
        debug_assert!(false, "a share released that was never counted");
        return;
    };
    *n -= 1;
    if *n == 0 {
        map.remove(&key);
    }
}

/// X-sampler-cooldown-split's cooldown memory: a fixed table of 32-bit
/// slots, each a 20-bit tag of an address and the minute its cooldown
/// ends, so remembering who not to ask takes about 4 bytes a node instead
/// of a table place.
///
/// - **Never shorter.** A cooldown ending at `until` is stored as the
///   first whole minute at or after it, and [`Spill::contains`] answers
///   yes until that minute. There are no false negatives: a node in
///   cooldown is always refused. A false positive (another address with the
///   same tag in one of its blocks) only skips a node.
/// - **Shares still hold.** An address's slots sit in one of two blocks of
///   64 chosen by its widest share (its IPv4 /24 or IPv6 /48), and its tag
///   is built from its block, its IPv6 /64, its IP and the address. So
///   [`Spill::shares`] counts, from the two blocks alone, how many moved
///   cooldowns each of its IP, block and /48 still holds, and the sampler's
///   per-source shares count them until they end. Every stored cooldown has
///   its own slot (none is merged into another), so a count is never low;
///   tag collisions only make it high, which only skips a node.
/// - **Bounded.** [`SLOTS`] slots, 32 MiB, allocated on the first spill.
///   When both blocks are full of live cooldowns, [`Spill::insert`]
///   refuses and the cooldown stays in the sampler's table: the sampler is
///   then no worse than with the switch off.
/// - **Hostile input.** Keys are hashed with a per-process random key
///   (SipHash), so remote nodes can't aim at one block, and only nodes this
///   node sampled are ever inserted, at the sample rate.
///
/// Minutes are kept modulo 4096 (68 h). A live end is at most
/// [`HORIZON_MIN`] (48 h and a minute) ahead, so an ended slot that isn't
/// cleared reads as live again about 1,215 minutes (20 h) after it ends.
/// [`Spill::sweep`] clears ended slots in a pass every 17 minutes, long
/// before that; and if one ever did read as live, it would only skip a
/// node.
pub(crate) mod spill {
    use std::hash::Hasher as _;
    use std::net::{IpAddr, SocketAddr};
    use std::time::Duration;

    use tokio::time::Instant;

    use super::super::limits::{Network, allocation, network};
    use crate::restart::{SPILL_BLOCK, SPILL_BLOCKS, SPILL_MINUTE_BITS, SeededSpill, Spilled};

    const MINUTE_NANOS: u128 = 60_000_000_000;
    const MINUTE_BITS: u32 = SPILL_MINUTE_BITS;
    const MINUTE_MASK: u32 = (1 << MINUTE_BITS) - 1;
    /// The tag, above the minute: 8 bits of the block's key, 4 of the
    /// IPv6 /64 (0 on IPv4), 4 (IPv6) or 8 (IPv4) of the IP, and 4 of the
    /// address. A used slot's tag is never all zero.
    const KEY_MASK: u32 = 0xFF << 24;
    const NET_MASK: u32 = 0xFFF << 20;
    const IP_MASK: u32 = 0xFFFF << 16;
    const TAG_MASK: u32 = !MINUTE_MASK;
    /// The furthest a stored end may lie ahead of the current minute: the
    /// longest cooldown (`MAX_INTERVAL` × `MAX_SCALE`, 48 h) and a minute.
    pub const HORIZON_MIN: u32 = 48 * 60 + 1;
    /// A block holds the widest share's cap (64) twice over. The restart
    /// snapshot keeps the filter in this shape (`restart::Spilled`).
    const BLOCK: usize = SPILL_BLOCK;
    const BLOCKS: usize = SPILL_BLOCKS;
    /// 2^23 slots of 4 bytes: 32 MiB. Balanced (100 samples a second)
    /// keeps about 2.2 million cooldowns live at WK-10e scale 1 and about
    /// 5.2 million at scale 8 (a fifth answered, cooling 48 h); the usable
    /// 15/16 of this holds either.
    pub const SLOTS: usize = BLOCKS * BLOCK;
    /// Slots cleared of ended cooldowns per [`Spill::sweep`] (8,192): a
    /// full pass every 1,024 sweeps, about 17 minutes at the 1 s tick.
    pub const SWEEP: usize = SLOTS / 1024;
    /// The filter's heap bytes once allocated: the slots and nothing else.
    #[cfg(test)]
    pub const BYTES: usize = SLOTS * 4;

    pub struct Spill {
        /// SipHash-2-4 keys: per process, or restored with the filter, so
        /// a restarted run finds the fingerprints a snapshot kept.
        keys: [u64; 2],
        /// Minute `m` ends at `base + m` minutes.
        base: Instant,
        /// Empty until the first insert.
        slots: Box<[u32]>,
        /// Slots holding a tag, ended or not.
        used: usize,
        cursor: usize,
    }

    impl std::fmt::Debug for Spill {
        // The keys stay out of logs: they decide where an address lands.
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Spill")
                .field("base", &self.base)
                .field("used", &self.used)
                .field("cursor", &self.cursor)
                .finish_non_exhaustive()
        }
    }

    /// Fresh keys: random, or fixed under `testing` so the simulator's runs
    /// repeat (as the seen filter's are).
    fn fresh_keys() -> [u64; 2] {
        #[cfg(not(feature = "testing"))]
        {
            [rand::random(), rand::random()]
        }
        #[cfg(feature = "testing")]
        {
            [0x5370_696c_6c4b_6579, 0x6c6f_6465_7374_6172]
        }
    }

    /// SipHash-2-4 over `bytes`. std's `SipHasher` is deprecated only in
    /// favour of `DefaultHasher`, whose algorithm may change between
    /// releases; a restored filter needs the same hash in the next binary,
    /// so this one is pinned (`siphash_is_the_reference_function`).
    #[allow(deprecated)]
    pub(super) fn siphash(keys: [u64; 2], bytes: &[u8]) -> u64 {
        let mut h = std::hash::SipHasher::new_with_keys(keys[0], keys[1]);
        h.write(bytes);
        h.finish()
    }

    /// The bytes hashed for each level: a domain byte, then the value in a
    /// fixed byte order, so the hash doesn't hang on std's `Hash` impls.
    fn net_bytes(domain: u8, n: Network) -> [u8; 10] {
        let mut out = [0; 10];
        out[0] = domain;
        out[1] = n.0;
        out[2..].copy_from_slice(&n.1.to_be_bytes());
        out
    }

    fn ip_bytes(domain: u8, ip: IpAddr, port: Option<u16>) -> ([u8; 20], usize) {
        let mut out = [0; 20];
        out[0] = domain;
        let mut n = match ip {
            IpAddr::V4(v4) => {
                out[1] = 4;
                out[2..6].copy_from_slice(&v4.octets());
                6
            }
            IpAddr::V6(v6) => {
                out[1] = 6;
                out[2..18].copy_from_slice(&v6.octets());
                18
            }
        };
        if let Some(p) = port {
            out[n..n + 2].copy_from_slice(&p.to_be_bytes());
            n += 2;
        }
        (out, n)
    }

    /// Where an address's slots live, and its tag (above the minute).
    struct Place {
        blocks: [usize; 2],
        tag: u32,
        v4: bool,
    }

    impl Spill {
        pub fn new(base: Instant) -> Spill {
            Spill {
                keys: fresh_keys(),
                base,
                slots: Box::default(),
                used: 0,
                cursor: 0,
            }
        }

        fn hash(&self, bytes: &[u8]) -> u64 {
            siphash(self.keys, bytes)
        }

        fn since_base(&self, t: Instant) -> u128 {
            t.saturating_duration_since(self.base).as_nanos()
        }

        /// The first minute still ahead at `now` (a cooldown stored as
        /// this minute or later hasn't ended).
        fn first_live(&self, now: Instant) -> u64 {
            u64::try_from(self.since_base(now) / MINUTE_NANOS)
                .unwrap_or(u64::MAX)
                .saturating_add(1)
        }

        /// Whether a slot's minute is still ahead of `first`.
        fn live(slot: u32, first: u64) -> bool {
            slot != 0 && (slot.wrapping_sub(first as u32) & MINUTE_MASK) < HORIZON_MIN
        }

        fn place(&self, addr: &SocketAddr) -> Place {
            let ip = addr.ip().to_canonical();
            let net = network(ip);
            let widest = allocation(ip).unwrap_or(net);
            // One keyed hash per level, each in its own domain.
            let hk = self.hash(&net_bytes(0, widest));
            let (b, n) = ip_bytes(1, ip, None);
            let hi = self.hash(&b[..n]);
            let (b, n) = ip_bytes(3, ip, Some(addr.port()));
            let ha = self.hash(&b[..n]);
            let v4 = matches!(ip, IpAddr::V4(_));
            let mid = if v4 {
                (hi & 0xFF) as u32
            } else {
                let hn = self.hash(&net_bytes(2, net));
                ((hn & 0xF) << 4 | (hi & 0xF)) as u32
            };
            // The key's byte is never 0, so a used slot is never 0.
            let key = ((hk >> 56) as u32).max(1);
            Place {
                blocks: [(hk as usize) % BLOCKS, ((hk >> 17) as usize) % BLOCKS],
                tag: key << 24 | mid << 16 | ((ha & 0xF) as u32) << MINUTE_BITS,
                v4,
            }
        }

        fn block(&self, b: usize) -> &[u32] {
            &self.slots[b * BLOCK..(b + 1) * BLOCK]
        }

        /// Remembers that `addr` cools until `until`, in a slot of its own.
        /// False, changing nothing, when both its blocks are full of live
        /// cooldowns or `until` is past the horizon; true without storing
        /// anything when the cooldown has already ended.
        pub fn insert(&mut self, addr: &SocketAddr, until: Instant, now: Instant) -> bool {
            debug_assert!(now >= self.base, "the filter's clock starts at its base");
            if until <= now {
                return true;
            }
            let first = self.first_live(now);
            let end =
                u64::try_from(self.since_base(until).div_ceil(MINUTE_NANOS)).unwrap_or(u64::MAX);
            // `end >= first` whenever `until > now >= base`; saturate anyway.
            let ahead = end.saturating_sub(first);
            if ahead >= u64::from(HORIZON_MIN) {
                return false;
            }
            if self.slots.is_empty() {
                self.slots = vec![0; SLOTS].into_boxed_slice();
            }
            let place = self.place(addr);
            // Per block: its first free slot and how many are free.
            let mut free = [(0, 0); 2];
            for (k, b) in place.blocks.into_iter().enumerate() {
                for (j, slot) in self.block(b).iter().enumerate() {
                    if !Spill::live(*slot, first) {
                        if free[k].1 == 0 {
                            free[k].0 = b * BLOCK + j;
                        }
                        free[k].1 += 1;
                    }
                }
            }
            // The emptier block (two choices keep the load even).
            let (i, n) = if free[1].1 > free[0].1 {
                free[1]
            } else {
                free[0]
            };
            if n == 0 {
                return false;
            }
            if self.slots[i] == 0 {
                self.used += 1;
            }
            self.slots[i] = place.tag | (end as u32 & MINUTE_MASK);
            true
        }

        /// Live slots in `addr`'s blocks whose tag matches its own under
        /// `mask`.
        fn count(&self, place: &Place, mask: u32, first: u64) -> u32 {
            let want = place.tag & mask;
            let mut n = 0;
            for b in place.blocks {
                n += self
                    .block(b)
                    .iter()
                    .filter(|s| *s & mask == want && Spill::live(**s, first))
                    .count() as u32;
            }
            n
        }

        /// Whether `addr` may still be cooling at `now`: never false while
        /// a cooldown stored for it hasn't ended.
        pub fn contains(&self, addr: &SocketAddr, now: Instant) -> bool {
            self.used != 0 && self.count(&self.place(addr), TAG_MASK, self.first_live(now)) > 0
        }

        /// Live moved cooldowns that count against `addr`'s IP, its block
        /// (IPv4 /24 or IPv6 /64) and its IPv6 /48 (0 on IPv4): never fewer
        /// than truly held, sometimes more.
        pub fn shares(&self, addr: &SocketAddr, now: Instant) -> (u32, u32, u32) {
            if self.used == 0 {
                return (0, 0, 0);
            }
            let first = self.first_live(now);
            let place = self.place(addr);
            let ip = self.count(&place, IP_MASK, first);
            if place.v4 {
                (ip, self.count(&place, KEY_MASK, first), 0)
            } else {
                (
                    ip,
                    self.count(&place, NET_MASK, first),
                    self.count(&place, KEY_MASK, first),
                )
            }
        }

        /// Clears ended slots, [`SWEEP`] of them from where the last
        /// sweep stopped.
        pub fn sweep(&mut self, now: Instant) {
            if self.used == 0 {
                return;
            }
            let first = self.first_live(now);
            for _ in 0..SWEEP {
                let slot = &mut self.slots[self.cursor];
                if *slot != 0 && !Spill::live(*slot, first) {
                    *slot = 0;
                    self.used -= 1;
                }
                self.cursor = (self.cursor + 1) % SLOTS;
            }
        }

        /// The filter for the restart snapshot (X-split-resume-gate): every
        /// slot live at `mono`, in its block, with the whole minutes left
        /// until its end, rounded up and capped at `horizon` as the listed
        /// entries are (WK-10e). `None` when no slot is live.
        pub fn export(&self, mono: Instant, horizon: Duration) -> Option<Spilled> {
            if self.used == 0 {
                return None;
            }
            let first = self.first_live(mono);
            let cap = mono + horizon;
            let mut raw = Vec::with_capacity(BLOCKS + 4 * self.used);
            let mut any = false;
            for b in 0..BLOCKS {
                let count_at = raw.len();
                raw.push(0u8);
                let mut n = 0u8;
                for slot in self.block(b) {
                    if !Spill::live(*slot, first) {
                        continue;
                    }
                    // The slot's minute, unwrapped: live means it's within
                    // `HORIZON_MIN` of `first`.
                    let minute = first + u64::from(slot.wrapping_sub(first as u32) & MINUTE_MASK);
                    let end = (self.base + Duration::from_secs(minute.saturating_mul(60))).min(cap);
                    let left = end.saturating_duration_since(mono).as_nanos();
                    // Live means its minute ends after `mono`; `cap` is
                    // after it too, so at least one minute is written.
                    let minutes = u32::try_from(left.div_ceil(MINUTE_NANOS))
                        .unwrap_or(u32::MAX)
                        .max(1);
                    debug_assert!(minutes <= MINUTE_MASK);
                    raw.extend_from_slice(&((slot & TAG_MASK) | minutes).to_be_bytes());
                    n += 1;
                }
                raw[count_at] = n;
                any |= n > 0;
            }
            let mut key = [0; 16];
            key[..8].copy_from_slice(&self.keys[0].to_le_bytes());
            key[8..].copy_from_slice(&self.keys[1].to_le_bytes());
            any.then_some(Spilled { key, raw })
        }

        /// A filter rebuilt from a restart snapshot's (X-split-resume-gate),
        /// with its keys, its clock starting at `now`. Each slot keeps its
        /// block and tag and ends at the first whole minute at or after its
        /// seeded end, so it's never shorter. `None` changes nothing: a
        /// block held more than it can, which a checked snapshot can't.
        pub fn restore(seeded: &SeededSpill, now: Instant) -> Option<Spill> {
            let key = seeded.key();
            let keys = [
                u64::from_le_bytes(key[..8].try_into().expect("8 bytes")),
                u64::from_le_bytes(key[8..].try_into().expect("8 bytes")),
            ];
            let mut spill = Spill {
                keys,
                base: now,
                slots: Box::default(),
                used: 0,
                cursor: 0,
            };
            for (b, tag, until) in seeded.slots() {
                if until <= now {
                    continue;
                }
                let end = u64::try_from(spill.since_base(until).div_ceil(MINUTE_NANOS))
                    .unwrap_or(u64::MAX);
                // Seeded ends are within the restart horizon (hours), far
                // inside the filter's 48 h.
                if end >= u64::from(HORIZON_MIN) || tag & MINUTE_MASK != 0 || tag == 0 {
                    return None;
                }
                if spill.slots.is_empty() {
                    spill.slots = vec![0; SLOTS].into_boxed_slice();
                }
                let i = (b * BLOCK..(b + 1) * BLOCK).find(|i| spill.slots[*i] == 0)?;
                spill.slots[i] = tag | end as u32;
                spill.used += 1;
            }
            Some(spill)
        }

        /// Whether there's likely room: below 15/16 of the slots used. An
        /// insert can still refuse (both its blocks full).
        pub fn has_room(&self) -> bool {
            self.used < SLOTS / 16 * 15
        }

        /// Slots in use, ended or not (until swept).
        pub fn used(&self) -> usize {
            self.used
        }

        /// Slots in use, ended or not, and heap bytes held.
        #[cfg(test)]
        pub fn counts(&self) -> (usize, usize) {
            (self.used, self.slots.len() * 4)
        }
    }
}

#[cfg(test)]
mod escalate_tests;
#[cfg(test)]
mod split_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact::NodeId;

    fn n(i: u32) -> NodeInfo {
        NodeInfo {
            id: NodeId([1; 20]),
            addr: SocketAddr::from((i.to_be_bytes(), 6881)),
        }
    }

    #[test]
    fn reserve_complete_duplicate_and_late_reply() {
        let mut s = Sampler::default();
        let now = Instant::now();
        s.offer(n(1), now);
        assert_eq!(s.next(now), Some(n(1)));
        assert!(s.reserve(n(1).addr, 1, now));
        s.offer(n(1), now);
        assert!(s.next(now).is_none());
        assert!(!s.reserve(n(1).addr, 2, now));
        assert!(!s.complete(n(1).addr, 2, MIN_INTERVAL, now));
        assert!(s.complete(n(1).addr, 1, Duration::ZERO, now));
        assert!(!s.complete(n(1).addr, 1, MAX_INTERVAL, now));
        assert!(!s.reserve(n(1).addr, 2, now + Duration::from_secs(59)));
        assert!(s.reserve(n(1).addr, 2, now + MIN_INTERVAL));
        assert!(!s.abort(n(1).addr, 1));
        assert!(s.abort(n(1).addr, 2));
        assert_eq!((s.known(), s.cooling()), (0, 0));
    }

    #[test]
    fn saturation_preserves_frontier_and_reclaims_in_bounded_steps() {
        // One address per /24, so the blocks' shares don't bind first.
        let wide = |i: u32| n(i << 8);
        let mut s = Sampler::default();
        let now = Instant::now();
        for i in 0..MAX_KNOWN as u32 {
            assert!(s.reserve(wide(i).addr, u64::from(i), now));
            assert!(s.complete(wide(i).addr, u64::from(i), MAX_INTERVAL, now));
        }
        s.offer(wide(MAX_KNOWN as u32), now);
        assert!(s.next(now).is_none());
        assert!(!s.reserve(wide(MAX_KNOWN as u32).addr, 999999, now));
        assert_eq!(
            (s.known(), s.cooling(), s.frontier_len()),
            (MAX_KNOWN, MAX_KNOWN, 1)
        );
        s.prune(now + MAX_INTERVAL);
        assert_eq!(s.known(), MAX_KNOWN - RECLAIM_PER_TICK);
        assert_eq!(s.next(now + MAX_INTERVAL), Some(wide(MAX_KNOWN as u32)));
    }

    #[test]
    fn interval_bounds_and_no_stale_expiries() {
        for wait in [0, 60, 900, 1800, 21600, 21601] {
            let mut s = Sampler::default();
            let now = Instant::now();
            assert!(s.reserve(n(1).addr, 1, now));
            assert!(s.complete(n(1).addr, 1, Duration::from_secs(wait), now));
            let due = now + Duration::from_secs(wait.clamp(60, 21600));
            s.prune(due - Duration::from_nanos(1));
            assert_eq!(s.known(), 1);
            s.prune(due);
            assert_eq!(s.known(), 0);
            assert!(s.reserve(n(1).addr, 2, due));
            s.prune(due + MAX_INTERVAL);
            assert_eq!(s.inflight(), 1);
        }
    }

    #[test]
    fn repeated_reuse_and_stale_completion_cannot_shorten_new_cooldown() {
        let mut s = Sampler::with_capacity(1);
        let start = Instant::now();
        let addr = n(1).addr;
        for token in 1..=100u64 {
            let now = start + MAX_INTERVAL * (token - 1) as u32;
            // Reserve directly at expiry, before the maintenance prune. This
            // must retire the old expiry index without freeing the new owner.
            assert!(s.reserve(addr, token, now));
            assert!(!s.complete(addr, token - 1, MIN_INTERVAL, now));
            assert!(!s.abort(addr, token - 1));
            s.prune(now);
            assert_eq!((s.known(), s.inflight(), s.cooling()), (1, 1, 0));
            assert!(s.complete(addr, token, MAX_INTERVAL, now));
            s.prune(now + MIN_INTERVAL);
            assert_eq!((s.known(), s.inflight(), s.cooling()), (1, 0, 1));
            assert!(!s.reserve(addr, token + 1, now + MIN_INTERVAL));
        }
        s.prune(start + MAX_INTERVAL * 100);
        assert_eq!((s.known(), s.inflight(), s.cooling()), (0, 0, 0));
    }

    /// WK-10e: a scaled cooldown is the node's bounded interval times the
    /// scale, which is taken as 1..=MAX_SCALE: at most 48 h.
    #[test]
    fn a_scaled_cooldown_is_the_interval_times_the_scale() {
        for (wait, scale, want) in [
            (3600, 1, 3600),
            (3600, 3, 3 * 3600),
            (21_600, 8, 8 * 21_600),
            // The node's own interval is bounded first.
            (30, 2, 120),
            (99_999, 8, 8 * 21_600),
            // 0 reads as 1; past MAX_SCALE reads as MAX_SCALE.
            (3600, 0, 3600),
            (21_600, 200, 8 * 21_600),
        ] {
            let mut s = Sampler::default();
            let now = Instant::now();
            assert!(s.reserve(n(1).addr, 1, now));
            assert!(s.complete_scaled(n(1).addr, 1, Duration::from_secs(wait), scale, now));
            let due = now + Duration::from_secs(want);
            assert!(
                !s.reserve(n(1).addr, 2, due - Duration::from_nanos(1)),
                "{wait} × {scale}"
            );
            assert!(s.reserve(n(1).addr, 2, due), "{wait} × {scale}");
        }
        assert_eq!(
            MAX_INTERVAL * u32::from(MAX_SCALE),
            Duration::from_secs(48 * 3600)
        );
    }

    /// WK-10e: with a full table of 48 h cooldowns, a new node waits on the
    /// frontier, isn't lost, and is sampled as soon as a cooldown ends.
    #[test]
    fn a_full_table_of_scaled_cooldowns_holds_new_nodes_until_one_ends() {
        let mut s = Sampler::with_capacity(4);
        let now = Instant::now();
        for i in 1..=4 {
            assert!(s.reserve(n(i).addr, u64::from(i), now));
            assert!(s.complete_scaled(n(i).addr, u64::from(i), MAX_INTERVAL, MAX_SCALE, now));
        }
        assert!(s.saturated());
        assert!(s.offer(n(9), now));
        assert_eq!(s.next(now + MAX_INTERVAL), None, "full: it waits");
        assert_eq!(s.frontier_len(), 1, "still queued");
        let end = now + MAX_INTERVAL * u32::from(MAX_SCALE);
        s.prune(end);
        assert!(!s.saturated());
        assert_eq!(s.next(end), Some(n(9)));
    }

    /// WK-10e: the snapshot lists a scaled cooldown as at most
    /// `MAX_INTERVAL` from now; the live one keeps its full length, and an
    /// unscaled or sealed record its own end.
    #[test]
    fn a_snapshot_bounds_only_scaled_cooldowns() {
        let mut s = Sampler::default();
        let now = Instant::now();
        assert!(s.reserve(n(1).addr, 1, now));
        assert!(s.complete_scaled(n(1).addr, 1, MAX_INTERVAL, MAX_SCALE, now));
        // Wire ids start anywhere below bit 63, bits 61 and 62 included.
        let high = (1 << 62) | (1 << 61) | 2;
        assert!(s.reserve(n(2).addr, high, now));
        assert!(s.complete_scaled(n(2).addr, high, Duration::from_secs(600), 4, now));
        assert!(s.reserve(n(4).addr, 4, now));
        assert!(s.complete(n(4).addr, 4, Duration::from_secs(900), now));
        assert!(s.reserve(n(3).addr, 3, now));
        let sealed = now + MAX_INTERVAL + Duration::from_secs(4);
        s.seal_in_flight(sealed);
        let listed: std::collections::HashMap<SocketAddr, Instant> = s.restrictions(now).collect();
        assert_eq!(listed[&n(1).addr], now + MAX_INTERVAL, "bounded");
        assert_eq!(listed[&n(2).addr], now + Duration::from_secs(2400));
        assert_eq!(listed[&n(4).addr], now + Duration::from_secs(900));
        assert_eq!(listed[&n(3).addr], sealed, "a seal keeps the horizon");
        // Live, the scaled one still cools for 48 h.
        let end = now + MAX_INTERVAL * u32::from(MAX_SCALE);
        assert!(!s.reserve(n(1).addr, 9, end - Duration::from_nanos(1)));
        assert!(s.reserve(n(1).addr, 9, end));
    }

    /// A sample's token is always a wire id below the proof's bit; one at
    /// it is a bug the snapshot would misread as a seal (debug builds).
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "a wire token below PROOF_TOKEN")]
    fn a_proof_bit_on_a_samples_token_is_refused() {
        let mut s = Sampler::default();
        let now = Instant::now();
        let _ = s.complete_scaled(n(1).addr, PROOF_TOKEN | 1, MAX_INTERVAL, 1, now);
    }

    type Counts = (Map<IpAddr, u32>, Map<Network, u32>, Map<Network, u32>);

    /// The shares, recomputed from scratch.
    fn recounted(s: &Sampler) -> Counts {
        let mut tracked: Set<SocketAddr> = s.queued.clone();
        tracked.extend(s.states.keys().copied());
        let (mut ips, mut nets, mut allocs) = (Map::default(), Map::default(), Map::default());
        for a in tracked {
            let ip = a.ip().to_canonical();
            *ips.entry(ip).or_default() += 1;
            *nets.entry(network(ip)).or_default() += 1;
            if let Some(al) = allocation(ip) {
                *allocs.entry(al).or_default() += 1;
            }
        }
        (ips, nets, allocs)
    }

    fn node(addr: SocketAddr) -> NodeInfo {
        NodeInfo {
            id: NodeId([1; 20]),
            addr,
        }
    }

    /// Offers every address, sampling half of what's queued as it goes so
    /// the table fills as well as the frontier, and returns how many of
    /// them the sampler holds at the end.
    fn push(s: &mut Sampler, addrs: impl Iterator<Item = SocketAddr>, now: Instant) -> usize {
        let mut token = 0;
        let mine: Vec<SocketAddr> = addrs.collect();
        for (i, a) in mine.iter().enumerate() {
            s.offer(node(*a), now);
            if i % 2 == 1
                && let Some(n) = s.next(now)
            {
                token += 1;
                if s.reserve(n.addr, token, now) {
                    s.complete(n.addr, token, MAX_INTERVAL, now);
                }
            }
        }
        mine.iter().filter(|a| s.tracked(a)).count()
    }

    #[test]
    fn one_ip_holds_at_most_its_share() {
        for ip in ["10.0.0.1", "2001:db8::1"] {
            let ip: IpAddr = ip.parse().unwrap();
            let mut s = Sampler::default();
            let held = push(
                &mut s,
                (1..=2000).map(|p| SocketAddr::new(ip, p)),
                Instant::now(),
            );
            assert_eq!(held, MAX_PER_IP as usize, "{ip}");
        }
    }

    #[test]
    fn one_v4_block_holds_at_most_its_share() {
        let mut s = Sampler::default();
        let addrs =
            (0..=255u8).flat_map(|h| (1..=8u16).map(move |p| SocketAddr::from(([10, 0, 0, h], p))));
        assert_eq!(
            push(&mut s, addrs, Instant::now()),
            MAX_PER_V4_NETWORK as usize
        );
        // Another block isn't affected.
        assert!(s.offer(node(SocketAddr::from(([10, 0, 1, 1], 1))), Instant::now()));
    }

    #[test]
    fn one_v6_block_holds_at_most_its_share() {
        let mut s = Sampler::default();
        let addrs =
            (1..=5000u16).map(|h| SocketAddr::from(([0x2001, 0xdb8, 0, 1, 0, 0, h, h], 6881)));
        assert_eq!(
            push(&mut s, addrs, Instant::now()),
            MAX_PER_V6_NETWORK as usize
        );
    }

    #[test]
    fn one_v6_allocation_holds_at_most_its_share_across_its_blocks() {
        let mut s = Sampler::default();
        // Many /64s of one /48, a few hosts each.
        let addrs = (0..=4000u16).flat_map(|net| {
            (1..=4u16).map(move |h| SocketAddr::from(([0x2001, 0xdb8, 7, net, 0, 0, 0, h], 6881)))
        });
        assert_eq!(
            push(&mut s, addrs, Instant::now()),
            MAX_PER_V6_ALLOCATION as usize
        );
        // Another /48 isn't affected.
        assert!(s.offer(
            node(SocketAddr::from(([0x2001, 0xdb8, 8, 0, 0, 0, 0, 1], 6881))),
            Instant::now()
        ));
    }

    #[test]
    fn a_place_comes_back_when_its_record_leaves() {
        let mut s = Sampler::default();
        let now = Instant::now();
        let at = |h: u8, p| SocketAddr::from(([10, 0, 0, h], p));
        for h in 1..=255u8 {
            s.offer(node(at(h, 1)), now);
        }
        assert!(!s.offer(node(at(1, 99)), now));
        // Sampling one frees nothing while it's in flight or cooling...
        let first = s.next(now).unwrap();
        assert!(s.reserve(first.addr, 1, now));
        assert!(!s.offer(node(at(1, 99)), now));
        // ...and an expired cooldown gives its place back.
        assert!(s.complete(first.addr, 1, MIN_INTERVAL, now));
        s.prune(now + MIN_INTERVAL);
        assert!(s.offer(node(at(1, 99)), now + MIN_INTERVAL));
    }

    #[derive(Debug, Clone)]
    enum Op {
        Offer(SocketAddr),
        Next,
        Complete(u8),
        Fail(u8),
        Abort(u8),
        Restore(SocketAddr),
        Advance(u16),
    }

    /// Addresses crowded into a few IPv4 /24s, IPv6 /64s and /48s.
    fn arb_addr() -> impl Strategy<Value = SocketAddr> {
        prop_oneof![
            (0..3u8, any::<u8>(), 1..20u16)
                .prop_map(|(b, h, p)| SocketAddr::from(([10, 0, b, h], p))),
            (0..2u16, 0..40u16, 0..20u16, 1..4u16)
                .prop_map(|(a, n, h, p)| SocketAddr::from(([0x2001, 0xdb8, a, n, 0, 0, 0, h], p))),
        ]
    }

    fn arb_op() -> impl Strategy<Value = Op> {
        prop_oneof![
            6 => arb_addr().prop_map(Op::Offer),
            3 => Just(Op::Next),
            1 => any::<u8>().prop_map(Op::Complete),
            1 => any::<u8>().prop_map(Op::Fail),
            1 => any::<u8>().prop_map(Op::Abort),
            1 => arb_addr().prop_map(Op::Restore),
            1 => (0..400u16).prop_map(Op::Advance),
        ]
    }

    use proptest::prelude::*;

    proptest! {
        /// Whatever the sequence, the share counts match the frontier and
        /// table, and outside restored records no IP, block or IPv6
        /// allocation holds more than its share. With escalation on, the
        /// held queue too (X-escalate-full-table), a full table included.
        #[test]
        fn shares_hold_under_any_sequence(
            escalate in any::<bool>(),
            small in any::<bool>(),
            split in any::<bool>(),
            ops in proptest::collection::vec(arb_op(), 1..600),
        ) {
            let capacity = if small { 8 } else { MAX_KNOWN };
            let mut s = if split {
                Sampler::split_with_capacity(capacity)
            } else {
                Sampler::with_capacity(capacity)
            }
            .with_escalation(escalate);
            let mut now = Instant::now();
            let mut token = 0u64;
            let mut flying: Vec<(SocketAddr, u64)> = vec![];
            let mut restored = false;
            for op in ops {
                match op {
                    Op::Offer(a) => {
                        s.offer(node(a), now);
                    }
                    Op::Next => {
                        if let Some(n) = s.next(now) {
                            token += 1;
                            if s.reserve(n.addr, token, now) {
                                flying.push((n.addr, token));
                            }
                        }
                    }
                    Op::Complete(i) if !flying.is_empty() => {
                        let (a, t) = flying.remove(usize::from(i) % flying.len());
                        s.complete(a, t, MIN_INTERVAL, now);
                    }
                    Op::Fail(i) if !flying.is_empty() => {
                        let (a, t) = flying.remove(usize::from(i) % flying.len());
                        s.fail(a, t, MAX_INTERVAL, now);
                    }
                    Op::Abort(i) if !flying.is_empty() => {
                        let (a, t) = flying.remove(usize::from(i) % flying.len());
                        s.abort(a, t);
                    }
                    Op::Restore(a) if flying.is_empty() => {
                        restored = true;
                        s.restore(a, now + MIN_INTERVAL);
                    }
                    Op::Advance(secs) => {
                        now += Duration::from_secs(u64::from(secs));
                        s.prune(now);
                    }
                    _ => {}
                }
                // Every queued address is in the frontier or held, once.
                let queued: Set<SocketAddr> =
                    s.frontier.iter().chain(&s.held).map(|n| n.addr).collect();
                prop_assert_eq!(queued.len(), s.frontier.len() + s.held.len());
                prop_assert_eq!(&queued, &s.queued);
                if !escalate {
                    prop_assert!(s.held.is_empty());
                }
                if !restored {
                    prop_assert!(s.known() <= capacity);
                }
                let (ips, nets, allocs) = recounted(&s);
                prop_assert_eq!(&s.per_ip, &ips);
                prop_assert_eq!(&s.per_network, &nets);
                prop_assert_eq!(&s.per_allocation, &allocs);
                if !restored {
                    prop_assert!(ips.values().all(|n| *n <= MAX_PER_IP));
                    let cap = |k: &Network| if k.0 == 4 { MAX_PER_V4_NETWORK } else { MAX_PER_V6_NETWORK };
                    prop_assert!(nets.iter().all(|(k, n)| *n <= cap(k)));
                    prop_assert!(allocs.values().all(|n| *n <= MAX_PER_V6_ALLOCATION));
                }
            }
        }
    }
}
