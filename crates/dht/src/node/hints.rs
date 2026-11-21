//! CR-1a-H: responder hints (`dht.responder_hints`,
//! docs/decisions/2026-10-30-CR-1a-H.md). A BEP 51 responder that listed
//! a hash we hadn't seen is remembered for [`HINT_TTL`], so the lookup the
//! pool later asks for can send it one direct `get_peers` before ordinary
//! routing. The responder is a DHT UDP endpoint, never a peer: a hint only
//! seeds a query, never the fetcher, and never counts as routing knowledge.
//!
//! Two bounded tables, both in memory only:
//! - hints: at most one per address family per hash, at most
//!   [`HINT_CAPACITY`] hashes, dropped once used or [`HINT_TTL`] old;
//! - the per-IP guard: at most [`GUARD_CAPACITY`] canonical IPs, each with
//!   one hinted query in flight and [`HINT_NOT_BEFORE`] between hinted
//!   sends. Ordinary routing honours a retained restriction too, so the
//!   fallback can't bypass it.
//!
//! CR-HINT-FIRST (`dht.hint_first`, docs/decisions/2026-11-21-CR-HINT-FIRST.md)
//! adds a third: per canonical IP, the one hash marked for the pool's T1
//! tier whose hint is still outstanding, at most [`T1_RESPONDERS`] IPs.

use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tokio::time::Instant;

use lodestar_metainfo::InfoHash;

use super::transport::Map;
use super::{Config, HintFallback, HintFirstSkip, HintReject};

// Candidate defaults (the decisions file): reviewed constants, not config.
/// A hint is fresh while its age is below this.
pub(crate) const HINT_TTL: Duration = Duration::from_secs(60);
/// Between two permitted hinted sends to one IP.
pub(crate) const HINT_NOT_BEFORE: Duration = Duration::from_secs(60);
/// Canonical IPs the guard holds restrictions for.
pub(crate) const GUARD_CAPACITY: usize = 20_000;
/// Hinted queries per family leg of one logical lookup.
pub(crate) const HINTED_ATTEMPTS_PER_FAMILY: usize = 1;
/// Hashes holding hints. Not a spec value: the pool's pending keys bound
/// the spec's "per pending key", but the actor can't see them, so this
/// caps the table directly (about 3 MB full).
pub(crate) const HINT_CAPACITY: usize = 20_000;
/// CR-HINT-FIRST: canonical IPs with an outstanding T1 hint (marked, not
/// yet taken, younger than [`HINT_TTL`]). A candidate default.
pub(crate) const T1_RESPONDERS: usize = 20_000;
/// Capture-order entries, live or stale, held at any moment: used hints
/// leave stale entries, which [`Hints::capture`] compacts away past this.
const ORDER_CAPACITY: usize = 4 * HINT_CAPACITY;

/// The slot a normalized endpoint's family uses.
fn slot(addr: SocketAddr) -> usize {
    usize::from(addr.is_ipv6())
}

/// The guard's key: one IP across ports, IPv4-mapped IPv6 folded to IPv4.
pub(crate) fn canonical(ip: IpAddr) -> IpAddr {
    ip.to_canonical()
}

