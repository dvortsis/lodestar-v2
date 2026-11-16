//! Cursors (architecture §8, "Stable pagination"): where the next page of
//! a search starts, inside the snapshot a searcher lease pins.
//!
//! A position is the sort key of the last hit served plus its document
//! address as the tie-break, so "after it" is exact within a snapshot:
//! every document is either before or after, never both.
//!
//! **Two-phase relevance across windows (§8):** results come in windows
//! of [`RERANK_WINDOW`](crate::index::RERANK_WINDOW) phase-1 candidates,
//! each re-ranked with the full features. A window is "the best 200 by
//! phase-1 score after the previous window's last candidate", so a cursor
//! carries that bound and the last re-ranked hit within the window. Order
//! is exact inside a window. A document that would re-rank above the
//! window boundary stays in its own window: the boundary is where the
//! order is approximate, as §8 says, and it's the same on every page, so
//! nothing is served twice or skipped.
//!
//! **Collapsed pages (GS-3):** a cursor of a collapsed search (one row
//! per group, `collapse.rs`) carries the same position (the last row
//! served) and a collapse flag. Nothing else: a later page skips every
//! group with a member ranking at or before the position, so no set of
//! groups seen grows with depth. Such a cursor is written as version 2;
//! an uncollapsed one stays exactly version 1, so cursors written before
//! GS-3 still open.
//!
//! [`Cursor::to_bytes`] is the payload; the API authenticates it (an
//! HMAC) and encodes it for URLs.

use tantivy::DocAddress;

use crate::index::Sort;

/// A searcher lease's id: 128 random bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LeaseId(pub [u8; 16]);

/// Where the next page starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Position {
    /// Relevance without re-ranking: after this score and address.
    Score { score: f32, addr: DocAddress },
    /// Two-phase relevance: the window of phase-1 candidates after
    /// `start` (`None`: the first window), after its re-ranked hit `last`
    /// (`None`: from the window's top).
    Window {
        start: Option<(f32, DocAddress)>,
        last: Option<(f32, DocAddress)>,
    },
    /// A fast-field sort: after this (has a value, ordered key, address),
    /// the order of `sorted.rs`.
    Field {
        present: bool,
        key: u64,
        addr: DocAddress,
    },
}

/// A cursor's contents.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cursor {
    pub lease: LeaseId,
    /// Identifies the query (and its options) the cursor continues; the
    /// API compares it with the request's.
    pub fingerprint: [u8; 8],
    pub sort: Sort,
    /// Rows served before this cursor's page: its offset (groups, when
    /// collapsed).
    pub served: u64,
    pub position: Position,
    /// The search shows one row per group (GS-3).
    pub collapse: bool,
}

/// An uncollapsed cursor's version, as before GS-3.
const VERSION: u8 = 1;
/// A collapsed cursor's.
const VERSION_COLLAPSED: u8 = 2;

fn sort_code(sort: Sort) -> u8 {
    match sort {
        Sort::Relevance => 0,
        Sort::Discovered { desc } => 2 | u8::from(desc),
        Sort::Size { desc } => 4 | u8::from(desc),
        Sort::Files { desc } => 6 | u8::from(desc),
        Sort::Seeders { desc } => 8 | u8::from(desc),
        Sort::Leechers { desc } => 10 | u8::from(desc),
        Sort::Trackers { desc } => 12 | u8::from(desc),
        Sort::Seen { desc } => 14 | u8::from(desc),
    }
}

fn sort_of(code: u8) -> Option<Sort> {
    let desc = code & 1 == 1;
    Some(match code & !1 {
        0 if !desc => Sort::Relevance,
        2 => Sort::Discovered { desc },
        4 => Sort::Size { desc },
        6 => Sort::Files { desc },
        8 => Sort::Seeders { desc },
        10 => Sort::Leechers { desc },
        12 => Sort::Trackers { desc },
        14 => Sort::Seen { desc },
        _ => return None,
    })
}

fn put_addr(out: &mut Vec<u8>, a: DocAddress) {
    out.extend_from_slice(&a.segment_ord.to_le_bytes());
    out.extend_from_slice(&a.doc_id.to_le_bytes());
}

fn put_scored(out: &mut Vec<u8>, s: Option<(f32, DocAddress)>) {
    match s {
        None => out.push(0),
        Some((score, addr)) => {
            out.push(1);
            out.extend_from_slice(&score.to_bits().to_le_bytes());
            put_addr(out, addr);
        }
    }
}

