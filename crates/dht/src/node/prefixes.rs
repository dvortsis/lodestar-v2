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
//! A target steers which routing contacts come back, not which hashes a
//! node samples (BEP 51). A full pass makes no claim that the keyspace
//! was covered.

use crate::compact::NodeId;

use super::TargetKind;

/// One cursor per address family and [`TargetKind`].
#[derive(Debug)]
pub(crate) struct Prefixes {
    cursors: Vec<[u8; TargetKind::ALL.len()]>,
}

impl Prefixes {
    pub fn new(families: usize) -> Prefixes {
        Prefixes {
            cursors: vec![[0; TargetKind::ALL.len()]; families],
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

    /// Moves the cursor past the prefix just used. True when that was the
    /// pass's last position (255, prefix 255), completing a pass; the
    /// cursor wraps to position 0.
    pub fn advance(&mut self, family: usize, kind: TargetKind) -> bool {
        let c = &mut self.cursors[family][kind as usize];
        let (next, wrapped) = c.overflowing_add(1);
        *c = next;
        wrapped
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

    #[test]
    fn reading_a_target_does_not_move_the_cursor() {
        let p = Prefixes::new(1);
        for _ in 0..3 {
            assert_eq!(p.target(0, TargetKind::Explore, [1; 19]).0[0], 0);
        }
    }
}
