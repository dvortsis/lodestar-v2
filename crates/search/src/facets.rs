//! Facets (architecture §8 "Results, facets and features", §10.4): term
//! counts per value and histograms over the documents a search returns,
//! a separate request from the results.
//!
//! **Same documents as the search.** Facets run on a [`Lease`]: its
//! snapshot, its compiled query (with the hidden-torrent clause and
//! whatever else the API put in the AST, and the stem-tier decision) and
//! its `now`. The API takes a new lease the way `/search` does, or the
//! one a search's cursor names.
//!
//! **Disjunctive counts.** A facet's counts ignore its own selection, so
//! a user can pick several values: they come from the query with the
//! facet's own clauses removed and every other clause kept. A facet's
//! own clauses ([`Facet::owns`]) are the conjuncts of the top-level
//! `AND` (or the whole query, when it isn't one) built only from that
//! facet's field:
//! - a leaf on the field (`res:1080p`, `ext:mkv,mp4`, `private:no`,
//!   `cat:video` for `category`);
//! - an `OR` or a parenthesised `AND` whose every branch is the field's
//!   own (`res:1080p | res:720p`);
//! - a negation of one (`-res:480p`, `-(res:480p | res:576p)`).
//!
//! A conjunct that mixes fields (`res:1080p | type:movie`) belongs to no
//! facet: it stays in every facet's query, as any other filter does. A
//! clause inside `file:(…)` is about one file, not the torrent's field,
//! and stays too. Values a facet's own clauses name are always listed:
//! as `selected`, or `excluded` under an odd number of negations.
//! Removing nothing (no own clause) means the facet's counts come from
//! the search's own query, which every facet without a selection and
//! every histogram share: one pass over the matches for all of them,
//! plus one per facet with a selection.
//!
//! **Sampling.** A pass over more than [`SAMPLE_ABOVE`] matches (the
//! search's count for the shared pass, the count estimate for a facet's
//! own) visits only the slices `count.rs` samples, 1/16 of each segment
//! above 65,536 documents, and scales each segment's counts by its
//! documents over those visited. Its groups say `approx`.
//! **Error bound:** a count `c` from a sample of 1/16 has a standard
//! deviation of about `sqrt(16 c) = 4 sqrt(c)` when its documents spread
//! over the doc-id space (a binomial count scaled by 16): ±4,000 (0.4%)
//! at a million, ±400 (4%) at 10,000, so a small value's relative error
//! is large while its absolute one stays small. Values packed into a
//! narrow doc-id range (a few days' discoveries) are off by up to one
//! stratum per edge, as `count.rs` documents for counts. Tested against
//! six standard deviations on a synthetic set.
//!
//! **Histograms** count the search's own matches (no disjunction), so
//! their bins plus `missing` sum to the result count. Edges are explicit:
//! size in bytes on fixed powers of two, discovered in UTC seconds on
//! bins chosen from the range, years as `[year, year + 1)`.
//!
//! The result is a pure function of (snapshot, AST, parameters), so the
//! result cache (B5c) can keep it.

use std::collections::{BTreeMap, HashMap};
use std::ops::{Bound, Range};

use tantivy::columnar::{Column, StrColumn};
use tantivy::{DocId, SegmentReader, TERMINATED};

use crate::cancel::CancelToken;
use crate::count::{self, Matcher, SAMPLE_ABOVE as SEGMENT_SAMPLE_ABOVE};
use crate::index::{Compiled, Result};
use crate::lease::Lease;
use crate::query::{Node, ShareBy, TagKey};

/// Above this many matches, a pass is computed on a sample (§8).
pub const SAMPLE_ABOVE: usize = 1_000_000;
/// Values listed per facet unless the request asks for more.
pub const DEFAULT_LIMIT: usize = 8;
/// The most values a request may ask for per facet.
pub const MAX_LIMIT: usize = 100;
/// Seeders and trackers-reporting buckets (§10.4: ≥ 1 / ≥ 5 / ≥ 50 and
/// never scraped). Off until M4's swarm fields are in the index; until
/// then they're `unavailable`, never zeros.
pub const SWARM_BUCKETS: bool = false;
/// The bucket thresholds, once [`SWARM_BUCKETS`] is on.
pub const SWARM_THRESHOLDS: [u64; 3] = [1, 5, 50];

/// A term facet: a field whose values are counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Facet {
    /// The media category (`ContentType`, §6).
    Type,
    Ext,
    /// File categories present: a torrent with any `video` file counts
    /// for `video` (`cat:video` selects it).
    Category,
    Res,
    Lang,
    Group,
    Codec,
    Audio,
    Hdr,
    /// Structural presence: `nfo`, `subs`, `cue`, `sample`, `sfv`.
    Has,
    /// Adult subtypes (§20.9). Listed only when the search's documents
    /// include adult ones or the query selects one: with Adult hidden,
    /// none are left to count.
    Adult,
    Private,
    V2,
    Ambiguous,
    Imported,
    Backfill,
}