/// The public-endpoint checks, at capture and again at send. A scoped IPv6
/// address or port 0 is an unsupported encoding; private, reserved or
/// blocklisted addresses are refused by policy.
pub(crate) fn check(addr: SocketAddr, cfg: &Config) -> Result<SocketAddr, HintReject> {
    if matches!(addr, SocketAddr::V6(v6) if v6.scope_id() != 0) || addr.port() == 0 {
        return Err(HintReject::Invalid);
    }
    let addr = lodestar_network_policy::normalize(addr);
    if !lodestar_network_policy::allows(addr, cfg.allow_private)
        || cfg.deny.as_ref().is_some_and(|d| d.denies(addr.ip()))
    {
        return Err(HintReject::Private);
    }
    Ok(addr)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Hint {
    pub addr: SocketAddr,
    /// When the sample reply arrived, on our monotonic clock.
    pub at: Instant,
}

impl Hint {
    pub fn fresh(&self, now: Instant) -> bool {
        now.duration_since(self.at) < HINT_TTL
    }
}

#[derive(Debug, Clone, Copy)]
struct Restriction {
    inflight: bool,
    not_before: Instant,
}

impl Restriction {
    fn live(&self, now: Instant) -> bool {
        self.inflight || now < self.not_before
    }
}

#[derive(Debug, Default)]
pub(crate) struct Hints {
    by_key: Map<InfoHash, [Option<Hint>; 2]>,
    /// Captures in arrival order, which is time order, for pruning.
    order: VecDeque<(Instant, InfoHash, usize)>,
    guard: Map<IpAddr, Restriction>,
    /// CR-HINT-FIRST: per canonical IP, the hash marked for T1 and when;
    /// empty unless `dht.hint_first` is on.
    t1: Map<IpAddr, (InfoHash, Instant)>,
    /// Compactions so far, for the amortised-bound test.
    #[cfg(test)]
    compactions: usize,
}

impl Hints {
    pub fn held(&self) -> usize {
        self.by_key.len()
    }

    pub fn guard_len(&self) -> usize {
        self.guard.len()
    }

    /// Remembers `addr` (already through [`check`]) as `key`'s responder
    /// in its family. An existing hint is never replaced or refreshed.
    /// Returns whether a hint was added.
    pub fn capture(
        &mut self,
        key: InfoHash,
        addr: SocketAddr,
        now: Instant,
    ) -> Result<bool, HintReject> {
        if !self.by_key.contains_key(&key) && self.by_key.len() >= HINT_CAPACITY {
            self.prune_hints(now);
            if self.by_key.len() >= HINT_CAPACITY {
                return Err(HintReject::Full);
            }
        }
        let s = slot(addr);
        let slots = self.by_key.entry(key).or_default();
        if slots[s].is_some() {
            return Ok(false);
        }
        slots[s] = Some(Hint { addr, at: now });
        self.order.push_back((now, key, s));
        if self.order.len() > ORDER_CAPACITY {
            self.compact_order();
        }
        Ok(true)
    }

    /// Takes `key`'s hints, by family slot (IPv4, IPv6): each is used once.
    /// A T1 mark for `key` at a taken hint's IP ends with it.
    pub fn take(&mut self, key: &InfoHash) -> [Option<Hint>; 2] {
        let found = self.by_key.remove(key).unwrap_or_default();
        for hint in found.iter().flatten() {
            let ip = canonical(hint.addr.ip());
            if self.t1.get(&ip).is_some_and(|(k, _)| k == key) {
                self.t1.remove(&ip);
            }
        }
        found
    }

    #[cfg(test)]
    pub fn t1_len(&self) -> usize {
        self.t1.len()
    }

    /// CR-HINT-FIRST: whether `key`, just sampled from `addr` (already
    /// through [`check`]), may be marked for T1 now: one outstanding T1
    /// hash per canonical IP, none while the guard restricts that IP, at
    /// most [`T1_RESPONDERS`] IPs, and room in the hint table to keep it.
    pub fn t1_check(
        &self,
        key: &InfoHash,
        addr: SocketAddr,
        now: Instant,
    ) -> Result<(), HintFirstSkip> {
        let ip = canonical(addr.ip());
        let outstanding = self.t1.get(&ip);
        if outstanding.is_some_and(|(_, at)| now.duration_since(*at) < HINT_TTL)
            || self.restricts(ip, now)
        {
            return Err(HintFirstSkip::Responder);
        }
        if outstanding.is_none() && self.t1.len() >= T1_RESPONDERS {
            return Err(HintFirstSkip::Full);
        }
        if !self.by_key.contains_key(key) && self.by_key.len() >= HINT_CAPACITY {
            return Err(HintFirstSkip::Rejected);
        }
        Ok(())
    }

    /// Records `key` as `addr`'s outstanding T1 hash (after
    /// [`Hints::t1_check`] and a successful [`Hints::capture`]).
    pub fn mark_t1(&mut self, key: InfoHash, addr: SocketAddr, now: Instant) {
        self.t1.insert(canonical(addr.ip()), (key, now));
    }

    /// The per-IP guard's permission for a hinted send to `ip` now.
    pub fn admit(&mut self, ip: IpAddr, now: Instant) -> Result<(), HintFallback> {
        let ip = canonical(ip);
        match self.guard.get(&ip) {
            Some(r) if r.live(now) => Err(HintFallback::Throttled),
            Some(_) => Ok(()),
            None if self.guard.len() < GUARD_CAPACITY => Ok(()),
            // Full: the tick's prune frees expired entries, so a full guard
            // costs no scan per attempt.
            None => Err(HintFallback::GuardFull),
        }
    }

    /// A hinted query to `ip` went out at `now` (after [`Hints::admit`]).
    pub fn sent(&mut self, ip: IpAddr, now: Instant) {
        self.guard.insert(
            canonical(ip),
            Restriction {
                inflight: true,
                not_before: now + HINT_NOT_BEFORE,
            },
        );
    }

    /// A hinted query to `ip` was answered, timed out or abandoned.
    pub fn settled(&mut self, ip: IpAddr) {
        if let Some(r) = self.guard.get_mut(&canonical(ip)) {
            r.inflight = false;
        }
    }

    /// Whether a retained restriction stops any lookup query to `ip` now.
    pub fn restricts(&self, ip: IpAddr, now: Instant) -> bool {
        self.guard.get(&canonical(ip)).is_some_and(|r| r.live(now))
    }

    /// Drops expired hints and restrictions; on every maintenance tick.
    /// Returns how many hints expired unused.
    pub fn prune(&mut self, now: Instant) -> u64 {
        let expired = self.prune_hints(now);
        self.prune_guard(now);
        self.t1
            .retain(|_, (_, at)| now.duration_since(*at) < HINT_TTL);
        expired
    }

    fn prune_hints(&mut self, now: Instant) -> u64 {
        let mut expired = 0;
        while let Some(&(at, key, s)) = self.order.front() {
            if now.duration_since(at) < HINT_TTL {
                break;
            }
            self.order.pop_front();
            if let Some(slots) = self.by_key.get_mut(&key) {
                // A slot taken and recaptured since has a later time.
                if slots[s].is_some_and(|h| h.at == at) {
                    slots[s] = None;
                    expired += 1;
                }
                if slots.iter().all(Option::is_none) {
                    self.by_key.remove(&key);
                }
            }
        }
        expired
    }

    /// Drops the order entries of hints already used or replaced, and all
    /// but one entry of each live hint: a slot taken and recaptured at the
    /// same `Instant` leaves entries that all match it. So at most one
    /// entry is left per live hint, 2 × [`HINT_CAPACITY`] (two families),
    /// and at least 2 × the cap captures pass before the next compaction:
    /// amortised O(1) per capture, however the clock reads. `retain` keeps
    /// the time order that [`Hints::prune_hints`] relies on; duplicates
    /// share one time, so which is kept doesn't matter.
    fn compact_order(&mut self) {
        #[cfg(test)]
        {
            self.compactions += 1;
        }
        let by_key = &self.by_key;
        let mut kept: Map<(InfoHash, usize), ()> = Map::default();
        self.order.retain(|&(at, key, s)| {
            by_key
                .get(&key)
                .is_some_and(|slots| slots[s].is_some_and(|h| h.at == at))
                && kept.insert((key, s), ()).is_none()
        });
        debug_assert!(self.order.len() <= 2 * HINT_CAPACITY);
    }

    fn prune_guard(&mut self, now: Instant) {
        self.guard.retain(|_, r| r.live(now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV6};

    fn public() -> Config {
        Config::default()
    }

    fn key(i: u32) -> InfoHash {
        let mut k = [0u8; 20];
        k[..4].copy_from_slice(&i.to_be_bytes());
        InfoHash(k)
    }

    fn ip(i: u32) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(0x1f00_0000 + i))
    }

    #[test]
    fn one_hint_per_family_never_refreshed_and_used_once() {
        let mut h = Hints::default();
        let t0 = Instant::now();
        let v4: SocketAddr = "31.0.0.1:6881".parse().unwrap();
        let v4b: SocketAddr = "31.0.0.2:6881".parse().unwrap();
        let v6: SocketAddr = "[2a00::1]:6881".parse().unwrap();
        assert_eq!(h.capture(key(1), v4, t0), Ok(true));
        // A rediscovery neither replaces nor refreshes it.
        assert_eq!(h.capture(key(1), v4b, t0 + HINT_TTL / 2), Ok(false));
        assert_eq!(h.capture(key(1), v6, t0), Ok(true));
        let [a, b] = h.take(&key(1));
        assert_eq!(a, Some(Hint { addr: v4, at: t0 }));
        assert_eq!(b.map(|h| h.addr), Some(v6));
        assert_eq!(h.take(&key(1)), [None, None]);
        assert_eq!(h.held(), 0);
    }

    #[test]
    fn hint_expiry_is_age_below_the_ttl() {
        let t0 = Instant::now();
        let hint = Hint {
            addr: "31.0.0.1:1".parse().unwrap(),
            at: t0,
        };
        assert!(hint.fresh(t0 + HINT_TTL - Duration::from_nanos(1)));
        assert!(!hint.fresh(t0 + HINT_TTL));
        let mut h = Hints::default();
        h.capture(key(1), hint.addr, t0).unwrap();
        h.prune(t0 + HINT_TTL - Duration::from_nanos(1));
        assert_eq!(h.held(), 1);
        assert_eq!(h.prune(t0 + HINT_TTL), 1);
        assert_eq!(h.held(), 0);
        assert!(h.order.is_empty());
    }

    #[test]
    fn ip_rotation_shares_one_canonical_restriction() {
        let mut h = Hints::default();
        let t0 = Instant::now();
        let v4 = IpAddr::V4(Ipv4Addr::new(31, 0, 0, 1));
        let mapped = IpAddr::V6(Ipv4Addr::new(31, 0, 0, 1).to_ipv6_mapped());
        assert_eq!(h.admit(v4, t0), Ok(()));
        h.sent(v4, t0);
        // In flight: another port, or the mapped form, is the same IP.
        assert_eq!(h.admit(mapped, t0), Err(HintFallback::Throttled));
        assert!(h.restricts(mapped, t0));
        h.settled(mapped);
        // Answered, but the not-before interval still holds, for the
        // fallback too.
        assert_eq!(
            h.admit(v4, t0 + Duration::from_secs(1)),
            Err(HintFallback::Throttled)
        );
        assert!(h.restricts(v4, t0 + HINT_NOT_BEFORE - Duration::from_nanos(1)));
        assert!(!h.restricts(mapped, t0 + HINT_NOT_BEFORE));
        assert_eq!(h.admit(mapped, t0 + HINT_NOT_BEFORE), Ok(()));
        assert_eq!(h.guard_len(), 1);
        // An unrestricted IP keeps today's policy.
        assert!(!h.restricts(ip(9), t0));
    }

    #[test]
    fn in_flight_holds_the_restriction_past_the_interval() {
        let mut h = Hints::default();
        let t0 = Instant::now();
        h.sent(ip(1), t0);
        let later = t0 + 2 * HINT_NOT_BEFORE;
        assert!(h.restricts(ip(1), later));
        h.prune(later);
        assert_eq!(h.guard_len(), 1, "an in-flight restriction isn't pruned");
        h.settled(ip(1));
        h.prune(later);
        assert_eq!(h.guard_len(), 0);
    }

    #[test]
    fn guard_saturation_evicts_no_restriction_and_prunes_on_expiry() {
        let mut h = Hints::default();
        let t0 = Instant::now();
        for i in 0..GUARD_CAPACITY as u32 {
            h.sent(ip(i), t0);
            h.settled(ip(i));
        }
        let new = ip(GUARD_CAPACITY as u32);
        assert_eq!(h.admit(new, t0), Err(HintFallback::GuardFull));
        assert_eq!(h.guard_len(), GUARD_CAPACITY);
        assert!((0..GUARD_CAPACITY as u32).all(|i| h.restricts(ip(i), t0)));
        // Ordinary routing to the new IP isn't restricted.
        assert!(!h.restricts(new, t0));
        // Expiry frees the whole table at the next prune.
        h.prune(t0 + HINT_NOT_BEFORE);
        assert_eq!(h.guard_len(), 0);
        assert_eq!(h.admit(new, t0 + HINT_NOT_BEFORE), Ok(()));
    }

    #[test]
    fn the_hint_table_is_bounded_and_pruned() {
        let mut h = Hints::default();
        let t0 = Instant::now();
        let addr: SocketAddr = "31.0.0.1:1".parse().unwrap();
        for i in 0..HINT_CAPACITY as u32 {
            h.capture(key(i), addr, t0).unwrap();
        }
        let extra = key(HINT_CAPACITY as u32);
        assert_eq!(h.capture(extra, addr, t0), Err(HintReject::Full));
        // A key already held still takes its other family.
        assert_eq!(
            h.capture(key(0), "[2a00::1]:1".parse().unwrap(), t0),
            Ok(true)
        );
        assert_eq!(h.held(), HINT_CAPACITY);
        assert_eq!(h.capture(extra, addr, t0 + HINT_TTL), Ok(true));
        assert_eq!(h.held(), 1);
        // Taking hints leaves order entries behind; they stay bounded at
        // every capture, with no tick in between.
        for i in 0..3 * ORDER_CAPACITY as u32 {
            let k = key(i + 1_000_000);
            h.capture(k, addr, t0 + HINT_TTL).unwrap();
            h.take(&k);
            assert!(h.order.len() <= ORDER_CAPACITY);
        }
        // Compaction keeps live entries in time order, so they still expire.
        let late = t0 + 2 * HINT_TTL;
        h.capture(key(7), addr, late).unwrap();
        for i in 0..ORDER_CAPACITY as u32 {
            let k = key(i + 9_000_000);
            h.capture(k, addr, late).unwrap();
            h.take(&k);
        }
        assert_eq!(h.held(), 2);
        assert!(h.order.iter().is_sorted_by_key(|(at, ..)| *at));
        assert_eq!(h.prune(late + HINT_TTL), 2);
        assert_eq!(h.held(), 0);
    }

    /// Recaptures at one `Instant` leave entries that all match a live
    /// hint, which `retain` alone can't drop: the order still stays
    /// bounded, and compactions stay rare.
    #[test]
    fn recaptures_at_one_instant_stay_bounded() {
        let mut h = Hints::default();
        let t0 = Instant::now();
        let addr: SocketAddr = "31.0.0.1:6881".parse().unwrap();
        let captures = 6 * ORDER_CAPACITY;
        for i in 0..captures {
            // Two keys alternate, so no entry repeats the one before it.
            let k = key(i as u32 % 2);
            h.take(&k);
            assert_eq!(h.capture(k, addr, t0), Ok(true));
            assert!(h.order.len() <= ORDER_CAPACITY, "at capture {i}");
        }
        assert_eq!(h.held(), 2);
        // One compaction per 2 × the cap captures at most, not one each.
        assert!(
            h.compactions <= captures / (2 * HINT_CAPACITY),
            "{} compactions for {captures} captures",
            h.compactions
        );
        assert!(h.compactions > 0);
        // The kept entries still expire the hints.
        assert_eq!(h.prune(t0 + HINT_TTL), 2);
        assert_eq!(h.held(), 0);
    }

    /// CR-HINT-FIRST: one outstanding T1 hash per canonical IP (mapped
    /// forms and ports included), none while the guard restricts it, and
    /// the mark ends when the hint is taken or at the TTL.
    #[test]
    fn one_t1_hash_per_responder_until_taken_or_expired() {
        let mut h = Hints::default();
        let t0 = Instant::now();
        let a: SocketAddr = "31.0.0.1:6881".parse().unwrap();
        let a_port: SocketAddr = "31.0.0.1:7000".parse().unwrap();
        let a_mapped: SocketAddr = "[::ffff:31.0.0.1]:6881".parse().unwrap();
        let b: SocketAddr = "31.0.0.2:6881".parse().unwrap();
        assert_eq!(h.t1_check(&key(1), a, t0), Ok(()));
        h.capture(key(1), a, t0).unwrap();
        h.mark_t1(key(1), a, t0);
        for other in [a, a_port, a_mapped] {
            assert_eq!(
                h.t1_check(&key(2), other, t0),
                Err(HintFirstSkip::Responder)
            );
        }
        assert_eq!(h.t1_check(&key(2), b, t0), Ok(()));
        // Taken: the mark ends, but the guard now holds the IP.
        assert!(h.take(&key(1))[0].is_some());
        assert_eq!(h.t1_len(), 0);
        h.sent(a.ip(), t0);
        assert_eq!(h.t1_check(&key(2), a, t0), Err(HintFirstSkip::Responder));
        h.settled(a.ip());
        assert_eq!(h.t1_check(&key(2), a, t0 + HINT_NOT_BEFORE), Ok(()));
        // Expired at the TTL, and pruned.
        h.capture(key(3), b, t0).unwrap();
        h.mark_t1(key(3), b, t0);
        let later = t0 + HINT_TTL - Duration::from_nanos(1);
        assert_eq!(h.t1_check(&key(4), b, later), Err(HintFirstSkip::Responder));
        assert_eq!(h.t1_check(&key(4), b, t0 + HINT_TTL), Ok(()));
        h.prune(t0 + HINT_TTL);
        assert_eq!(h.t1_len(), 0);
    }

    /// CR-HINT-FIRST: taking a hint ends only its own key's T1 mark.
    #[test]
    fn taking_another_keys_hint_keeps_the_mark() {
        let mut h = Hints::default();
        let t0 = Instant::now();
        let a: SocketAddr = "31.0.0.1:6881".parse().unwrap();
        h.capture(key(1), a, t0).unwrap();
        h.capture(key(2), a, t0).unwrap();
        h.mark_t1(key(1), a, t0);
        h.take(&key(2));
        assert_eq!(h.t1_len(), 1);
        h.take(&key(1));
        assert_eq!(h.t1_len(), 0);
    }

    /// CR-HINT-FIRST: at [`T1_RESPONDERS`] a new IP is refused; a stale
    /// mark's IP may be marked again before the prune.
    #[test]
    fn t1_marks_are_bounded() {
        let mut h = Hints::default();
        let t0 = Instant::now();
        for i in 0..T1_RESPONDERS as u32 {
            h.t1.insert(ip(i), (key(i), t0));
        }
        let fresh = SocketAddr::new(ip(T1_RESPONDERS as u32), 1);
        assert_eq!(h.t1_check(&key(0), fresh, t0), Err(HintFirstSkip::Full));
        let known = SocketAddr::new(ip(0), 1);
        assert_eq!(h.t1_check(&key(0), known, t0 + HINT_TTL), Ok(()));
        h.prune(t0 + HINT_TTL);
        assert_eq!(h.t1_check(&key(0), fresh, t0 + HINT_TTL), Ok(()));
    }

    #[test]
    fn endpoint_checks() {
        let cfg = public();
        let ok: SocketAddr = "31.0.0.1:6881".parse().unwrap();
        assert_eq!(check(ok, &cfg), Ok(ok));
        let mapped = SocketAddr::new(
            IpAddr::V6(Ipv4Addr::new(31, 0, 0, 1).to_ipv6_mapped()),
            6881,
        );
        assert_eq!(check(mapped, &cfg), Ok(ok));
        for (addr, why) in [
            ("31.0.0.1:0", HintReject::Invalid),
            ("10.0.0.1:6881", HintReject::Private),
            ("127.0.0.1:6881", HintReject::Private),
            ("192.0.2.1:6881", HintReject::Private),
            ("[fe80::1]:6881", HintReject::Private),
            ("[::ffff:192.168.0.1]:6881", HintReject::Private),
        ] {
            assert_eq!(check(addr.parse().unwrap(), &cfg), Err(why), "{addr}");
        }
        let scoped = SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 6881, 0, 2));
        assert_eq!(check(scoped, &cfg), Err(HintReject::Invalid));
        let deny = Config {
            deny: Some(super::super::Deny(std::sync::Arc::new(|_| true))),
            ..Config::default()
        };
        assert_eq!(check(ok, &deny), Err(HintReject::Private));
    }

    fn any_endpoint() -> impl Strategy<Value = SocketAddr> {
        let port = prop_oneof![Just(0u16), any::<u16>()];
        let v4 = (any::<[u8; 4]>(), port.clone())
            .prop_map(|(o, p)| SocketAddr::new(IpAddr::V4(Ipv4Addr::from(o)), p));
        let mapped = (any::<[u8; 4]>(), port.clone())
            .prop_map(|(o, p)| SocketAddr::new(IpAddr::V6(Ipv4Addr::from(o).to_ipv6_mapped()), p));
        let v6 = (
            any::<[u8; 16]>(),
            port,
            prop_oneof![Just(0u32), any::<u32>()],
        )
            .prop_map(|(o, p, scope)| {
                SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::from(o), p, 0, scope))
            });
        prop_oneof![v4, mapped, v6]
    }

    proptest! {
        /// Whatever a responder's endpoint looks like, an accepted hint is
        /// normalized, public, nonzero-port and unscoped, and its guard key
        /// is the same for its mapped and plain forms.
        #[test]
        fn accepted_hints_are_public_and_canonical(addr in any_endpoint()) {
            let cfg = public();
            match check(addr, &cfg) {
                Ok(a) => {
                    prop_assert_eq!(a, lodestar_network_policy::normalize(addr));
                    prop_assert!(a.port() != 0);
                    prop_assert!(lodestar_network_policy::allows(a, false));
                    prop_assert!(!matches!(a, SocketAddr::V6(v6) if v6.scope_id() != 0
                        || v6.ip().to_ipv4_mapped().is_some()));
                    prop_assert_eq!(canonical(addr.ip()), a.ip());
                    let mut h = Hints::default();
                    let t0 = Instant::now();
                    h.sent(addr.ip(), t0);
                    prop_assert!(h.restricts(a.ip(), t0));
                    prop_assert_eq!(h.admit(a.ip(), t0), Err(HintFallback::Throttled));
                }
                Err(why) => {
                    let invalid = addr.port() == 0
                        || matches!(addr, SocketAddr::V6(v6) if v6.scope_id() != 0);
                    prop_assert_eq!(why == HintReject::Invalid, invalid);
                    prop_assert!(invalid || !lodestar_network_policy::allows(addr, false));
                }
            }
        }
    }
}
