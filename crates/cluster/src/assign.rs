//! The `Assign` payload, v1 (architecture §19.5; workers plan WK-5): what
//! the coordinator tells one worker to run.
//!
//! [`proto::Assign`] carries a revision and opaque bytes; this module
//! defines those bytes. An [`Assignment`] says:
//! - which built-in roles run: the DHT (with its keyspace [`ShardSpec`]),
//!   metadata fetch and prepare. Source plugins have no field: v1 never
//!   places one on a worker (plan Q9);
//! - the worker's [`Budget`] and [`Intensity`] (§19.5: each worker has its
//!   own);
//! - the fetch-pool cap (plan Q5, [`DEFAULT_POOL_CAP`] keys);
//! - the dictionary and filter-snapshot versions to judge and prepare with
//!   (their bytes travel as `ToWorker::Snapshot` chunks);
//! - the worker's share of per-remote limits ([`Politeness`]), so workers
//!   behind one public IP never look like more than one install.
//!
//! **Encoding.** One version byte, then a fixed layout, little-endian: no
//! length field, nothing allocated while decoding. An unknown version is
//! refused before anything else is read, a payload of the wrong length is
//! refused before any field is read, and every field is checked before an
//! [`Assignment`] is returned, so nothing is ever half-used.

use std::fmt;
use std::net::IpAddr;
use std::ops::Range;

use crate::proto;

/// The payload version this build writes and reads.
pub const ASSIGN_VERSION: u8 = 1;
/// A v1 payload's length, version byte included.
pub const ASSIGN_V1_LEN: usize = 56;
/// The worker's fetch-pool cap by default: 50,000 keys (plan Q5, about
/// 34–46 MiB).
pub const DEFAULT_POOL_CAP: u32 = 50_000;
/// The largest pool cap a payload may carry, so a bad value can't size a
/// worker's pool past any memory budget.
pub const MAX_POOL_CAP: u32 = 1_000_000;
/// The 8 BEP 42 id slots.
pub const ALL_SLOTS: u8 = 0xff;
/// Connections one install opens to one peer IP (the fetch pool's
/// `per_ip`, §6).
pub const INSTALL_PER_PEER_IP: u8 = 2;

const ROLE_DHT: u8 = 1;
const ROLE_FETCH: u8 = 2;
const ROLE_PREPARE: u8 = 4;
const ROLES: u8 = ROLE_DHT | ROLE_FETCH | ROLE_PREPARE;

/// A DHT keyspace shard as it travels: `start..end` of the 256 target
/// prefixes and a bitset of the 8 BEP 42 id slots. It checks what
/// `lodestar_dht::node::shard::Shard::new` checks, so a decoded one always
/// converts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShardSpec {
    start: u16,
    end: u16,
    id_slots: u8,
}

impl ShardSpec {
    /// Every prefix, every slot: no shard.
    pub const FULL: ShardSpec = ShardSpec {
        start: 0,
        end: 256,
        id_slots: ALL_SLOTS,
    };

    /// `None` for an empty range, one past 256, or no slot.
    pub fn new(prefixes: Range<u16>, id_slots: u8) -> Option<ShardSpec> {
        (prefixes.start < prefixes.end && prefixes.end <= 256 && id_slots != 0).then_some(
            ShardSpec {
                start: prefixes.start,
                end: prefixes.end,
                id_slots,
            },
        )
    }

    pub fn prefixes(&self) -> Range<u16> {
        self.start..self.end
    }

    pub fn id_slots(&self) -> u8 {
        self.id_slots
    }

    pub fn is_full(&self) -> bool {
        *self == ShardSpec::FULL
    }
}

/// The DHT crawl preset (§5), as the worker's own setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Intensity {
    Low,
    #[default]
    Balanced,
    Aggressive,
}

impl Intensity {
    fn code(self) -> u8 {
        match self {
            Intensity::Low => 0,
            Intensity::Balanced => 1,
            Intensity::Aggressive => 2,
        }
    }

    fn from_code(c: u8) -> Option<Intensity> {
        match c {
            0 => Some(Intensity::Low),
            1 => Some(Intensity::Balanced),
            2 => Some(Intensity::Aggressive),
            _ => None,
        }
    }
}

