//! CR-1b's target prefixes (`dht.target_prefixes`, off by default).
//!
//! With the switch on, each random routing target the sampler draws (the
//! `sample_infohashes` target and the exploration `find_node`) takes its
//! first byte from a cursor over the 256 eight-bit prefixes, and its
//! other 19 bytes from the actor's RNG. Every address family and each
//! [`TargetKind`] has its own cursor, so one doesn't skew another. A
//! cursor moves only when its query was actually sent, so a throttled,
//! refused or paused tick doesn't skip a prefix.
//!
//! A pass visits the prefixes in bit-reversed order (0, 128, 64, 192, …),
//! so consecutive targets land far apart in the keyspace instead of in
//! neighbouring regions whose closest nodes overlap. Every prefix is
//! still used exactly once per pass.
//!
//! WK-4: a keyspace shard narrows the cursor to its sub-range of
//! prefixes. The pass keeps the same bit-reversed order and skips the
//! positions whose prefix is outside the range, so each prefix in range is
//! still used exactly once per pass, and the full range is the cursor
//! above, step for step.
//!
//! A target steers which routing contacts come back, not which hashes a
//! node samples (BEP 51). A full pass makes no claim that the keyspace
//! was covered.

use std::ops::Range;

use crate::compact::NodeId;

use super::TargetKind;

/// One cursor per address family and [`TargetKind`].
#[derive(Debug)]
pub(crate) struct Prefixes {
    cursors: Vec<[u8; TargetKind::ALL.len()]>,
    /// The prefixes the cursor may use (WK-4): non-empty, within 0..256.
    range: Range<u16>,
}

impl Prefixes {
    pub fn new(families: usize) -> Prefixes {
        Prefixes::in_range(families, 0..256)
    }

    /// A cursor over `range` only (a [`super::shard::Shard`]'s prefixes,
    /// so non-empty and within 0..256). Each cursor starts at the pass's
    /// first position in range.
    pub fn in_range(families: usize, range: Range<u16>) -> Prefixes {
        assert!(
            range.start < range.end && range.end <= 256,
            "a non-empty range of the 256 prefixes"
        );
        let first = (0..=255u8)
            .find(|p| range.contains(&u16::from(p.reverse_bits())))
            .expect("a non-empty range has a first position");
        Prefixes {
            cursors: vec![[first; TargetKind::ALL.len()]; families],
            range,
        }
    }

    /// The target the cursor stands at: its position's prefix (the
    /// position, bit-reversed), then `suffix`.
    pub fn target(&self, family: usize, kind: TargetKind, suffix: [u8; 19]) -> NodeId {
        let mut id = [0u8; 20];
        id[0] = self.cursors[family][kind as usize].reverse_bits();
        id[1..].copy_from_slice(&suffix);
        NodeId(id)
    }

    /// Moves the cursor past the prefix just used to the next position in
    /// range. True when that was the pass's last position in range (255,
    /// prefix 255, for the full range), completing a pass; the cursor wraps
    /// to the pass's first position in range.
    pub fn advance(&mut self, family: usize, kind: TargetKind) -> bool {
        let range = self.range.clone();
        let c = &mut self.cursors[family][kind as usize];
        let mut wrapped = false;
        // At most 256 steps: the range holds at least one prefix.
        loop {
            let (next, w) = c.overflowing_add(1);
            *c = next;
            wrapped |= w;
            if range.contains(&u16::from(next.reverse_bits())) {
                return wrapped;
            }
        }
    }

