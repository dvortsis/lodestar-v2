//! Per-field population: the contract SB-6 sets for every field v6 adds
//! (build-plan §0.2 Track M7, "population contract (r3)").
//!
//! A field added by a schema bump is empty on every document indexed
//! before its producer ships: an existing current-version index is
//! opened as it is, and the live indexer only visits new ids and
//! `reindex_log` entries. So each tracked field has a **generation**:
//!
//! - this build's, in [`GENERATIONS`]: 0 while no producer fills it, and
//!   a producer slice bumps it by one when it ships (or changes what it
//!   writes);
//! - the index's, recorded per field in each index's Tantivy commit
//!   payload, atomically with the documents it describes.
//!
//! When the two differ, the index **backfills** the field: a walk over
//! the torrent ids the index held when it noticed (`(cursor, high]`),
//! re-indexing each from the store in bounded batches, the same way a
//! `reindex_log` entry is replayed. Torrents indexed after that already
//! carry this build's fields. The cursor commits with the documents, so
//! a restart resumes the walk where its last commit left it; a torrent
//! deleted or changed meanwhile is read as it is now (deleted ones are
//! dropped, changed ones also get their own `reindex_log` replay). The
//! walk is **complete** once the cursor reaches `high`; only then is the
//! field [`Availability::Ready`].
//!
//! Until then queries are told the truth ([`Availability`]): a clause on
//! a field being backfilled matches only the documents done so far, and
//! a prefilter on it must not run at all
//! ([`SearchIndex::prefilter`](crate::SearchIndex::prefilter)), since a
//! document it hasn't reached would look like "no match".

use std::collections::BTreeMap;

/// The two indexes a tracked field can live in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Torrents,
    /// File-index fields are named `file.<name>`, as
    /// [`SearchIndex::missing_fields`](crate::SearchIndex::missing_fields)
    /// names them.
    Files,
}

impl Side {
    fn of(field: &str) -> Side {
        if field.starts_with(FILE_PREFIX) {
            Side::Files
        } else {
            Side::Torrents
        }
    }
}

const FILE_PREFIX: &str = "file.";

/// This build's generation of every tracked field (the fields v6 added).
/// 0: no producer yet, so every document lacks it. SB-6's own rebuild
/// fills the file index's fields from the store, so they start at 1.
///
/// A producer slice raises its field's number when it ships. Every
/// existing index then backfills that field, and its acceptance tests
/// run SB-6 → import → producer on an existing database, including a
/// restart mid-backfill.
pub const GENERATIONS: [(&str, u32); 13] = [
    // SRC-3 (§18.6): every torrent's provenance.
    ("verified", 1),
    ("source", 1),
    ("network", 1),
    // GS-0: `lodestar_classify::payload::payload_sig`, the hash CS-8a reads.
    ("payload_sig", 1),
    // GS-2: `group_key` is `title_identity`'s value in the fast column
    // (one identity rule, plan Q8), so a change to
    // `TitleIdentityParts::key` bumps both; `group_minhash` is
    // `lodestar_classify::group::band_terms` (plan Q9).
    ("group_key", 1),
    ("group_minhash", 1),
    ("name_trigram", 0),
    ("file.size_bucket", 1),
    ("file.category", 1),
    ("file.pieces_root", 1),
    // X-search-files-5 (v7): filled by the rebuild that makes v7. A
    // change to `files::size_code` or `covered_buckets` must bump the two
    // bucket fields, so an index answers from them only once rewritten.
    // Generation 2 (X-search-files-6): finer codes, with decimal floors.
    ("file_bucket", 2),
    ("file_ext_bucket", 2),
    ("title_identity", 1),
];

/// The generations an index is built to: [`GENERATIONS`], or another
/// set for tests that play a producer shipping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generations(BTreeMap<String, u32>);

impl Default for Generations {
    fn default() -> Self {
        Generations::current()
    }
}

