//! Blocklist formats (§20.13): what subscriptions download. Everything
//! here parses untrusted input, so it's strict about what it accepts,
//! bounded in what it keeps, and counts what it skips instead of failing
//! a whole list over one bad line.
//!
//! - **Hash lists:** one info hash per line, as 40 hex digits, 32 base32
//!   characters or a magnet link.
//! - **IP lists:** PeerGuardian `.p2p` (`description:1.2.3.4-1.2.3.255`),
//!   eMule `ipfilter.dat` (`1.2.3.4 - 1.2.3.255 , 000 , description`;
//!   access levels of 128 and above allow, so they're skipped), and
//!   CIDR/netset (`1.2.3.0/24`, a bare address, or IPv6 forms).
//!
//! `#` and `//` start comment lines. Compressed downloads are gunzipped
//! with a cap on the expanded size.

use std::collections::HashSet;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, RwLock};

use lodestar_store::{FloorSource, Store};

use crate::core::Action;
use crate::engine::{Handle, ListLayer};

/// Entries kept per list.
pub const MAX_ENTRIES: usize = 4_000_000;
/// Longer lines are skipped (counted as invalid).
const MAX_LINE: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Hashes,
    P2p,
    Dat,
    Cidr,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Hashes => "hashes",
            Format::P2p => "p2p",
            Format::Dat => "dat",
            Format::Cidr => "cidr",
        }
    }

    pub fn from_name(name: &str) -> Option<Format> {
        [Format::Hashes, Format::P2p, Format::Dat, Format::Cidr]
            .into_iter()
            .find(|f| f.as_str() == name)
    }

    pub fn is_ip(self) -> bool {
        !matches!(self, Format::Hashes)
    }
}

/// What a parse kept and skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed<T> {
    pub entries: Vec<T>,
    /// Lines that weren't comments or blank and didn't parse.
    pub invalid: usize,
    /// Entries past [`MAX_ENTRIES`].
    pub truncated: usize,
}

impl<T> Default for Parsed<T> {
    fn default() -> Self {
        Parsed {
            entries: Vec::new(),
            invalid: 0,
            truncated: 0,
        }
    }
}

impl<T> Parsed<T> {
    fn keep(&mut self, entry: Option<T>) {
        match entry {
            Some(_) if self.entries.len() >= MAX_ENTRIES => self.truncated += 1,
            Some(e) => self.entries.push(e),
            None => self.invalid += 1,
        }
    }
}

fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("//"))
}

/// Guesses a list's format from its first meaningful lines.
pub fn detect(text: &str) -> Option<Format> {
    for line in lines(text).take(20).filter(|l| l.len() <= MAX_LINE) {
        if hash(line).is_some() {
            return Some(Format::Hashes);
        }
        if dat_line(line).is_some() {
            return Some(Format::Dat);
        }
        if cidr(line).is_some() {
            return Some(Format::Cidr);
        }
        if p2p_line(line).is_some() {
            return Some(Format::P2p);
        }
    }
    None
}

pub fn parse_hashes(text: &str) -> Parsed<[u8; 20]> {
    let mut out = Parsed::default();
    for line in lines(text) {
        out.keep(if line.len() > MAX_LINE {
            None
        } else {
            hash(line)
        });
    }
    out.entries.sort_unstable();
    out.entries.dedup();
    out
}

/// One info hash: 40 hex digits, 32 base32 characters, or a magnet link.
fn hash(s: &str) -> Option<[u8; 20]> {
    let s = match s.strip_prefix("magnet:?") {
        Some(q) => q.split('&').find_map(|p| p.strip_prefix("xt=urn:btih:"))?,
        None => s,
    };
    match s.len() {
        40 => {
            let mut out = [0u8; 20];
            for (i, pair) in s.as_bytes().chunks(2).enumerate() {
                let hex = std::str::from_utf8(pair).ok()?;
                if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return None;
                }
                out[i] = u8::from_str_radix(hex, 16).ok()?;
            }
            Some(out)
        }
        32 => base32(s),
        _ => None,
    }
}

fn base32(s: &str) -> Option<[u8; 20]> {
    let mut out = [0u8; 20];
    let (mut acc, mut bits, mut n) = (0u64, 0u32, 0usize);
    for c in s.bytes() {
        let v = match c.to_ascii_uppercase() {
            c @ b'A'..=b'Z' => c - b'A',
            c @ b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        acc = (acc << 5) | u64::from(v);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out[n] = (acc >> bits) as u8;
            n += 1;
        }
    }
    (n == 20).then_some(out)
}