    /// Test seam: put a cursor at `position` in its pass.
    #[cfg(test)]
    pub fn set(&mut self, family: usize, kind: TargetKind, position: u8) {
        self.cursors[family][kind as usize] = position;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cursor_walks_every_prefix_once_per_pass_and_wraps() {
        let mut p = Prefixes::new(1);
        let mut cycles = 0;
        let mut seen = Vec::new();
        for _ in 0..512 {
            let t = p.target(0, TargetKind::Sample, [7; 19]);
            assert_eq!(&t.0[1..], &[7; 19], "the suffix is the caller's");
            seen.push(t.0[0]);
            if p.advance(0, TargetKind::Sample) {
                cycles += 1;
            }
        }
        assert_eq!(seen[..4], [0, 128, 64, 192], "bit-reversed order");
        let mut pass = seen[..256].to_vec();
        pass.sort_unstable();
        assert_eq!(pass, (0..=255).collect::<Vec<u8>>(), "each prefix once");
        assert_eq!(
            seen[256..],
            seen[..256],
            "the second pass repeats the first"
        );
        assert!(
            seen.windows(2).all(|w| (w[0] ^ w[1]) & 0x80 != 0),
            "consecutive targets are in opposite halves of the keyspace"
        );
        assert_eq!(cycles, 2);
        assert_eq!(p.target(0, TargetKind::Sample, [0; 19]).0[0], 0);
    }

    #[test]
    fn families_and_kinds_keep_independent_cursors() {
        let mut p = Prefixes::new(2);
        for _ in 0..5 {
            p.advance(0, TargetKind::Sample);
        }
        p.advance(1, TargetKind::Explore);
        let at = |p: &Prefixes, f, k| p.target(f, k, [0; 19]).0[0].reverse_bits();
        assert_eq!(at(&p, 0, TargetKind::Sample), 5);
        assert_eq!(at(&p, 0, TargetKind::Explore), 0);
        assert_eq!(at(&p, 1, TargetKind::Sample), 0);
        assert_eq!(at(&p, 1, TargetKind::Explore), 1);
    }

    /// One pass of `p`'s Sample cursor in family 0, from where it stands to
    /// the pass's end: its prefixes in order.
    fn one_pass(p: &mut Prefixes) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            out.push(p.target(0, TargetKind::Sample, [0; 19]).0[0]);
            if p.advance(0, TargetKind::Sample) {
                return out;
            }
            assert!(out.len() <= 256, "a pass ends within 256 targets");
        }
    }

    #[test]
    fn a_sub_range_pass_uses_each_prefix_in_range_once_in_bit_reversed_order() {
        for range in [0..256u16, 0..128, 128..256, 10..11, 255..256, 0..1, 37..201] {
            let mut p = Prefixes::in_range(1, range.clone());
            let pass = one_pass(&mut p);
            let mut sorted = pass.clone();
            sorted.sort_unstable();
            let want: Vec<u8> = range.clone().map(|v| v as u8).collect();
            assert_eq!(sorted, want, "{range:?}: each prefix in range once");
            // The same relative order as the full pass.
            let full: Vec<u8> = (0..=255u8)
                .map(u8::reverse_bits)
                .filter(|v| range.contains(&u16::from(*v)))
                .collect();
            assert_eq!(pass, full, "{range:?}: the full pass's order, filtered");
            assert_eq!(
                one_pass(&mut p),
                pass,
                "{range:?}: the next pass repeats it"
            );
        }
    }

    #[test]
    fn the_full_range_is_the_unsharded_cursor_step_for_step() {
        let mut a = Prefixes::new(2);
        let mut b = Prefixes::in_range(2, 0..256);
        for i in 0..600 {
            let kind = TargetKind::ALL[i % 2];
            let f = (i / 3) % 2;
            assert_eq!(a.target(f, kind, [9; 19]), b.target(f, kind, [9; 19]));
            assert_eq!(a.advance(f, kind), b.advance(f, kind));
        }
    }

    #[test]
    #[should_panic(expected = "a non-empty range")]
    fn an_empty_range_is_refused() {
        let _ = Prefixes::in_range(1, 4..4);
    }

    #[test]
    fn reading_a_target_does_not_move_the_cursor() {
        let p = Prefixes::new(1);
        for _ in 0..3 {
            assert_eq!(p.target(0, TargetKind::Explore, [1; 19]).0[0], 0);
        }
    }
}