/// The worker's local resource budget (§19.5, §18.4's scheduler runs per
/// worker). A 0 for memory or bandwidth means no cap beyond the defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Budget {
    /// CPU share, 1–1000 ‰.
    pub cpu_permille: u16,
    /// Memory cap in MiB, 0 for none.
    pub ram_mb: u32,
    pub up_kbps: u32,
    pub down_kbps: u32,
}

impl Default for Budget {
    fn default() -> Budget {
        Budget {
            cpu_permille: 1000,
            ram_mb: 0,
            up_kbps: 0,
            down_kbps: 0,
        }
    }
}

/// This worker's share of the limits a remote peer or node sees from one
/// public address (plan Q9, "same-IP politeness").
///
/// - **Connections per peer IP.** The members of one public IP are split
///   into `peer_ip_groups` groups. A peer IP belongs to exactly one group
///   ([`peer_ip_bucket`]); only that group's members connect to it, each
///   up to `per_peer_ip`, and a group's limits sum to at most one
///   install's ([`INSTALL_PER_PEER_IP`]).
/// - **BEP 51 intervals.** Each member waits `bep51_interval_scale` times
///   the interval a remote node asks for, so the members together sample
///   it no more often than one install.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Politeness {
    pub peer_ip_groups: u16,
    pub peer_ip_group: u16,
    pub per_peer_ip: u8,
    pub bep51_interval_scale: u8,
}

impl Politeness {
    /// A single install's limits.
    pub const ALONE: Politeness = Politeness {
        peer_ip_groups: 1,
        peer_ip_group: 0,
        per_peer_ip: INSTALL_PER_PEER_IP,
        bep51_interval_scale: 1,
    };

    /// How many connections this member may open to `peer`: its share, or
    /// 0 when another group serves that IP.
    pub fn connections_to(&self, peer: IpAddr) -> u8 {
        if peer_ip_bucket(peer, self.peer_ip_groups) == self.peer_ip_group {
            self.per_peer_ip
        } else {
            0
        }
    }

    fn valid(&self) -> bool {
        self.peer_ip_groups >= 1
            && self.peer_ip_group < self.peer_ip_groups
            && (1..=INSTALL_PER_PEER_IP).contains(&self.per_peer_ip)
            && self.bep51_interval_scale >= 1
    }
}

/// Which of `groups` a peer IP belongs to. Every member computes the same
/// answer (FNV-1a over the address's bytes, an IPv4-mapped IPv6 address
/// taken as its IPv4 address). The hash isn't keyed: a peer gains nothing
/// by choosing its bucket, it only changes which of our members dials it.
pub fn peer_ip_bucket(peer: IpAddr, groups: u16) -> u16 {
    if groups <= 1 {
        return 0;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: &[u8]| {
        for &x in b {
            h ^= u64::from(x);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    match peer.to_canonical() {
        IpAddr::V4(a) => eat(&a.octets()),
        IpAddr::V6(a) => eat(&a.octets()),
    }
    (h % u64::from(groups)) as u16
}

/// What one worker runs, at one revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Assignment {
    /// The fleet plan's revision, ≥ 1; a worker applies only a newer one.
    pub revision: u64,
    /// The DHT's shard, or `None` when this worker runs no DHT.
    pub dht: Option<ShardSpec>,
    pub fetch: bool,
    pub prepare: bool,
    pub intensity: Intensity,
    pub budget: Budget,
    /// Keys the worker's fetch pool may hold (plan Q5), 1–[`MAX_POOL_CAP`].
    pub pool_cap: u32,
    /// The file-tree dictionary's version, 0 before one is sent.
    pub dict_version: u64,
    /// The filter snapshot's version, 0 before one is sent.
    pub filter_version: u64,
    pub politeness: Politeness,
}

/// Why an `Assign` payload was refused. Nothing from it was used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignError {
    /// A version this build doesn't read.
    UnknownVersion(u8),
    /// The payload isn't a v1 payload's length.
    Length(usize),
    /// The message's revision and the payload's differ.
    RevisionMismatch,
    /// A field outside its range.
    Field(&'static str),
}

impl fmt::Display for AssignError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AssignError::UnknownVersion(v) => write!(f, "unknown assignment version {v}"),
            AssignError::Length(n) => {
                write!(f, "an assignment is {ASSIGN_V1_LEN} bytes, not {n}")
            }
            AssignError::RevisionMismatch => f.write_str("assignment revisions differ"),
            AssignError::Field(what) => write!(f, "invalid assignment field: {what}"),
        }
    }
}