/// An inclusive address range, both ends in the same family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpRange {
    V4(u32, u32),
    V6(u128, u128),
}

impl IpRange {
    fn new(a: IpAddr, b: IpAddr) -> Option<IpRange> {
        match (a, b) {
            (IpAddr::V4(a), IpAddr::V4(b)) if a <= b => Some(IpRange::V4(a.into(), b.into())),
            (IpAddr::V6(a), IpAddr::V6(b)) if a <= b => Some(IpRange::V6(a.into(), b.into())),
            _ => None,
        }
    }
}

pub fn parse_ips(text: &str, format: Format) -> Parsed<IpRange> {
    let mut out = Parsed::default();
    for line in lines(text) {
        let r = if line.len() > MAX_LINE {
            None
        } else {
            match format {
                Format::P2p => p2p_line(line),
                Format::Dat => match dat_line(line) {
                    // An allowing entry: not a block, not an error.
                    Some(None) => continue,
                    Some(r) => r,
                    None => None,
                },
                Format::Cidr => cidr(line),
                Format::Hashes => None,
            }
        };
        out.keep(r);
    }
    out
}

fn addr(s: &str) -> Option<IpAddr> {
    let s = s.trim();
    // eMule pads octets with zeros (`001.002.003.004`), which the standard
    // parser rejects as ambiguous; here they're plainly decimal.
    if s.contains('.') && !s.contains(':') {
        let mut octets = [0u8; 4];
        let mut parts = s.split('.');
        for o in &mut octets {
            let p = parts.next()?;
            if p.is_empty() || p.len() > 3 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            *o = p.parse().ok()?;
        }
        return parts
            .next()
            .is_none()
            .then(|| IpAddr::V4(Ipv4Addr::from(octets)));
    }
    s.parse::<Ipv6Addr>().ok().map(IpAddr::V6)
}

/// `description:first-last`; the description may itself contain colons.
fn p2p_line(line: &str) -> Option<IpRange> {
    let (_, range) = line.rsplit_once(':')?;
    // An IPv6 range would be split inside the address; p2p is IPv4 only.
    let (a, b) = range.split_once('-')?;
    IpRange::new(addr(a)?, addr(b)?).filter(|r| matches!(r, IpRange::V4(..)))
}

/// `first - last , level , description`. `Some(None)` for an allowing
/// entry (level 128 and above).
fn dat_line(line: &str) -> Option<Option<IpRange>> {
    let mut fields = line.splitn(3, ',');
    let range = fields.next()?;
    let level: u16 = fields.next()?.trim().parse().ok()?;
    let (a, b) = range.split_once('-')?;
    let r = IpRange::new(addr(a)?, addr(b)?)?;
    Some((level < 128).then_some(r))
}

/// `addr`, `addr/len`, or a `first-last` range (some netsets use it).
fn cidr(line: &str) -> Option<IpRange> {
    let line = line.split_whitespace().next()?;
    if let Some((a, b)) = line.split_once('-') {
        return IpRange::new(addr(a)?, addr(b)?);
    }
    let (a, len) = match line.split_once('/') {
        Some((a, len)) => (addr(a)?, Some(len.parse::<u32>().ok()?)),
        None => (addr(line)?, None),
    };
    match a {
        IpAddr::V4(v) => {
            let len = len.unwrap_or(32);
            if len > 32 {
                return None;
            }
            let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
            let lo = u32::from(v) & mask;
            Some(IpRange::V4(lo, lo | !mask))
        }
        IpAddr::V6(v) => {
            let len = len.unwrap_or(128);
            if len > 128 {
                return None;
            }
            let mask = if len == 0 {
                0
            } else {
                u128::MAX << (128 - len)
            };
            let lo = u128::from(v) & mask;
            Some(IpRange::V6(lo, lo | !mask))
        }
    }
}

/// Sorted, merged, non-overlapping ranges per family: a lookup is a
/// binary search.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IpSet {
    v4: Vec<(u32, u32)>,
    v6: Vec<(u128, u128)>,
}