/// Where a facet's values are in the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// `key:value` terms of the `tag` column.
    Tag(TagKey),
    /// The `ext` column.
    Ext,
    /// One term of the `flag` column, as `yes`/`no`.
    Flag(&'static str),
    /// `cat:<category>` terms of the `tag` column, one per file
    /// category the torrent has (`doc.rs`): read in the same pass as the
    /// tags, where ten share columns per document would double its cost.
    Category,
}

impl Facet {
    /// Every facet, in §10.4's decision order: the default groups, then
    /// "More filters".
    pub const ALL: [Facet; 16] = [
        Facet::Type,
        Facet::Ext,
        Facet::Res,
        Facet::Category,
        Facet::Lang,
        Facet::Group,
        Facet::Codec,
        Facet::Audio,
        Facet::Hdr,
        Facet::Private,
        Facet::V2,
        Facet::Has,
        Facet::Ambiguous,
        Facet::Imported,
        Facet::Backfill,
        Facet::Adult,
    ];

    /// The name in the API (and the query language's field, but for
    /// `category`, which is `cat:`).
    pub fn name(self) -> &'static str {
        match self.source() {
            Source::Tag(k) => k.as_str(),
            Source::Ext => "ext",
            Source::Flag(f) => f,
            Source::Category => "category",
        }
    }

    pub fn from_name(name: &str) -> Option<Facet> {
        Facet::ALL.into_iter().find(|f| f.name() == name)
    }

    fn source(self) -> Source {
        match self {
            Facet::Type => Source::Tag(TagKey::Type),
            Facet::Ext => Source::Ext,
            Facet::Category => Source::Category,
            Facet::Res => Source::Tag(TagKey::Res),
            Facet::Lang => Source::Tag(TagKey::Lang),
            Facet::Group => Source::Tag(TagKey::Group),
            Facet::Codec => Source::Tag(TagKey::Codec),
            Facet::Audio => Source::Tag(TagKey::Audio),
            Facet::Hdr => Source::Tag(TagKey::Hdr),
            Facet::Has => Source::Tag(TagKey::Has),
            Facet::Adult => Source::Tag(TagKey::Adult),
            Facet::Private => Source::Flag("private"),
            Facet::V2 => Source::Flag("v2"),
            Facet::Ambiguous => Source::Flag("ambiguous"),
            Facet::Imported => Source::Flag("imported"),
            Facet::Backfill => Source::Flag("backfill"),
        }
    }

    /// The index fields the facet reads (for an older index that lacks
    /// them, `SearchIndex::missing_fields`).
    fn index_fields(self) -> Vec<String> {
        match self.source() {
            Source::Tag(_) => vec!["tag".into()],
            Source::Ext => vec!["ext".into()],
            Source::Flag(_) => vec!["flag".into()],
            // The `cat:` terms arrived with schema v5, as `release_year`
            // did: an older index lacks that field, and the terms too.
            Source::Category => vec!["tag".into(), "release_year".into()],
        }
    }

    /// Whether `node` is one of this facet's own clauses (module docs):
    /// a leaf on its field, or `AND`/`OR`/`NOT` made only of them.
    pub fn owns(self, node: &Node) -> bool {
        match node {
            Node::And { of } | Node::Or { of } => !of.is_empty() && of.iter().all(|n| self.owns(n)),
            Node::Not { of } => self.owns(of),
            leaf => self.leaf_values(leaf).is_some(),
        }
    }

    /// The values a leaf on this facet's field names; `None` for any
    /// other node.
    fn leaf_values(self, node: &Node) -> Option<Vec<String>> {
        match (self.source(), node) {
            (Source::Tag(k), Node::Tag { key, any }) if *key == k => Some(any.clone()),
            (Source::Ext, Node::Ext { any }) => Some(any.clone()),
            (Source::Flag(f), Node::Flag { name, value }) if name == f => {
                Some(vec![yes_no(*value).to_owned()])
            }
            (
                Source::Category,
                Node::Share {
                    by: ShareBy::Count,
                    cat,
                    ..
                },
            ) => Some(vec![cat.clone()]),
            _ => None,
        }
    }

    /// The values this facet's own clauses in `node` name, with whether
    /// each is negated.
    fn selection(self, node: &Node) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        for c in conjuncts(node).into_iter().filter(|c| self.owns(c)) {
            self.collect(c, false, &mut out);
        }
        out
    }

    fn collect(self, node: &Node, negated: bool, out: &mut Vec<(String, bool)>) {
        match node {
            Node::And { of } | Node::Or { of } => {
                of.iter().for_each(|n| self.collect(n, negated, out));
            }
            Node::Not { of } => self.collect(of, !negated, out),
            leaf => {
                for v in self.leaf_values(leaf).unwrap_or_default() {
                    if !out.iter().any(|(w, n)| *w == v && *n == negated) {
                        out.push((v, negated));
                    }
                }
            }
        }
    }
}