impl Generations {
    /// This build's.
    pub fn current() -> Generations {
        Generations(
            GENERATIONS
                .iter()
                .map(|(f, g)| ((*f).to_owned(), *g))
                .collect(),
        )
    }

    /// The same with `field` at `generation`, as when its producer ships.
    ///
    /// # Panics
    /// When `field` isn't tracked: a typo would test nothing.
    #[must_use]
    pub fn with(mut self, field: &str, generation: u32) -> Generations {
        let g = self
            .0
            .get_mut(field)
            .unwrap_or_else(|| panic!("{field} isn't a tracked field"));
        *g = generation;
        self
    }

    /// `field`'s generation; `None` when it isn't tracked.
    pub fn get(&self, field: &str) -> Option<u32> {
        self.0.get(field).copied()
    }

    fn side(&self, side: Side) -> impl Iterator<Item = (&str, u32)> {
        self.0
            .iter()
            .filter(move |(f, _)| Side::of(f) == side)
            .map(|(f, g)| (f.as_str(), *g))
    }
}

/// How much of a field the serving index can answer for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// Every document carries it when it applies (a v1 torrent's files
    /// simply have no `pieces_root`).
    Ready,
    /// No producer fills it yet: every document lacks it.
    NoProducer,
    /// A backfill is filling it: torrents with ids up to `done` have it;
    /// those in `(done, upto]` may not yet. Torrents past `upto` do.
    Backfilling { done: i64, upto: i64 },
    /// The serving index doesn't have the field at all (an older index
    /// serving during a rebuild): clauses on it match nothing.
    Missing,
}

impl Availability {
    /// Whether a clause on the field can be trusted to find every match,
    /// so a prefilter on it may exclude what it doesn't match.
    pub fn complete(self) -> bool {
        self == Availability::Ready
    }
}

/// A field that isn't [`Availability::Ready`], for the "not yet
/// available" notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldAvailability {
    pub field: String,
    pub availability: Availability,
}

/// A backfill walk: torrents with ids in `(cursor, high]` may predate
/// the field's generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Walk {
    pub cursor: i64,
    pub high: i64,
}

/// What an index records for one tracked field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recorded {
    /// The generation its documents are being (or have been) built to.
    pub generation: u32,
    /// The backfill still to do; `None` when complete.
    pub walk: Option<Walk>,
}

/// One index's population state: what its commit payload records for
/// the tracked fields on its side.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PopState(BTreeMap<String, Recorded>);

impl PopState {
    /// A new, empty index: everything it will ever hold is indexed by
    /// this build, so every field is complete at this build's generation.
    pub fn fresh(gens: &Generations, side: Side) -> PopState {
        PopState(
            gens.side(side)
                .map(|(f, g)| {
                    (
                        f.to_owned(),
                        Recorded {
                            generation: g,
                            walk: None,
                        },
                    )
                })
                .collect(),
        )
    }

    /// Brings what an index recorded up to `gens`: a field at another
    /// generation (or not recorded at all) starts a walk over the
    /// torrents up to `last_id`, the highest the index holds, unless no
    /// producer fills it (generation 0), in which case there's nothing
    /// to write. A walk already under way for the same generation goes
    /// on from its cursor. Fields this build doesn't track are dropped.
    pub fn reconcile(&mut self, gens: &Generations, side: Side, last_id: i64) {
        let mut next = BTreeMap::new();
        for (f, g) in gens.side(side) {
            let kept = self.0.get(f).copied().filter(|r| r.generation == g);
            let r = kept.unwrap_or(Recorded {
                generation: g,
                walk: (g > 0 && last_id > 0).then_some(Walk {
                    cursor: 0,
                    high: last_id,
                }),
            });
            next.insert(f.to_owned(), r);
        }
        self.0 = next;
    }