fn merge<T: Ord + Copy>(mut v: Vec<(T, T)>, next: impl Fn(T) -> Option<T>) -> Vec<(T, T)> {
    v.sort_unstable();
    let mut out: Vec<(T, T)> = Vec::with_capacity(v.len());
    for (a, b) in v {
        match out.last_mut() {
            // Overlapping or adjacent.
            Some(last) if next(last.1).is_none_or(|n| a <= n) => {
                if b > last.1 {
                    last.1 = b;
                }
            }
            _ => out.push((a, b)),
        }
    }
    out
}

/// How many addresses of `r` the merged ranges `v` cover. Saturates, so
/// an IPv6 count can't overflow.
fn overlap<T>(v: &[(T, T)], r: (T, T)) -> T::Wide
where
    T: Ord + Copy + Widen,
{
    let start = v.partition_point(|&(_, b)| b < r.0);
    let mut n = T::Wide::default();
    for &(a, b) in &v[start..] {
        if a > r.1 {
            break;
        }
        n = T::add(n, T::span(a.max(r.0), b.min(r.1)));
    }
    n
}

/// Range sizes without overflow.
trait Widen: Sized {
    type Wide: Default + Copy;
    fn span(a: Self, b: Self) -> Self::Wide;
    fn add(x: Self::Wide, y: Self::Wide) -> Self::Wide;
}

impl Widen for u32 {
    type Wide = u64;
    fn span(a: u32, b: u32) -> u64 {
        u64::from(b) - u64::from(a) + 1
    }
    fn add(x: u64, y: u64) -> u64 {
        x + y
    }
}

impl Widen for u128 {
    type Wide = u128;
    fn span(a: u128, b: u128) -> u128 {
        (b - a).saturating_add(1)
    }
    fn add(x: u128, y: u128) -> u128 {
        x.saturating_add(y)
    }
}

/// IANA special-purpose IPv4 ranges that aren't globally reachable,
/// sorted and disjoint.
const V4_SPECIAL: &[(u32, u32)] = &[
    v4(0, 0, 0, 0, 8),       // "this network"
    v4(10, 0, 0, 0, 8),      // private
    v4(100, 64, 0, 0, 10),   // carrier-grade NAT
    v4(127, 0, 0, 0, 8),     // loopback
    v4(169, 254, 0, 0, 16),  // link-local
    v4(172, 16, 0, 0, 12),   // private
    v4(192, 0, 0, 0, 24),    // protocol assignments
    v4(192, 0, 2, 0, 24),    // documentation
    v4(192, 168, 0, 0, 16),  // private
    v4(198, 18, 0, 0, 15),   // benchmarking
    v4(198, 51, 100, 0, 24), // documentation
    v4(203, 0, 113, 0, 24),  // documentation
    v4(224, 0, 0, 0, 4),     // multicast
    v4(240, 0, 0, 0, 4),     // reserved and broadcast
];

const fn v4(a: u8, b: u8, c: u8, d: u8, len: u32) -> (u32, u32) {
    let lo = u32::from_be_bytes([a, b, c, d]);
    (lo, lo | (u32::MAX >> len))
}

fn find<T: Ord + Copy>(v: &[(T, T)], x: T) -> bool {
    let i = v.partition_point(|&(a, _)| a <= x);
    i > 0 && v[i - 1].1 >= x
}

impl IpSet {
    pub fn new(ranges: impl IntoIterator<Item = IpRange>) -> IpSet {
        let (mut v4, mut v6) = (Vec::new(), Vec::new());
        for r in ranges {
            match r {
                IpRange::V4(a, b) => v4.push((a, b)),
                IpRange::V6(a, b) => v6.push((a, b)),
            }
        }
        IpSet {
            v4: merge(v4, |x: u32| x.checked_add(1)),
            v6: merge(v6, |x: u128| x.checked_add(1)),
        }
    }