fn yes_no(v: bool) -> &'static str {
    if v { "yes" } else { "no" }
}

/// The top-level conjuncts of a query: an `AND`'s children, or the
/// query itself.
fn conjuncts(node: &Node) -> Vec<&Node> {
    match node {
        Node::And { of } => of.iter().collect(),
        n => vec![n],
    }
}

/// `node` without `facet`'s own clauses (everything when nothing else is
/// left), or `None` when it has none.
pub fn without_facet(node: &Node, facet: Facet) -> Option<Node> {
    let all = conjuncts(node);
    let kept: Vec<Node> = all
        .iter()
        .filter(|c| !facet.owns(c))
        .map(|c| (*c).clone())
        .collect();
    if kept.len() == all.len() {
        return None;
    }
    Some(match kept.len() {
        0 => Node::All,
        1 if !matches!(node, Node::And { .. }) => kept.into_iter().next().expect("one"),
        _ => Node::And { of: kept },
    })
}

/// A histogram.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Histo {
    /// Bytes, on powers of two from 1 MiB to 1 TiB, plus under and over.
    Size,
    /// UTC seconds, in hour, day, week or month bins chosen from the
    /// range of the matches.
    Discovered,
    /// The release's first year, one bin per year.
    Year,
}

impl Histo {
    pub const ALL: [Histo; 3] = [Histo::Size, Histo::Discovered, Histo::Year];

    pub fn name(self) -> &'static str {
        match self {
            Histo::Size => "size",
            Histo::Discovered => "discovered",
            Histo::Year => "year",
        }
    }

    fn index_field(self) -> &'static str {
        match self {
            Histo::Size => "size",
            Histo::Discovered => "discovered",
            Histo::Year => "release_year",
        }
    }
}

/// What a histogram's bin edges measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Bytes,
    Hour,
    Day,
    /// Seven UTC days from a Thursday 00:00 (Unix epoch weeks, as
    /// `docs/metrics.md` §7.2's swarm history).
    Week,
    /// A calendar month in UTC.
    Month,
    Year,
}

impl Unit {
    pub fn as_str(self) -> &'static str {
        match self {
            Unit::Bytes => "bytes",
            Unit::Hour => "hour",
            Unit::Day => "day",
            Unit::Week => "week",
            Unit::Month => "month",
            Unit::Year => "year",
        }
    }
}

/// What to compute beyond the defaults.
#[derive(Debug, Clone, Default)]
pub struct FacetParams {
    /// Values listed per facet: [`DEFAULT_LIMIT`] unless set here, at
    /// most [`MAX_LIMIT`].
    pub limits: BTreeMap<Facet, usize>,
    /// When the crawler first ran (the store's `first_run_at`):
    /// discovered bins that end by then are marked `backfill` (§10.4).
    pub first_run: Option<i64>,
    pub(crate) policy: Policy,
}

impl FacetParams {
    pub fn new(limits: BTreeMap<Facet, usize>, first_run: Option<i64>) -> FacetParams {
        FacetParams {
            limits,
            first_run,
            policy: Policy::default(),
        }
    }
}

/// When passes are sampled; tests lower both thresholds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Policy {
    /// Matches above which a pass is sampled.
    pub sample_above: usize,
    /// Segments up to this size are visited whole even then.
    pub segment_sample_above: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            sample_above: SAMPLE_ABOVE,
            segment_sample_above: SEGMENT_SAMPLE_ABOVE,
        }
    }
}

/// The facets of one search.
#[derive(Debug, Clone, PartialEq)]
pub struct Facets {
    /// Every facet in [`Facet::ALL`] order, but those `unavailable` (and
    /// `adult` with nothing to list).
    pub terms: Vec<(Facet, TermCounts)>,
    pub histograms: Vec<(Histo, Histogram)>,
    /// The share of the documents a sampled pass visited; `None` when
    /// nothing was sampled.
    pub sample_rate: Option<f64>,
    /// Facets and histograms the index can't serve: fields an older
    /// index lacks while it's rebuilt, and the swarm buckets until M4.
    pub unavailable: Vec<String>,
    /// Matches the shared pass counted (scaled when sampled).
    pub matches: u64,
}

impl Facets {
    pub fn term(&self, f: Facet) -> Option<&TermCounts> {
        self.terms.iter().find(|(g, _)| *g == f).map(|(_, t)| t)
    }