    /// The walk still to do, as `(from, high)`: the lowest cursor and the
    /// highest end among the fields being backfilled. One pass over
    /// `(from, high]` serves them all, since a document is rebuilt whole.
    pub fn pending(&self) -> Option<(i64, i64)> {
        let walks = self.0.values().filter_map(|r| r.walk);
        walks.fold(None, |acc, w| match acc {
            None => Some((w.cursor, w.high)),
            Some((from, high)) => Some((from.min(w.cursor), high.max(w.high))),
        })
    }

    /// Records that every torrent with an id in `(from, to]` was just
    /// re-indexed, `from` being [`PopState::pending`]'s: each walk moves
    /// to `to` (a walk whose cursor is past `from` had the rest of that
    /// range already), and one that reaches its end is complete.
    pub fn advance(&mut self, to: i64) {
        for r in self.0.values_mut() {
            if let Some(w) = &mut r.walk {
                w.cursor = w.cursor.max(to);
                if w.cursor >= w.high {
                    r.walk = None;
                }
            }
        }
    }

    /// What this state says of `field`; `None` when it isn't tracked
    /// here.
    pub fn availability(&self, field: &str) -> Option<Availability> {
        self.0.get(field).map(|r| match (r.generation, r.walk) {
            (0, _) => Availability::NoProducer,
            (_, None) => Availability::Ready,
            (_, Some(w)) => Availability::Backfilling {
                done: w.cursor,
                upto: w.high,
            },
        })
    }

    pub fn fields(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    pub fn get(&self, field: &str) -> Option<Recorded> {
        self.0.get(field).copied()
    }

    /// The payload form: `name=gen` for a complete field and
    /// `name=gen@cursor/high` for one being backfilled, comma-separated
    /// in name order.
    pub fn encode(&self) -> String {
        let mut out = String::new();
        for (f, r) in &self.0 {
            if !out.is_empty() {
                out.push(',');
            }
            out.push_str(f);
            out.push('=');
            out.push_str(&r.generation.to_string());
            if let Some(w) = r.walk {
                out.push_str(&format!("@{}/{}", w.cursor, w.high));
            }
        }
        out
    }

    /// Parses [`PopState::encode`]'s form. Anything else, including a
    /// repeated or out-of-order field, a walk that's empty or runs
    /// backwards, or a walk at generation 0, is
    /// `None`: the payload is ours alone, so it's corrupt.
    pub fn decode(s: &str) -> Option<PopState> {
        let mut out = BTreeMap::new();
        if s.is_empty() {
            return Some(PopState(out));
        }
        for entry in s.split(',') {
            let (name, rest) = entry.split_once('=')?;
            let name_ok = !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.".contains(&b));
            if !name_ok {
                return None;
            }
            let (generation, walk) = match rest.split_once('@') {
                None => (number::<u32>(rest)?, None),
                Some((g, w)) => {
                    let (c, h) = w.split_once('/')?;
                    let (cursor, high) = (number::<i64>(c)?, number::<i64>(h)?);
                    // A walk without a producer has nothing to write.
                    if cursor >= high || g == "0" {
                        return None;
                    }
                    (number::<u32>(g)?, Some(Walk { cursor, high }))
                }
            };
            // In name order, each once, as encoded.
            if out
                .keys()
                .next_back()
                .is_some_and(|last: &String| last.as_str() >= name)
            {
                return None;
            }
            out.insert(name.to_owned(), Recorded { generation, walk });
        }
        Some(PopState(out))
    }
}

/// A non-negative decimal number with no sign or padding.
fn number<T: std::str::FromStr>(s: &str) -> Option<T> {
    let canonical =
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s == "0" || !s.starts_with('0'));
    if canonical { s.parse().ok() } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn gens() -> Generations {
        Generations::current()
    }