impl std::error::Error for AssignError {}

impl Assignment {
    /// Checks every field the decoder checks.
    pub fn validate(&self) -> Result<(), AssignError> {
        let field = |ok: bool, what| {
            if ok {
                Ok(())
            } else {
                Err(AssignError::Field(what))
            }
        };
        field(self.revision != 0, "revision")?;
        field(
            (1..=1000).contains(&self.budget.cpu_permille),
            "cpu_permille",
        )?;
        field((1..=MAX_POOL_CAP).contains(&self.pool_cap), "pool_cap")?;
        field(self.politeness.valid(), "politeness")?;
        if let Some(s) = self.dht {
            field(ShardSpec::new(s.prefixes(), s.id_slots).is_some(), "shard")?;
        }
        Ok(())
    }

    /// The v1 bytes. `None` for an assignment that wouldn't decode.
    pub fn encode(&self) -> Option<Vec<u8>> {
        self.validate().ok()?;
        let mut out = Vec::with_capacity(ASSIGN_V1_LEN);
        out.push(ASSIGN_VERSION);
        out.extend_from_slice(&self.revision.to_le_bytes());
        let roles = if self.dht.is_some() { ROLE_DHT } else { 0 }
            | if self.fetch { ROLE_FETCH } else { 0 }
            | if self.prepare { ROLE_PREPARE } else { 0 };
        out.push(roles);
        let s = self.dht.unwrap_or(ShardSpec {
            start: 0,
            end: 0,
            id_slots: 0,
        });
        out.extend_from_slice(&s.start.to_le_bytes());
        out.extend_from_slice(&s.end.to_le_bytes());
        out.push(s.id_slots);
        out.push(self.intensity.code());
        out.extend_from_slice(&self.budget.cpu_permille.to_le_bytes());
        out.extend_from_slice(&self.budget.ram_mb.to_le_bytes());
        out.extend_from_slice(&self.budget.up_kbps.to_le_bytes());
        out.extend_from_slice(&self.budget.down_kbps.to_le_bytes());
        out.extend_from_slice(&self.pool_cap.to_le_bytes());
        out.extend_from_slice(&self.dict_version.to_le_bytes());
        out.extend_from_slice(&self.filter_version.to_le_bytes());
        let p = self.politeness;
        out.extend_from_slice(&p.peer_ip_groups.to_le_bytes());
        out.extend_from_slice(&p.peer_ip_group.to_le_bytes());
        out.push(p.per_peer_ip);
        out.push(p.bep51_interval_scale);
        debug_assert_eq!(out.len(), ASSIGN_V1_LEN);
        Some(out)
    }

    /// Decodes a payload, refusing an unknown version, a wrong length or
    /// any field out of range.
    pub fn decode(bytes: &[u8]) -> Result<Assignment, AssignError> {
        let Some(&version) = bytes.first() else {
            return Err(AssignError::Length(0));
        };
        if version != ASSIGN_VERSION {
            return Err(AssignError::UnknownVersion(version));
        }
        let b: &[u8; ASSIGN_V1_LEN] = bytes
            .try_into()
            .map_err(|_| AssignError::Length(bytes.len()))?;
        let mut r = Reader { b, at: 1 };
        let revision = r.u64();
        let roles = r.u8();
        let (start, end, id_slots) = (r.u16(), r.u16(), r.u8());
        let intensity = r.u8();
        let budget = Budget {
            cpu_permille: r.u16(),
            ram_mb: r.u32(),
            up_kbps: r.u32(),
            down_kbps: r.u32(),
        };
        let pool_cap = r.u32();
        let dict_version = r.u64();
        let filter_version = r.u64();
        let politeness = Politeness {
            peer_ip_groups: r.u16(),
            peer_ip_group: r.u16(),
            per_peer_ip: r.u8(),
            bep51_interval_scale: r.u8(),
        };
        debug_assert_eq!(r.at, ASSIGN_V1_LEN);

        if roles & !ROLES != 0 {
            return Err(AssignError::Field("roles"));
        }
        let dht = if roles & ROLE_DHT != 0 {
            Some(ShardSpec::new(start..end, id_slots).ok_or(AssignError::Field("shard"))?)
        } else if (start, end, id_slots) == (0, 0, 0) {
            None
        } else {
            return Err(AssignError::Field("shard"));
        };
        let a = Assignment {
            revision,
            dht,
            fetch: roles & ROLE_FETCH != 0,
            prepare: roles & ROLE_PREPARE != 0,
            intensity: Intensity::from_code(intensity).ok_or(AssignError::Field("intensity"))?,
            budget,
            pool_cap,
            dict_version,
            filter_version,
            politeness,
        };
        a.validate()?;
        Ok(a)
    }