    pub fn histogram(&self, h: Histo) -> Option<&Histogram> {
        self.histograms
            .iter()
            .find(|(g, _)| *g == h)
            .map(|(_, t)| t)
    }

    /// Whether any group was computed on a sample.
    pub fn sampled(&self) -> bool {
        self.sample_rate.is_some()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TermCounts {
    /// The top values by count (ties by value), then any selected or
    /// excluded value outside them.
    pub values: Vec<ValueCount>,
    /// The counts of the values not listed, summed.
    pub other: u64,
    /// Distinct values with a count, listed or not.
    pub distinct: u64,
    /// Computed on a sample.
    pub approx: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueCount {
    pub value: String,
    pub count: u64,
    /// The query selects it.
    pub selected: bool,
    /// The query excludes it (`-res:480p`).
    pub excluded: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Histogram {
    pub unit: Unit,
    /// In order; empty bins are present with count 0.
    pub bins: Vec<Bin>,
    /// Matches without a value (no year parsed).
    pub missing: u64,
    pub approx: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bin {
    /// Inclusive.
    pub lo: i64,
    /// Exclusive; `None` for the open top bin.
    pub hi: Option<i64>,
    pub count: u64,
    /// Ends before the crawler's first run: discovered dates there are
    /// "when we first saw it" during the backfill (§10.4).
    pub backfill: bool,
}

/// The facets of the search `lease` holds, whose query is `node` (the
/// AST the lease was taken for).
pub fn facets(
    lease: &Lease,
    node: &Node,
    params: &FacetParams,
    cancel: &CancelToken,
) -> Result<Facets> {
    cancel.check()?;
    let index = lease.index();
    let missing = index.missing_fields();
    let lacks = |names: &[String]| names.iter().any(|n| missing.contains(n));
    let mut unavailable: Vec<String> = Vec::new();
    let mut served: Vec<Facet> = Vec::new();
    for f in Facet::ALL {
        if lacks(&f.index_fields()) {
            unavailable.push(f.name().into());
        } else {
            served.push(f);
        }
    }
    let mut histos: Vec<Histo> = Vec::new();
    for h in Histo::ALL {
        if lacks(&[h.index_field().to_owned()]) {
            unavailable.push(h.name().into());
        } else {
            histos.push(h);
        }
    }
    if !SWARM_BUCKETS {
        unavailable.extend(["seeders".into(), "trackers".into()]);
    }

    // Facets with their own clauses get a pass of their own.
    let (own, shared): (Vec<Facet>, Vec<Facet>) = served
        .into_iter()
        .partition(|f| conjuncts(node).iter().any(|c| f.owns(c)));
    let searcher = lease.searcher();
    let policy = params.policy;
    let base = Want {
        facets: shared,
        histos,
    };
    let base_sampled = lease.total().value > policy.sample_above;
    let mut sample_rate = None;
    let shared_tally = pass(
        lease.compiled(),
        searcher,
        &base,
        base_sampled,
        policy,
        cancel,
    )?;
    if shared_tally.sampled {
        sample_rate = Some(shared_tally.rate);
    }
    let mut tallies: Vec<(Vec<Facet>, Tally)> = vec![(base.facets.clone(), shared_tally)];
    for f in own {
        let variant = without_facet(node, f).expect("an own clause");
        let compiled = index.compile(&variant, lease.now(), lease.stemmed());
        let estimate =
            count::estimate_with(searcher, &compiled, cancel, policy.segment_sample_above)?;
        let want = Want {
            facets: vec![f],
            histos: Vec::new(),
        };
        let t = pass(
            &compiled,
            searcher,
            &want,
            estimate.value > policy.sample_above,
            policy,
            cancel,
        )?;
        if t.sampled {
            sample_rate = Some(t.rate);
        }
        tallies.push((vec![f], t));
    }

    let mut terms = Vec::new();
    for f in Facet::ALL {
        let Some((_, t)) = tallies.iter().find(|(fs, _)| fs.contains(&f)) else {
            continue;
        };
        let limit = params
            .limits
            .get(&f)
            .copied()
            .unwrap_or(DEFAULT_LIMIT)
            .clamp(1, MAX_LIMIT);
        let counts = top(f, t, &f.selection(node), limit);
        if f == Facet::Adult && counts.values.is_empty() {
            continue;
        }
        terms.push((f, counts));
    }
    let shared_tally = &tallies[0].1;
    let histograms = base
        .histos
        .iter()
        .map(|h| (*h, histogram(*h, shared_tally, params.first_run)))
        .collect();
    Ok(Facets {
        terms,
        histograms,
        sample_rate,
        unavailable,
        matches: shared_tally.docs.round() as u64,
    })
}

/// What one pass counts.
#[derive(Debug, Clone)]
struct Want {
    facets: Vec<Facet>,
    histos: Vec<Histo>,
}

impl Want {
    fn tags(&self) -> Vec<TagKey> {
        self.facets
            .iter()
            .filter_map(|f| match f.source() {
                Source::Tag(k) => Some(k),
                _ => None,
            })
            .collect()
    }

    fn flags(&self) -> Vec<(Facet, &'static str)> {
        self.facets
            .iter()
            .filter_map(|f| match f.source() {
                Source::Flag(n) => Some((*f, n)),
                _ => None,
            })
            .collect()
    }

    fn has(&self, f: Facet) -> bool {
        self.facets.contains(&f)
    }

    fn histo(&self, h: Histo) -> bool {
        self.histos.contains(&h)
    }
}

/// One pass's counts over every segment, scaled to the whole segment
/// where it was sampled.
#[derive(Debug, Default)]
struct Tally {
    docs: f64,
    /// Term facet values (flags as `yes`; `no` is the rest).
    terms: HashMap<(Facet, String), f64>,
    size: [f64; SIZE_BINS],
    /// By UTC hour.
    hours: HashMap<i64, f64>,
    years: HashMap<u64, f64>,
    /// Matches with a year.
    with_year: f64,
    sampled: bool,
    /// Documents visited over documents in the segments.
    rate: f64,
}

/// The `tag` key of the file-category terms (`doc.rs`); not a query
/// field (`cat:` is the share range).
pub(crate) const CATEGORY_TAG: &str = "cat";

/// Size bins: under 1 MiB, the 20 powers of two from 1 MiB to 1 TiB,
/// and 1 TiB or more.
const SIZE_BINS: usize = 22;
const SIZE_FROM_LOG2: u32 = 20;

fn size_bin(bytes: u64) -> usize {
    if bytes < 1 << SIZE_FROM_LOG2 {
        return 0;
    }
    let log2 = 63 - bytes.leading_zeros();
    ((log2 - SIZE_FROM_LOG2 + 1) as usize).min(SIZE_BINS - 1)
}

fn pass(
    compiled: &Compiled,
    searcher: &tantivy::Searcher,
    want: &Want,
    sample: bool,
    policy: Policy,
    cancel: &CancelToken,
) -> Result<Tally> {
    let parts = count::per_segment(searcher, compiled, cancel, |weight, reader| {
        let sample_above = if sample {
            policy.segment_sample_above
        } else {
            u32::MAX
        };
        segment(weight, reader, compiled, want, sample_above, cancel)
    })?;
    let mut total = Tally::default();
    let (mut visited, mut docs) = (0u64, 0u64);
    for (t, seen, max) in parts {
        visited += seen;
        docs += max;
        total.sampled |= t.sampled;
        total.docs += t.docs;
        for (k, v) in t.terms {
            *total.terms.entry(k).or_default() += v;
        }
        for (a, b) in total.size.iter_mut().zip(t.size) {
            *a += b;
        }
        for (k, v) in t.hours {
            *total.hours.entry(k).or_default() += v;
        }
        for (k, v) in t.years {
            *total.years.entry(k).or_default() += v;
        }
        total.with_year += t.with_year;
    }
    total.rate = if docs == 0 {
        1.0
    } else {
        visited as f64 / docs as f64
    };
    Ok(total)
}

/// One segment's columns for a pass.
struct Columns {
    tag: Option<StrColumn>,
    ext: Option<StrColumn>,
    flag: Option<StrColumn>,
    size: Option<Column<u64>>,
    discovered: Option<Column<i64>>,
    year: Option<Column<u64>>,
}

impl Columns {
    fn open(reader: &SegmentReader, want: &Want) -> tantivy::Result<Columns> {
        let fast = reader.fast_fields();
        let str_col = |name: &str, on: bool| if on { fast.str(name) } else { Ok(None) };
        Ok(Columns {
            tag: str_col("tag", !want.tags().is_empty() || want.has(Facet::Category))?,
            ext: str_col("ext", want.has(Facet::Ext))?,
            flag: str_col("flag", !want.flags().is_empty())?,
            size: if want.histo(Histo::Size) {
                fast.column_opt("size")?
            } else {
                None
            },
            discovered: if want.histo(Histo::Discovered) {
                fast.column_opt("discovered")?
            } else {
                None
            },
            year: if want.histo(Histo::Year) {
                fast.column_opt("release_year")?
            } else {
                None
            },
        })
    }
}

/// Raw counts in one segment.
struct Counts {
    docs: u64,
    tag: Vec<u32>,
    ext: Vec<u32>,
    flag: Vec<u32>,
    size: [u64; SIZE_BINS],
    hours: Hours,
    years: HashMap<u64, u64>,
    with_year: u64,
}

/// Counts by UTC hour: dense from the segment's earliest hour when its
/// span is reasonable, by hash otherwise (and for anything outside it,
/// which a column's bounds rule out).
struct Hours {
    base: i64,
    dense: Vec<u32>,
    sparse: HashMap<i64, u64>,
}

const DENSE_HOURS: i64 = 1 << 20;

impl Hours {
    fn for_column(col: Option<&Column<i64>>) -> Hours {
        let mut h = Hours {
            base: 0,
            dense: Vec::new(),
            sparse: HashMap::new(),
        };
        if let Some(col) = col {
            let (lo, hi) = (hour(col.min_value()), hour(col.max_value()));
            if let Some(span) = hi.checked_sub(lo).filter(|s| *s < DENSE_HOURS) {
                h.base = lo;
                h.dense = vec![0; span as usize + 1];
            }
        }
        h
    }

    fn add(&mut self, h: i64) {
        let slot = h
            .checked_sub(self.base)
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| self.dense.get_mut(i));
        match slot {
            Some(c) => *c += 1,
            None => *self.sparse.entry(h).or_default() += 1,
        }
    }

    fn into_counts(self) -> impl Iterator<Item = (i64, u64)> {
        let base = self.base;
        self.dense
            .into_iter()
            .enumerate()
            .filter(|(_, c)| *c > 0)
            .map(move |(i, c)| (base + i as i64, u64::from(c)))
            .chain(self.sparse)
    }
}

fn hour(t: i64) -> i64 {
    t.div_euclid(3600)
}

impl Counts {
    fn new(cols: &Columns) -> Counts {
        let terms = |c: &Option<StrColumn>| vec![0; c.as_ref().map_or(0, |c| c.num_terms())];
        Counts {
            docs: 0,
            tag: terms(&cols.tag),
            ext: terms(&cols.ext),
            flag: terms(&cols.flag),
            size: [0; SIZE_BINS],
            hours: Hours::for_column(cols.discovered.as_ref()),
            years: HashMap::new(),
            with_year: 0,
        }
    }

    fn add(
        &mut self,
        cols: &Columns,
        docs: &[DocId],
        buf: &mut Vec<Option<u64>>,
        times: &mut Vec<Option<i64>>,
    ) {
        self.docs += docs.len() as u64;
        for (col, counts) in [
            (&cols.tag, &mut self.tag),
            (&cols.ext, &mut self.ext),
            (&cols.flag, &mut self.flag),
        ] {
            if let Some(col) = col {
                for &d in docs {
                    for ord in col.term_ords(d) {
                        counts[ord as usize] += 1;
                    }
                }
            }
        }
        buf.resize(docs.len(), None);
        if let Some(col) = &cols.size {
            col.first_vals(docs, buf);
            for v in buf.iter().flatten() {
                self.size[size_bin(*v)] += 1;
            }
        }
        if let Some(col) = &cols.discovered {
            times.resize(docs.len(), None);
            col.first_vals(docs, times);
            for t in times.iter().flatten() {
                self.hours.add(hour(*t));
            }
        }
        if let Some(col) = &cols.year {
            col.first_vals(docs, buf);
            for y in buf.iter().flatten() {
                *self.years.entry(*y).or_default() += 1;
                self.with_year += 1;
            }
        }
    }

    /// Into a [`Tally`], each count scaled by `scale`.
    fn harvest(self, cols: &Columns, want: &Want, scale: f64) -> std::io::Result<Tally> {
        let mut t = Tally {
            docs: self.docs as f64 * scale,
            ..Tally::default()
        };
        let mut put = |f: Facet, value: &str, c: u64| {
            *t.terms.entry((f, value.to_owned())).or_default() += c as f64 * scale;
        };
        if let Some(col) = &cols.tag {
            let mut keys: Vec<(Facet, String)> = want
                .tags()
                .into_iter()
                .map(|key| {
                    let f = Facet::ALL
                        .into_iter()
                        .find(|f| f.source() == Source::Tag(key))
                        .expect("a facet per key");
                    (f, format!("{}:", key.as_str()))
                })
                .collect();
            if want.has(Facet::Category) {
                keys.push((Facet::Category, format!("{CATEGORY_TAG}:")));
            }
            for (f, prefix) in keys {
                let range = prefix_range(col, &prefix)?;
                terms(col, &self.tag, range, |term, c| {
                    put(f, term.strip_prefix(&prefix).unwrap_or(term), c)
                })?;
            }
        }
        if let Some(col) = &cols.ext {
            terms(col, &self.ext, 0..col.num_terms() as u64, |term, c| {
                put(Facet::Ext, term, c)
            })?;
        }
        if let Some(col) = &cols.flag {
            let flags = want.flags();
            terms(col, &self.flag, 0..col.num_terms() as u64, |term, c| {
                if let Some((f, _)) = flags.iter().find(|(_, n)| *n == term) {
                    put(*f, "yes", c);
                }
            })?;
        }
        for (a, b) in t.size.iter_mut().zip(self.size) {
            *a = b as f64 * scale;
        }
        for (h, c) in self.hours.into_counts() {
            *t.hours.entry(h).or_default() += c as f64 * scale;
        }
        t.years = self
            .years
            .into_iter()
            .map(|(y, c)| (y, c as f64 * scale))
            .collect();
        t.with_year = self.with_year as f64 * scale;
        Ok(t)
    }
}

/// The ordinals of the terms starting with `prefix`.
fn prefix_range(col: &StrColumn, prefix: &str) -> std::io::Result<Range<u64>> {
    let mut end = prefix.as_bytes().to_vec();
    // The prefix ends in `:`; `;` is the next byte.
    if let Some(last) = end.last_mut() {
        *last += 1;
    }
    let n = col.num_terms() as u64;
    let (lo, hi) = col.dictionary().term_bounds_to_ord(
        Bound::Included(prefix.as_bytes()),
        Bound::Excluded(&end[..]),
    )?;
    let lo = match lo {
        Bound::Included(o) => o,
        Bound::Excluded(o) => o.saturating_add(1),
        Bound::Unbounded => 0,
    };
    let hi = match hi {
        Bound::Included(o) => o.saturating_add(1),
        Bound::Excluded(o) => o,
        Bound::Unbounded => n,
    };
    Ok(lo.min(n)..hi.min(n).max(lo.min(n)))
}

/// Calls `f` with each term in `range` that has a count, and its count.
fn terms(
    col: &StrColumn,
    counts: &[u32],
    range: Range<u64>,
    mut f: impl FnMut(&str, u64),
) -> std::io::Result<()> {
    let ords: Vec<u64> = range.filter(|o| counts[*o as usize] > 0).collect();
    let mut i = 0;
    col.dictionary().sorted_ords_to_term_cb(&ords, |bytes| {
        let c = u64::from(counts[ords[i] as usize]);
        i += 1;
        // Keyword terms are written from `&str`.
        if let Ok(term) = std::str::from_utf8(bytes) {
            f(term, c);
        }
    })?;
    Ok(())
}

/// One segment's tally, and the documents it visited and has.
fn segment(
    weight: &dyn tantivy::query::Weight,
    reader: &SegmentReader,
    compiled: &Compiled,
    want: &Want,
    sample_above: u32,
    cancel: &CancelToken,
) -> tantivy::Result<(Tally, u64, u64)> {
    let max = reader.max_doc();
    let cols = Columns::open(reader, want)?;
    let mut counts = Counts::new(&cols);
    let mut m = Matcher::new(weight, reader, compiled, cancel)?;
    let (mut buf, mut times) = (Vec::new(), Vec::new());
    let mut add = |docs: &[DocId]| counts.add(&cols, docs, &mut buf, &mut times);
    let (scale, visited, sampled) = match count::slices(max, sample_above) {
        None => {
            m.visit_until(TERMINATED, &mut add)?;
            (1.0, u64::from(max), false)
        }
        Some(plan) => {
            for slice in &plan {
                m.seek(slice.start);
                m.visit_until(slice.end, &mut add)?;
            }
            let visited = plan.iter().map(|r| u64::from(r.end - r.start)).sum();
            (count::scale(max, &plan), visited, true)
        }
    };
    let mut t = counts
        .harvest(&cols, want, scale)
        .map_err(tantivy::TantivyError::from)?;
    t.sampled = sampled;
    Ok((t, visited, u64::from(max)))
}

/// A facet's listed values: the top `limit` by count, then the selected
/// and excluded ones outside them.
fn top(f: Facet, t: &Tally, selection: &[(String, bool)], limit: usize) -> TermCounts {
    let mut all: Vec<(String, u64)> = match f.source() {
        Source::Flag(_) => {
            let yes = t.terms.get(&(f, "yes".to_owned())).copied().unwrap_or(0.0);
            vec![
                ("yes".to_owned(), yes.round() as u64),
                ("no".to_owned(), (t.docs - yes).max(0.0).round() as u64),
            ]
        }
        _ => t
            .terms
            .iter()
            .filter(|((g, _), _)| *g == f)
            .map(|((_, v), c)| (v.clone(), c.round() as u64))
            .filter(|(_, c)| *c > 0)
            .collect(),
    };
    all.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let distinct = all.iter().filter(|(_, c)| *c > 0).count() as u64;
    let state = |v: &str| {
        let selected = selection.iter().any(|(w, neg)| w == v && !neg);
        let excluded = selection.iter().any(|(w, neg)| w == v && *neg);
        (selected, excluded)
    };
    let mut values: Vec<ValueCount> = Vec::new();
    let mut other = 0;
    for (i, (v, c)) in all.iter().enumerate() {
        let (selected, excluded) = state(v);
        if i < limit || selected || excluded {
            values.push(ValueCount {
                value: v.clone(),
                count: *c,
                selected,
                excluded,
            });
        } else {
            other += c;
        }
    }
    // Selected values nothing matches are listed too, at 0.
    for (v, _) in selection {
        if !values.iter().any(|x| x.value == *v) {
            let (selected, excluded) = state(v);
            values.push(ValueCount {
                value: v.clone(),
                count: 0,
                selected,
                excluded,
            });
        }
    }
    TermCounts {
        values,
        other,
        distinct,
        approx: t.sampled,
    }
}

fn histogram(h: Histo, t: &Tally, first_run: Option<i64>) -> Histogram {
    let round = |v: f64| v.round() as u64;
    match h {
        Histo::Size => {
            let bins = (0..SIZE_BINS)
                .map(|i| {
                    let (lo, hi) = match i {
                        0 => (0, Some(1i64 << SIZE_FROM_LOG2)),
                        i if i == SIZE_BINS - 1 => (1i64 << (SIZE_FROM_LOG2 + i as u32 - 1), None),
                        i => (
                            1i64 << (SIZE_FROM_LOG2 + i as u32 - 1),
                            Some(1i64 << (SIZE_FROM_LOG2 + i as u32)),
                        ),
                    };
                    Bin {
                        lo,
                        hi,
                        count: round(t.size[i]),
                        backfill: false,
                    }
                })
                .collect();
            Histogram {
                unit: Unit::Bytes,
                bins,
                missing: 0,
                approx: t.sampled,
            }
        }
        Histo::Discovered => {
            let (unit, bins) = discovered_bins(&t.hours, first_run);
            Histogram {
                unit,
                bins,
                missing: 0,
                approx: t.sampled,
            }
        }
        Histo::Year => {
            let bins = match (t.years.keys().min(), t.years.keys().max()) {
                (Some(&lo), Some(&hi)) => (lo..=hi)
                    .map(|y| Bin {
                        lo: y as i64,
                        hi: Some(y as i64 + 1),
                        count: round(t.years.get(&y).copied().unwrap_or(0.0)),
                        backfill: false,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            Histogram {
                unit: Unit::Year,
                bins,
                missing: round((t.docs - t.with_year).max(0.0)),
                approx: t.sampled,
            }
        }
    }
}

const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;

/// The bin unit for matches spanning `first..=last` (UTC hours): about
/// 50 to 100 bins at most, but for months.
fn unit_for(first: i64, last: i64) -> Unit {
    let span = last - first;
    if span < 48 {
        Unit::Hour
    } else if span < 90 * 24 {
        Unit::Day
    } else if span < 104 * 7 * 24 {
        Unit::Week
    } else {
        Unit::Month
    }
}

/// The start of the bin holding UTC time `t`, and the next bin's.
fn bin_of(unit: Unit, t: i64) -> (i64, i64) {
    let floor = |step: i64| t.div_euclid(step) * step;
    match unit {
        Unit::Hour => (floor(HOUR), floor(HOUR) + HOUR),
        Unit::Day => (floor(DAY), floor(DAY) + DAY),
        Unit::Week => (floor(WEEK), floor(WEEK) + WEEK),
        _ => {
            let (y, m, _) = civil(t.div_euclid(DAY));
            let next = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
            (
                days_from_civil(y, m, 1) * DAY,
                days_from_civil(next.0, next.1, 1) * DAY,
            )
        }
    }
}

fn discovered_bins(hours: &HashMap<i64, f64>, first_run: Option<i64>) -> (Unit, Vec<Bin>) {
    let (Some(&first), Some(&last)) = (hours.keys().min(), hours.keys().max()) else {
        return (Unit::Day, Vec::new());
    };
    let unit = unit_for(first, last);
    let mut by_bin: BTreeMap<i64, f64> = BTreeMap::new();
    for (h, c) in hours {
        *by_bin.entry(bin_of(unit, h * HOUR).0).or_default() += c;
    }
    let mut bins = Vec::new();
    let (mut lo, end) = (bin_of(unit, first * HOUR).0, bin_of(unit, last * HOUR).1);
    while lo < end {
        let hi = bin_of(unit, lo).1;
        bins.push(Bin {
            lo,
            hi: Some(hi),
            count: by_bin.get(&lo).copied().unwrap_or(0.0).round() as u64,
            backfill: first_run.is_some_and(|f| hi <= f),
        });
        lo = hi;
    }
    (unit, bins)
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian
/// (Howard Hinnant's `civil_from_days`).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// The inverse of [`civil`].
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests;