    #[test]
    fn tracked_fields_are_v6s() {
        let mut want: Vec<String> = crate::schema::V6_FIELDS
            .iter()
            .chain(&crate::schema::V7_FIELDS)
            .map(|f| (*f).to_owned())
            .chain(crate::files::V6_FIELDS.iter().map(|f| format!("file.{f}")))
            .collect();
        want.sort();
        let g = gens();
        let got: Vec<&str> = g.0.keys().map(String::as_str).collect();
        assert_eq!(got, want);
        // Each names a field of its index.
        let (schema, _) = crate::schema::build();
        let (files, _) = crate::files::schema();
        for (f, _) in GENERATIONS {
            match f.strip_prefix(FILE_PREFIX) {
                Some(name) => assert!(files.get_field(name).is_ok(), "{f}"),
                None => assert!(schema.get_field(f).is_ok(), "{f}"),
            }
        }
    }

    #[test]
    fn a_fresh_index_is_complete_at_this_builds_generations() {
        let t = PopState::fresh(&gens(), Side::Torrents);
        assert_eq!(
            t.availability("name_trigram"),
            Some(Availability::NoProducer)
        );
        assert_eq!(t.availability("payload_sig"), Some(Availability::Ready));
        assert_eq!(t.availability("verified"), Some(Availability::Ready));
        assert_eq!(t.availability("file.category"), None, "another side");
        assert_eq!(t.pending(), None);
        let f = PopState::fresh(&gens(), Side::Files);
        assert_eq!(f.availability("file.category"), Some(Availability::Ready));
        assert_eq!(
            f.encode(),
            "file.category=1,file.pieces_root=1,file.size_bucket=1"
        );
    }

    #[test]
    fn a_producer_shipping_starts_a_walk_that_resumes_and_completes() {
        // An index built before GS-0 shipped the `payload_sig` producer.
        let mut s = PopState::fresh(&gens().with("payload_sig", 0), Side::Torrents);
        let shipped = gens();
        assert_eq!(shipped.get("payload_sig"), Some(1));
        s.reconcile(&shipped, Side::Torrents, 500);
        let backfilling = |done| Some(Availability::Backfilling { done, upto: 500 });
        assert_eq!(s.availability("payload_sig"), backfilling(0));
        assert_eq!(
            s.availability("name_trigram"),
            Some(Availability::NoProducer)
        );
        assert_eq!(s.pending(), Some((0, 500)));
        s.advance(200);
        // Reopened (a restart): the walk goes on, whatever the index
        // holds now.
        let mut reopened = PopState::decode(&s.encode()).unwrap();
        assert_eq!(reopened, s);
        reopened.reconcile(&shipped, Side::Torrents, 900);
        assert_eq!(reopened.availability("payload_sig"), backfilling(200));
        reopened.advance(500);
        assert_eq!(
            reopened.availability("payload_sig"),
            Some(Availability::Ready)
        );
        assert_eq!(reopened.pending(), None);
    }

    /// GS-2 ships `group_key` and `group_minhash` together: an index
    /// from GS-0's build walks once for both, and neither is Ready until
    /// that walk is done.
    #[test]
    fn the_grouping_producers_share_one_walk() {
        let before = gens().with("group_key", 0).with("group_minhash", 0);
        let mut s = PopState::fresh(&before, Side::Torrents);
        assert_eq!(
            s.availability("group_minhash"),
            Some(Availability::NoProducer)
        );
        s.reconcile(&gens(), Side::Torrents, 300);
        for f in ["group_key", "group_minhash"] {
            assert_eq!(
                s.availability(f),
                Some(Availability::Backfilling { done: 0, upto: 300 }),
                "{f}"
            );
        }
        assert_eq!(s.availability("payload_sig"), Some(Availability::Ready));
        assert_eq!(s.pending(), Some((0, 300)));
        s.advance(300);
        assert_eq!(s.availability("group_key"), Some(Availability::Ready));
        assert_eq!(s.availability("group_minhash"), Some(Availability::Ready));
    }