/// Reads fixed-size fields off the front of a slice.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*head)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take::<1>().map(|b| b[0])
    }

    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Option<u64> {
        self.take().map(u64::from_le_bytes)
    }

    /// Scores are finite: a NaN or infinity was never served.
    fn score(&mut self) -> Option<f32> {
        Some(f32::from_bits(self.u32()?)).filter(|s| s.is_finite())
    }

    fn addr(&mut self) -> Option<DocAddress> {
        Some(DocAddress::new(self.u32()?, self.u32()?))
    }

    fn scored(&mut self) -> Option<Option<(f32, DocAddress)>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some((self.score()?, self.addr()?))),
            _ => None,
        }
    }
}

impl Cursor {
    /// At most 61 bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(61);
        out.push(if self.collapse {
            VERSION_COLLAPSED
        } else {
            VERSION
        });
        out.extend_from_slice(&self.lease.0);
        out.extend_from_slice(&self.fingerprint);
        out.push(sort_code(self.sort));
        out.extend_from_slice(&self.served.to_le_bytes());
        match self.position {
            Position::Score { score, addr } => {
                out.push(0);
                out.extend_from_slice(&score.to_bits().to_le_bytes());
                put_addr(&mut out, addr);
            }
            Position::Window { start, last } => {
                out.push(1);
                put_scored(&mut out, start);
                put_scored(&mut out, last);
            }
            Position::Field { present, key, addr } => {
                out.push(2);
                out.push(u8::from(present));
                out.extend_from_slice(&key.to_le_bytes());
                put_addr(&mut out, addr);
            }
        }
        out
    }

    /// The cursor in `bytes`, or `None` unless they're exactly one this
    /// build writes.
    pub fn from_bytes(bytes: &[u8]) -> Option<Cursor> {
        let mut r = Reader(bytes);
        let collapse = match r.u8()? {
            VERSION => false,
            VERSION_COLLAPSED => true,
            _ => return None,
        };
        let lease = LeaseId(r.take()?);
        let fingerprint = r.take()?;
        let sort = sort_of(r.u8()?)?;
        let served = r.u64()?;
        let position = match r.u8()? {
            0 => Position::Score {
                score: r.score()?,
                addr: r.addr()?,
            },
            1 => Position::Window {
                start: r.scored()?,
                last: r.scored()?,
            },
            2 => Position::Field {
                present: match r.u8()? {
                    0 => false,
                    1 => true,
                    _ => return None,
                },
                key: r.u64()?,
                addr: r.addr()?,
            },
            _ => return None,
        };
        // The position must fit the sort.
        let fits = matches!(
            (sort, position),
            (
                Sort::Relevance,
                Position::Score { .. } | Position::Window { .. }
            ) | (
                Sort::Discovered { .. }
                    | Sort::Size { .. }
                    | Sort::Files { .. }
                    | Sort::Seeders { .. }
                    | Sort::Leechers { .. }
                    | Sort::Trackers { .. }
                    | Sort::Seen { .. },
                Position::Field { .. }
            )
        );
        (fits && r.0.is_empty()).then_some(Cursor {
            lease,
            fingerprint,
            sort,
            served,
            position,
            collapse,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn addr() -> impl Strategy<Value = DocAddress> {
        (any::<u32>(), any::<u32>()).prop_map(|(s, d)| DocAddress::new(s, d))
    }

    fn scored() -> impl Strategy<Value = Option<(f32, DocAddress)>> {
        proptest::option::of((-1e30f32..1e30, addr()))
    }

    fn sorted() -> impl Strategy<Value = Sort> {
        prop_oneof![
            any::<bool>().prop_map(|desc| Sort::Discovered { desc }),
            any::<bool>().prop_map(|desc| Sort::Size { desc }),
            any::<bool>().prop_map(|desc| Sort::Files { desc }),
            any::<bool>().prop_map(|desc| Sort::Seeders { desc }),
            any::<bool>().prop_map(|desc| Sort::Leechers { desc }),
            any::<bool>().prop_map(|desc| Sort::Trackers { desc }),
            any::<bool>().prop_map(|desc| Sort::Seen { desc }),
        ]
    }

    fn cursor() -> impl Strategy<Value = Cursor> {
        let head = (any::<[u8; 16]>(), any::<[u8; 8]>(), any::<u64>());
        let relevance = (
            head,
            prop_oneof![
                (-1e30f32..1e30, addr()).prop_map(|(score, addr)| Position::Score { score, addr }),
                (scored(), scored()).prop_map(|(start, last)| Position::Window { start, last }),
            ],
        )
            .prop_map(|(h, p)| (h, Sort::Relevance, p));
        let field = (
            head,
            sorted(),
            (any::<bool>(), any::<u64>(), addr())
                .prop_map(|(present, key, addr)| Position::Field { present, key, addr }),
        );
        (prop_oneof![relevance, field], any::<bool>()).prop_map(
            |(((lease, fingerprint, served), sort, position), collapse)| Cursor {
                lease: LeaseId(lease),
                fingerprint,
                sort,
                served,
                position,
                collapse,
            },
        )
    }

    proptest! {
        #[test]
        fn cursors_round_trip_in_at_most_61_bytes(c in cursor()) {
            let b = c.to_bytes();
            prop_assert!(b.len() <= 61, "{} bytes", b.len());
            prop_assert_eq!(Cursor::from_bytes(&b), Some(c));
        }

        #[test]
        fn any_bytes_parse_or_are_refused_without_panicking(b in proptest::collection::vec(any::<u8>(), 0..80)) {
            if let Some(c) = Cursor::from_bytes(&b) {
                prop_assert_eq!(c.to_bytes(), b, "only canonical bytes parse");
            }
        }

        #[test]
        fn truncated_or_extended_cursors_are_refused(c in cursor(), cut in 1usize..61, extra in any::<u8>()) {
            let b = c.to_bytes();
            let cut = cut.min(b.len());
            prop_assert_eq!(Cursor::from_bytes(&b[..b.len() - cut]), None);
            let mut longer = b.clone();
            longer.push(extra);
            prop_assert_eq!(Cursor::from_bytes(&longer), None);
        }
    }

    #[test]
    fn a_position_must_fit_its_sort_and_scores_be_finite() {
        let c = Cursor {
            lease: LeaseId([1; 16]),
            fingerprint: [2; 8],
            sort: Sort::Relevance,
            served: 50,
            position: Position::Score {
                score: 1.5,
                addr: DocAddress::new(0, 3),
            },
            collapse: false,
        };
        let mut b = c.to_bytes();
        // Sort byte (after version, lease and fingerprint) → size desc.
        b[25] = 5;
        assert_eq!(Cursor::from_bytes(&b), None);
        let nan = Cursor {
            position: Position::Score {
                score: f32::NAN,
                addr: DocAddress::new(0, 3),
            },
            ..c
        };
        assert_eq!(Cursor::from_bytes(&nan.to_bytes()), None);
        // Relevance with the descending bit set isn't a sort.
        let mut b = c.to_bytes();
        b[25] = 1;
        assert_eq!(Cursor::from_bytes(&b), None);
    }

    /// A cursor written before GS-3 (version 1, these exact bytes) still
    /// opens, uncollapsed; collapse only changes the version byte, and an
    /// unknown version is refused.
    #[test]
    fn version_one_cursors_still_open_and_collapse_is_version_two() {
        let mut v1 = vec![1u8];
        v1.extend_from_slice(&[7; 16]);
        v1.extend_from_slice(&[9; 8]);
        v1.push(9); // seeders, descending
        v1.extend_from_slice(&50u64.to_le_bytes());
        v1.push(2);
        v1.push(1);
        v1.extend_from_slice(&123u64.to_le_bytes());
        v1.extend_from_slice(&4u32.to_le_bytes());
        v1.extend_from_slice(&5u32.to_le_bytes());
        let c = Cursor::from_bytes(&v1).expect("a v1 cursor");
        assert!(!c.collapse);
        assert_eq!(c.sort, Sort::Seeders { desc: true });
        assert_eq!(
            c.to_bytes(),
            v1,
            "uncollapsed cursors are written as before"
        );
        let collapsed = Cursor {
            collapse: true,
            ..c
        };
        let b = collapsed.to_bytes();
        assert_eq!(b[0], 2);
        assert_eq!(&b[1..], &v1[1..]);
        assert_eq!(Cursor::from_bytes(&b), Some(collapsed));
        let mut v3 = v1.clone();
        v3[0] = 3;
        assert_eq!(Cursor::from_bytes(&v3), None);
    }
}