    /// Every range of several sets.
    pub fn union<'a>(sets: impl IntoIterator<Item = &'a IpSet>) -> IpSet {
        let mut ranges = Vec::new();
        for s in sets {
            ranges.extend(s.v4.iter().map(|&(a, b)| IpRange::V4(a, b)));
            ranges.extend(s.v6.iter().map(|&(a, b)| IpRange::V6(a, b)));
        }
        IpSet::new(ranges)
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v) => find(&self.v4, u32::from(v)),
            IpAddr::V6(v) => match v.to_ipv4_mapped() {
                Some(v4) => find(&self.v4, u32::from(v4)),
                None => find(&self.v6, u128::from(v)),
            },
        }
    }

    /// The share of the IPv4 address space covered (0–1). A broad list
    /// costs the crawler coverage (§20.13), so this is shown and warned on.
    pub fn v4_coverage(&self) -> f64 {
        let n: u64 = self
            .v4
            .iter()
            .map(|&(a, b)| u64::from(b) - u64::from(a) + 1)
            .sum();
        n as f64 / (1u64 << 32) as f64
    }

    /// The share of the public IPv4 space covered (0–1): the space minus
    /// the special-purpose ranges that hold no DHT nodes (private,
    /// loopback, link-local, documentation, multicast, reserved). This is
    /// what a list costs the crawler, so the strictness assessment uses it.
    pub fn v4_public_coverage(&self) -> f64 {
        let size = |&(a, b): &(u32, u32)| u64::from(b) - u64::from(a) + 1;
        let reserved: u64 = V4_SPECIAL.iter().map(size).sum();
        let covered: u64 = self.v4.iter().map(size).sum();
        let inside: u64 = V4_SPECIAL.iter().map(|r| overlap(&self.v4, *r)).sum();
        (covered - inside) as f64 / ((1u64 << 32) - reserved) as f64
    }

    /// The share of IPv6 global unicast (`2000::/3`) covered (0–1).
    pub fn v6_coverage(&self) -> f64 {
        let lo = 0x2000u128 << 112;
        let hi = lo | (u128::MAX >> 3);
        let n = overlap(&self.v6, (lo, hi));
        n as f64 / (hi - lo) as f64
    }

    /// Whether every address in `other` is in this set too, so swapping
    /// `other` for this set unblocks nothing.
    pub fn covers(&self, other: &IpSet) -> bool {
        fn all<T: Ord + Copy>(have: &[(T, T)], want: &[(T, T)]) -> bool {
            // Both are merged, so a covered range lies in one of `have`'s.
            want.iter().all(|&(a, b)| {
                let i = have.partition_point(|&(lo, _)| lo <= a);
                i > 0 && have[i - 1].1 >= b
            })
        }
        all(&self.v4, &other.v4) && all(&self.v6, &other.v6)
    }

    pub fn v4_ranges(&self) -> usize {
        self.v4.len()
    }

    pub fn v6_ranges(&self) -> usize {
        self.v6.len()
    }

    /// Merged ranges.
    pub fn len(&self) -> usize {
        self.v4.len() + self.v6.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A compact form for the store: per range, a family byte and both
    /// ends in big-endian.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.v4.len() * 9 + self.v6.len() * 33);
        for &(a, b) in &self.v4 {
            out.push(4);
            out.extend(a.to_be_bytes());
            out.extend(b.to_be_bytes());
        }
        for &(a, b) in &self.v6 {
            out.push(6);
            out.extend(a.to_be_bytes());
            out.extend(b.to_be_bytes());
        }
        out
    }

    pub fn decode(mut data: &[u8]) -> Option<IpSet> {
        let mut ranges = Vec::new();
        while let Some((&family, rest)) = data.split_first() {
            let (range, rest) = match family {
                4 if rest.len() >= 8 => {
                    let a = u32::from_be_bytes(rest[..4].try_into().ok()?);
                    let b = u32::from_be_bytes(rest[4..8].try_into().ok()?);
                    (IpRange::V4(a, b), &rest[8..])
                }
                6 if rest.len() >= 32 => {
                    let a = u128::from_be_bytes(rest[..16].try_into().ok()?);
                    let b = u128::from_be_bytes(rest[16..32].try_into().ok()?);
                    (IpRange::V6(a, b), &rest[32..])
                }
                _ => return None,
            };
            ranges.push(range);
            data = rest;
        }
        Some(IpSet::new(ranges))
    }
}

/// The current union of every subscribed IP list, read on every packet
/// and connection and replaced whole on each refresh.
#[derive(Debug, Default)]
pub struct SharedIps(RwLock<Arc<IpSet>>);

impl SharedIps {
    pub fn contains(&self, ip: IpAddr) -> bool {
        self.0
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(ip)
    }

    pub fn replace(&self, set: IpSet) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(set);
    }

    pub fn current(&self) -> Arc<IpSet> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// `subscriptions.kind` for IP lists (0 is content).