    /// The protocol message. `None` for an assignment that wouldn't decode.
    pub fn to_message(&self) -> Option<proto::Assign> {
        Some(proto::Assign {
            revision: self.revision,
            payload: self.encode()?,
        })
    }

    /// The assignment in a message, or `None` for the empty greeting
    /// (revision 0, no payload: nothing assigned yet).
    pub fn from_message(m: &proto::Assign) -> Result<Option<Assignment>, AssignError> {
        if m.revision == 0 && m.payload.is_empty() {
            return Ok(None);
        }
        let a = Assignment::decode(&m.payload)?;
        if a.revision != m.revision {
            return Err(AssignError::RevisionMismatch);
        }
        Ok(Some(a))
    }
}

struct Reader<'a> {
    b: &'a [u8; ASSIGN_V1_LEN],
    at: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0; N];
        out.copy_from_slice(&self.b[self.at..self.at + N]);
        self.at += N;
        out
    }
    fn u8(&mut self) -> u8 {
        self.take::<1>()[0]
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take())
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }
    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.take())
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use proptest::prelude::*;

    use super::*;

    fn sample() -> Assignment {
        Assignment {
            revision: 7,
            dht: ShardSpec::new(64..128, 0b0101),
            fetch: true,
            prepare: true,
            intensity: Intensity::Aggressive,
            budget: Budget {
                cpu_permille: 500,
                ram_mb: 256,
                up_kbps: 1000,
                down_kbps: 0,
            },
            pool_cap: DEFAULT_POOL_CAP,
            dict_version: 3,
            filter_version: 9,
            politeness: Politeness {
                peer_ip_groups: 2,
                peer_ip_group: 1,
                per_peer_ip: 1,
                bep51_interval_scale: 3,
            },
        }
    }

    #[test]
    fn round_trips_and_has_the_v1_length() {
        let a = sample();
        let bytes = a.encode().unwrap();
        assert_eq!(bytes.len(), ASSIGN_V1_LEN);
        assert_eq!(bytes[0], ASSIGN_VERSION);
        assert_eq!(Assignment::decode(&bytes), Ok(a));
        let no_dht = Assignment { dht: None, ..a };
        assert_eq!(Assignment::decode(&no_dht.encode().unwrap()), Ok(no_dht));
        let m = a.to_message().unwrap();
        assert_eq!(m.revision, 7);
        assert_eq!(Assignment::from_message(&m), Ok(Some(a)));
    }

    #[test]
    fn refuses_unknown_versions_wrong_lengths_and_bad_fields() {
        let good = sample().encode().unwrap();
        assert_eq!(Assignment::decode(&[]), Err(AssignError::Length(0)));
        for v in [0, 2, 255] {
            let mut b = good.clone();
            b[0] = v;
            assert_eq!(Assignment::decode(&b), Err(AssignError::UnknownVersion(v)));
        }
        assert_eq!(
            Assignment::decode(&good[..55]),
            Err(AssignError::Length(55))
        );
        let mut long = good.clone();
        long.push(0);
        assert_eq!(Assignment::decode(&long), Err(AssignError::Length(57)));

        let bad = |at: usize, val: u8, what: &'static str| {
            let mut b = good.clone();
            b[at] = val;
            assert_eq!(
                Assignment::decode(&b),
                Err(AssignError::Field(what)),
                "{what}"
            );
        };
        bad(1, 0, "revision"); // low byte; the sample's revision is 7
        bad(9, 0x08, "roles");
        bad(10, 200, "shard"); // start 200 ≥ end 128
        bad(14, 0, "shard"); // no slot
        bad(15, 3, "intensity");
        bad(50, 1, "politeness"); // group 1 of 1 group
        for cpu in [0u16, 1001] {
            let mut b = good.clone();
            b[16..18].copy_from_slice(&cpu.to_le_bytes());
            assert_eq!(
                Assignment::decode(&b),
                Err(AssignError::Field("cpu_permille"))
            );
        }
        // A DHT-less payload must carry a zero shard.
        let mut b = Assignment {
            dht: None,
            ..sample()
        }
        .encode()
        .unwrap();
        b[14] = 1;
        assert_eq!(Assignment::decode(&b), Err(AssignError::Field("shard")));
        // pool cap 0 and over the maximum.
        for cap in [0u32, MAX_POOL_CAP + 1] {
            let mut b = good.clone();
            b[30..34].copy_from_slice(&cap.to_le_bytes());
            assert_eq!(Assignment::decode(&b), Err(AssignError::Field("pool_cap")));
        }
        // per_peer_ip above one install's, and a zero interval scale.
        for (at, val) in [(54, INSTALL_PER_PEER_IP + 1), (54, 0), (55, 0)] {
            let mut b = good.clone();
            b[at] = val;
            assert_eq!(
                Assignment::decode(&b),
                Err(AssignError::Field("politeness"))
            );
        }
    }

    #[test]
    fn message_revision_must_match_and_the_greeting_is_nothing() {
        assert_eq!(
            Assignment::from_message(&proto::Assign::default()),
            Ok(None)
        );
        let mut m = sample().to_message().unwrap();
        m.revision = 8;
        assert_eq!(
            Assignment::from_message(&m),
            Err(AssignError::RevisionMismatch)
        );
        let empty_at_rev = proto::Assign {
            revision: 3,
            payload: vec![],
        };
        assert_eq!(
            Assignment::from_message(&empty_at_rev),
            Err(AssignError::Length(0))
        );
    }

    #[test]
    fn an_invalid_assignment_doesnt_encode() {
        assert_eq!(
            Assignment {
                revision: 0,
                ..sample()
            }
            .encode(),
            None
        );
        let mut p = sample();
        p.politeness.peer_ip_group = 2;
        assert!(p.to_message().is_none());
    }

    #[test]
    fn buckets_are_stable_and_mapped_addresses_agree() {
        let v4 = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9));
        let mapped = IpAddr::V6(Ipv4Addr::new(203, 0, 113, 9).to_ipv6_mapped());
        for g in 1..20 {
            assert_eq!(peer_ip_bucket(v4, g), peer_ip_bucket(mapped, g));
            assert!(peer_ip_bucket(v4, g) < g);
        }
        assert_eq!(peer_ip_bucket(IpAddr::V6(Ipv6Addr::LOCALHOST), 0), 0);
        assert_eq!(Politeness::ALONE.connections_to(v4), INSTALL_PER_PEER_IP);
    }

    proptest! {
        #[test]
        fn decode_never_panics_and_what_decodes_round_trips(
            bytes in proptest::collection::vec(any::<u8>(), 0..80),
        ) {
            if let Ok(a) = Assignment::decode(&bytes) {
                prop_assert_eq!(a.encode(), Some(bytes.clone()));
            }
        }

        #[test]
        fn mutated_payloads_decode_to_valid_assignments_or_nothing(
            at in 0..ASSIGN_V1_LEN, val in any::<u8>(),
        ) {
            let mut b = sample().encode().unwrap();
            b[at] = val;
            if let Ok(a) = Assignment::decode(&b) {
                prop_assert!(a.validate().is_ok());
                prop_assert_eq!(a.encode().unwrap(), b);
            }
        }
    }
}