    #[test]
    fn an_empty_index_or_a_missing_record_needs_no_walk_or_a_full_one() {
        let shipped = gens().with("group_key", 2);
        let mut empty = PopState::default();
        empty.reconcile(&shipped, Side::Torrents, 0);
        assert_eq!(empty.availability("group_key"), Some(Availability::Ready));
        // An index that recorded nothing (or another generation) walks
        // everything it holds.
        let mut old = PopState::decode("group_key=1,legacy=3").unwrap();
        old.reconcile(&shipped, Side::Torrents, 10);
        assert_eq!(
            old.get("group_key").unwrap().walk,
            Some(Walk {
                cursor: 0,
                high: 10
            })
        );
        assert!(old.get("legacy").is_none(), "untracked fields are dropped");
    }

    #[test]
    fn walks_started_at_different_times_share_one_pass() {
        let mut s = PopState::default();
        s.reconcile(&gens().with("payload_sig", 1), Side::Torrents, 100);
        s.advance(60);
        s.reconcile(
            &gens().with("payload_sig", 1).with("name_trigram", 1),
            Side::Torrents,
            150,
        );
        assert_eq!(s.pending(), Some((0, 150)));
        // A pass over (0, 80] finishes nothing; payload_sig moves on.
        s.advance(80);
        assert_eq!(
            s.get("payload_sig").unwrap().walk,
            Some(Walk {
                cursor: 80,
                high: 100
            })
        );
        assert_eq!(s.pending(), Some((80, 150)));
        s.advance(120);
        assert_eq!(s.availability("payload_sig"), Some(Availability::Ready));
        assert_eq!(s.pending(), Some((120, 150)));
    }

    #[test]
    fn bad_payloads_are_refused() {
        for bad in [
            "x",
            "x=",
            "=1",
            "X=1",
            "x=1,x=2",
            "y=1,x=1",
            "x=-1",
            "x=01",
            "x=+1",
            "x=1@5/5",
            "x=1@6/5",
            "x=1@-1/5",
            "x=0@1/5",
            "x=1@1",
            "x=1@1/",
            "x=1,",
            ",x=1",
            "x y=1",
            "x=4294967296",
        ] {
            assert_eq!(PopState::decode(bad), None, "{bad}");
        }
        assert_eq!(PopState::decode(""), Some(PopState::default()));
    }

    fn state() -> impl Strategy<Value = PopState> {
        let walk = proptest::option::of((0i64..1 << 40, 1i64..1 << 20));
        let rec = (any::<u32>(), walk).prop_map(|(generation, w)| Recorded {
            generation,
            walk: w.filter(|_| generation > 0).map(|(cursor, len)| Walk {
                cursor,
                high: cursor + len,
            }),
        });
        proptest::collection::btree_map("[a-z_][a-z0-9_.]{0,15}", rec, 0..8).prop_map(PopState)
    }

    proptest! {
        #[test]
        fn encoding_round_trips(s in state()) {
            prop_assert_eq!(PopState::decode(&s.encode()), Some(s));
        }

        #[test]
        fn decoding_never_panics_and_only_accepts_canonical_forms(s in "[a-z0-9_.=@/,+-]{0,40}") {
            if let Some(p) = PopState::decode(&s) {
                prop_assert_eq!(p.encode(), s);
            }
        }

        #[test]
        fn advancing_never_moves_a_cursor_back_or_past_its_end(
            s in state(),
            to in 0i64..1 << 41,
        ) {
            let mut a = s.clone();
            a.advance(to);
            for f in s.fields() {
                let (before, after) = (s.get(f).unwrap(), a.get(f).unwrap());
                prop_assert_eq!(before.generation, after.generation);
                match (before.walk, after.walk) {
                    (Some(b), Some(w)) => {
                        prop_assert!(w.cursor >= b.cursor && w.cursor < w.high);
                        prop_assert_eq!(b.high, w.high);
                    }
                    (Some(b), None) => prop_assert!(to >= b.high),
                    (None, None) => {}
                    (None, Some(_)) => prop_assert!(false, "a complete field restarted"),
                }
            }
        }
    }
}