pub const KIND_IP: u8 = 1;

/// IP lists covering more than this share of IPv4 get a warning.
const BROAD: f64 = 0.10;

/// Rebuilds the filter's list layers and the shared IP set from every
/// enabled subscription's applied version.
pub fn apply(store: &Store, filter: &Handle, ips: &SharedIps) -> Result<(), String> {
    let subs = store.subscriptions().map_err(|e| e.to_string())?;
    let mut ip_sets = Vec::new();
    let mut content = Vec::new();
    for s in &subs {
        let data = if s.enabled {
            store
                .subscription_data(s.id, false)
                .map_err(|e| e.to_string())?
        } else {
            None
        };
        match s.kind {
            KIND_IP => {
                if let Some(set) = data.as_deref().and_then(IpSet::decode) {
                    ip_sets.push(set);
                }
            }
            _ => {
                let mut hashes = data.map(|d| {
                    d.chunks_exact(20)
                        .map(|c| <[u8; 20]>::try_from(c).expect("20 bytes"))
                        .collect::<HashSet<_>>()
                });
                // A floor-marked list only adds (owner's decision §2.7):
                // what its publisher removed stays until you remove it in
                // the app.
                if let (true, Some(set)) = (s.floor, hashes.as_mut()) {
                    let kept = store
                        .pending_floor_of(&FloorSource::List(s.id))
                        .map_err(|e| e.to_string())?;
                    set.extend(kept);
                }
                content.push((s, hashes.map(Arc::new)));
            }
        }
    }
    let set = IpSet::union(&ip_sets);
    let coverage = set.v4_coverage();
    if coverage > BROAD {
        lodestar_log::log_event!(
            crate::log_events::FILTERS_IP_LISTS_BROAD,
            percent = (coverage * 100.0).round() as u64
        );
    }
    ips.replace(set);
    filter.update_engine(|e| {
        // A list that's no longer subscribed stops applying too, so
        // nothing released for it is held by it again.
        let known: HashSet<u32> = subs.iter().map(|s| s.id as u32).collect();
        e.retain_lists(|id| known.contains(&id));
        e.core.cs.retain_lists(|id| known.contains(&id));
        for (s, hashes) in content {
            let id = s.id as u32;
            if s.floor {
                e.core.cs.set_list(id, hashes);
                e.set_list(None, id);
            } else {
                let action = Action::from_code(s.action)
                    .filter(|a| a.protective())
                    .unwrap_or(Action::Drop);
                e.set_list(hashes.map(|hashes| ListLayer { id, action, hashes }), id);
                e.core.cs.set_list(id, None);
            }
        }
    });
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    TooLarge,
    Corrupt,
    NotText,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            DecodeError::TooLarge => "list is larger than the size limit",
            DecodeError::Corrupt => "list is compressed but damaged",
            DecodeError::NotText => "list isn't text",
        })
    }
}

impl std::error::Error for DecodeError {}

