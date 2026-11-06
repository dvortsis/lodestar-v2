//! WK-4's keyspace shard (§19.5 `sharded(keyspace)`): the part of the DHT
//! one node of a fleet walks, so walks from workers that share an
//! external IP don't overlap.
//!
//! A shard has two halves:
//! - **A target-prefix sub-range**, `start..end` of the 256 eight-bit
//!   prefixes. CR-1b's cursor ([`super::prefixes`]) walks only these, so
//!   the BEP 51 sample and exploration targets stay inside it. A sub-range
//!   shard uses the cursor whether or not `dht.target_prefixes` is on: the
//!   cursor is how a shard's walk stays in range.
//! - **A subset of the 8 BEP 42 id slots** (a bitset; bit `s` is slot
//!   `s`, the low 3 bits of `r` in an id's last byte). The node's
//!   virtual ids use only these slots. For one external IP, distinct slots
//!   always give distinct 21-bit BEP 42 prefixes
//!   ([`crate::secure_id::virtual_ids_in`]), so disjoint slot sets never
//!   mint the same prefix.
//!
//! The full shard (every prefix, every slot) is no shard: the actor treats
//! it as `None`, byte for byte. Any other shard is a *sub-range* shard and
//! caps the node's ids at its number of slots, since ids beyond that would
//! share a slot's prefix. Per-node BEP 51 intervals and per-IP limits
//! don't change; dividing them among workers behind one IP is the
//! planner's job (WK-5).

use std::fmt;
use std::ops::Range;

pub use crate::secure_id::ALL_SLOTS;

/// A keyspace shard. Built only through [`Shard::new`], so it's never
/// empty and its range never leaves the 256 prefixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Shard {
    start: u16,
    end: u16,
    id_slots: u8,
}

/// Why [`Shard::new`] refused its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShardError {
    /// The prefix range is empty or reaches past 256.
    Prefixes { start: u16, end: u16 },
    /// No id slot is set.
    NoSlots,
}

impl fmt::Display for ShardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShardError::Prefixes { start, end } => write!(
                f,
                "shard prefixes {start}..{end} must be a non-empty range within 0..256"
            ),
            ShardError::NoSlots => f.write_str("a shard needs at least one BEP 42 id slot"),
        }
    }
}

impl std::error::Error for ShardError {}

impl Shard {
    /// The whole keyspace: every prefix and every id slot. Equal to no
    /// shard.
    pub const FULL: Shard = Shard {
        start: 0,
        end: 256,
        id_slots: ALL_SLOTS,
    };

    /// A shard over target prefixes `prefixes` (a non-empty range within
    /// `0..256`) and the id slots set in `id_slots` (at least one).
    pub fn new(prefixes: Range<u16>, id_slots: u8) -> Result<Shard, ShardError> {
        let Range { start, end } = prefixes;
        if start >= end || end > 256 {
            return Err(ShardError::Prefixes { start, end });
        }
        if id_slots == 0 {
            return Err(ShardError::NoSlots);
        }
        Ok(Shard {
            start,
            end,
            id_slots,
        })
    }

    /// The target prefixes, `start..end` of 256.
    pub fn prefixes(&self) -> Range<u16> {
        self.start..self.end
    }

    /// The id-slot bitset.
    pub fn id_slots(&self) -> u8 {
        self.id_slots
    }

    /// How many id slots the shard has (1–8).
    pub fn slot_count(&self) -> usize {
        self.id_slots.count_ones() as usize
    }

    /// Whether this is [`Shard::FULL`].
    pub fn is_full(&self) -> bool {
        *self == Shard::FULL
    }

    /// Whether `prefix` (an id's first byte) is in the shard's range.
    pub fn has_prefix(&self, prefix: u8) -> bool {
        (self.start..self.end).contains(&u16::from(prefix))
    }

    /// Whether BEP 42 slot `slot` (taken mod 8, as `r`'s low 3 bits are)
    /// is in the shard.
    pub fn has_slot(&self, slot: u8) -> bool {
        self.id_slots & (1 << (slot & 7)) != 0
    }

    /// How many virtual ids a node asked for `want` runs: `want` for the
    /// full shard, else at most one per slot.
    pub fn id_cap(&self, want: usize) -> usize {
        if self.is_full() {
            want
        } else {
            want.min(self.slot_count())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_refuses_empty_or_out_of_range_prefixes_and_no_slots() {
        for (start, end) in [(0, 0), (5, 5), (9, 3), (0, 257), (256, 257), (300, 400)] {
            assert_eq!(
                Shard::new(start..end, 1),
                Err(ShardError::Prefixes { start, end }),
                "{start}..{end}"
            );
        }
        assert_eq!(Shard::new(0..256, 0), Err(ShardError::NoSlots));
        assert!(Shard::new(255..256, 0x80).is_ok());
        assert!(Shard::new(0..1, 1).is_ok());
        assert_eq!(Shard::new(0..256, ALL_SLOTS), Ok(Shard::FULL));
    }

    #[test]
    fn membership_and_caps() {
        let s = Shard::new(64..128, 0b0000_0101).unwrap();
        assert!(!s.is_full());
        assert_eq!(s.prefixes(), 64..128);
        assert_eq!(s.id_slots(), 5);
        assert_eq!(s.slot_count(), 2);
        assert!(!s.has_prefix(63) && s.has_prefix(64) && s.has_prefix(127) && !s.has_prefix(128));
        assert!(s.has_slot(0) && !s.has_slot(1) && s.has_slot(2) && s.has_slot(8));
        assert_eq!(
            s.id_cap(4),
            2,
            "a sub-range shard caps the ids at its slots"
        );
        assert_eq!(s.id_cap(1), 1);
        assert_eq!(Shard::FULL.id_cap(16), 16, "the full shard doesn't");
        assert!(Shard::FULL.has_prefix(0) && Shard::FULL.has_prefix(255));
        // Every slot but a narrower range is still a sub-range shard.
        assert_eq!(Shard::new(0..255, ALL_SLOTS).unwrap().id_cap(16), 8);
        assert_eq!(Shard::new(0..256, 0x7f).unwrap().id_cap(16), 7);
    }

    #[test]
    fn errors_read_plainly() {
        assert_eq!(
            Shard::new(Range { start: 3, end: 1 }, 1)
                .unwrap_err()
                .to_string(),
            "shard prefixes 3..1 must be a non-empty range within 0..256"
        );
        assert_eq!(
            ShardError::NoSlots.to_string(),
            "a shard needs at least one BEP 42 id slot"
        );
    }
}