/// A downloaded list as text: gunzipped if it's gzip, at most `max`
/// bytes expanded. Invalid UTF-8 is replaced (descriptions in old lists
/// are often Latin-1), but a binary body is refused.
pub fn decode(body: &[u8], max: usize) -> Result<String, DecodeError> {
    let bytes = if body.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::MultiGzDecoder::new(body)
            .take(max as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|_| DecodeError::Corrupt)?;
        out
    } else {
        body.to_vec()
    };
    if bytes.len() > max {
        return Err(DecodeError::TooLarge);
    }
    if bytes.iter().take(4096).any(|&b| b == 0) {
        return Err(DecodeError::NotText);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn hashes() {
        let text = "# comment\n\
            0123456789abcdef0123456789ABCDEF01234567\n\
            magnet:?dn=x&xt=urn:btih:AERUKZ4JVPG66AJDIVSTGI2ZPHVRCELS&tr=y\n\
            not a hash\n\
            0123456789abcdef0123456789abcdef01234567\n";
        let p = parse_hashes(text);
        assert_eq!(p.entries.len(), 2, "deduplicated");
        assert_eq!(p.invalid, 1);
        assert_eq!(p.entries[0][0], 0x01);
        assert_eq!(detect(text), Some(Format::Hashes));
        assert_eq!(hash(&"g".repeat(40)), None);
        assert_eq!(base32("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA1"), None);
    }

    #[test]
    fn p2p() {
        let text = "\"Some: Org\" Ltd:1.2.3.4-1.2.3.255\nbad line\nX:5.6.7.8-5.6.7.1\n";
        let p = parse_ips(text, Format::P2p);
        assert_eq!(p.entries, vec![IpRange::V4(0x0102_0304, 0x0102_03ff)]);
        assert_eq!(p.invalid, 2, "a reversed range is invalid");
        assert_eq!(detect(text), Some(Format::P2p));
    }

    #[test]
    fn dat() {
        let text = "001.002.003.004 - 001.002.003.255 , 000 , Org\n\
                    009.009.009.000 - 009.009.009.255 , 200 , allowed\n";
        let p = parse_ips(text, Format::Dat);
        assert_eq!(p.entries, vec![IpRange::V4(0x0102_0304, 0x0102_03ff)]);
        assert_eq!(p.invalid, 0);
        assert_eq!(detect(text), Some(Format::Dat));
    }

    #[test]
    fn cidr_and_sets() {
        let text = "10.0.0.0/8\n192.168.1.7\n2001:db8::/32\n10.1.0.0/16\n1.1.1.1-1.1.1.9\n300.1.1.1\n10.0.0.0/33\n";
        assert_eq!(detect(text), Some(Format::Cidr));
        let p = parse_ips(text, Format::Cidr);
        assert_eq!(p.invalid, 2);
        let set = IpSet::new(p.entries);
        assert_eq!(set.len(), 4, "10.1/16 merges into 10/8");
        for yes in [
            "10.255.0.1",
            "192.168.1.7",
            "2001:db8::1",
            "1.1.1.5",
            "::ffff:10.0.0.1",
        ] {
            assert!(set.contains(ip(yes)), "{yes}");
        }
        for no in [
            "11.0.0.0",
            "192.168.1.8",
            "2001:db9::",
            "1.1.1.10",
            "9.255.255.255",
        ] {
            assert!(!set.contains(ip(no)), "{no}");
        }
        assert_eq!(IpSet::decode(&set.encode()), Some(set.clone()));
        assert_eq!(IpSet::decode(&[4, 1, 2]), None);
        let edge = IpSet::new([IpRange::V4(u32::MAX - 1, u32::MAX), IpRange::V4(0, 0)]);
        assert!(edge.contains(ip("255.255.255.255")) && edge.contains(ip("0.0.0.0")));
        assert_eq!(IpSet::new([IpRange::V4(0, u32::MAX)]).v4_coverage(), 1.0);
        assert!((set.v4_coverage() - (1u64 << 24) as f64 / (1u64 << 32) as f64).abs() < 1e-6);
        let both = IpSet::union([&set, &edge]);
        assert_eq!(both.len(), 6);
    }

    #[test]
    fn covers_means_nothing_is_unblocked() {
        let v4 = |a, b| IpRange::V4(a, b);
        let wide = IpSet::new([v4(10, 20), v4(21, 30), IpRange::V6(5, 9)]);
        let empty = IpSet::default();
        assert!(wide.covers(&empty) && wide.covers(&wide));
        assert!(!empty.covers(&wide));
        // Adjacent ranges merge, so a range across the join is covered.
        assert!(wide.covers(&IpSet::new([v4(15, 25), IpRange::V6(6, 9)])));
        for gone in [v4(9, 12), v4(29, 31), v4(40, 40), IpRange::V6(9, 10)] {
            assert!(!wide.covers(&IpSet::new([gone])), "{gone:?}");
        }
        let gap = IpSet::new([v4(10, 14), v4(16, 20)]);
        assert!(!gap.covers(&IpSet::new([v4(12, 18)])));
    }

    #[test]
    fn decoding() {
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(b"1.2.3.4\n").unwrap();
        let gz = gz.finish().unwrap();
        assert_eq!(decode(&gz, 100).unwrap(), "1.2.3.4\n");
        assert_eq!(decode(&gz, 4), Err(DecodeError::TooLarge));
        assert_eq!(decode(&gz[..gz.len() - 4], 100), Err(DecodeError::Corrupt));
        assert_eq!(decode(b"a\0b", 100), Err(DecodeError::NotText));
        assert_eq!(decode(b"plain", 100).unwrap(), "plain");
    }
}
