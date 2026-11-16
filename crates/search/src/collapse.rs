//! Collapse-by-group (architecture §8, "show best version, +N others";
//! GS-3): one row per `group_key`, the group's best version under the
//! query's own order, with how many other matches share the group, and a
//! count of groups beside the count of torrents.
//!
//! **Groups.** `group_key` is a torrent's title identity in a fast column
//! (GS-2b), the same value as its `title_identity` term, so a group's
//! members are that term's postings. A torrent without a key is a group
//! of its own. While either field isn't Ready in the lease's snapshot
//! (the population contract, SB-6), the search answers ungrouped and says
//! why ([`Unavailable`]).
//!
//! **Pages.** A collapsed page walks the lease's ranked order (any sort,
//! relevance's windows included), reading each hit's key from the column.
//! A group's first hit is its best member: it's the row, and the group's
//! other hits are skipped. A page that resumes from a cursor skips every
//! group that has a member ranking at or before the cursor's position
//! ([`Lease::ranks_at_or_before`]), since that group was served from its
//! best member then. So the cursor carries only the last row's position,
//! the same position an uncollapsed cursor holds: no set of groups seen
//! grows with depth, and under one lease no group is served twice or
//! skipped. Per page, at most [`MAX_SCAN_GROUPS`] groups are decided; a
//! page that reaches it ends short, with a cursor after the last hit it
//! decided, so the next page carries on from there.
//!
//! **Others** is the group's members among all the query's matches, less
//! the row itself: the term's postings checked against the query (ranges
//! and deletes included), in doc order, a batch of groups per pass.
//!
//! **The groups count** follows the torrents count's rule (§8, `count.rs`):
//! exact while the torrents count is (up to 100k matches, or on demand),
//! from the distinct keys of every match, at most [`GROUPS_EXACT_CAP`]
//! held; otherwise "≈", a Horvitz–Thompson estimate. That draws a fixed
//! weighted sample of [`HT_SAMPLE`] keyed matches from the count's own
//! strata slices (a document of a sampled segment stands for `scale`
//! documents), counts each sampled group's members `m` exactly, and
//! estimates the keyed groups as `keyed × mean(1/m)`: a group of `m`
//! matches is drawn about `m` times as often, so each draw stands for
//! `1/m` of a group. Keyless matches, each a group, are counted over
//! every match the slices visit, not drawn: they're often most of the
//! groups, and drawing them would make most of the variance.

use std::collections::{BinaryHeap, HashMap, HashSet};

use tantivy::columnar::Column;
use tantivy::query::EnableScoring;
use tantivy::schema::IndexRecordOption;
use tantivy::{DocAddress, DocId, DocSet, Searcher, TERMINATED, Term};

use crate::cancel::{self, CHECK_EVERY, CancelToken};
use crate::count::{self, Matcher, Total};
use crate::cursor::Position;
use crate::index::{Error, Hit, Result};
use crate::lease::{EXACT_BUDGET, Lease, Start};
use crate::population::Availability;

/// Groups decided per page at most (served, or skipped as served
/// before): bounds a page's work and memory whatever the query.
pub const MAX_SCAN_GROUPS: usize = 4_096;
/// Member addresses held per batch at most, for the served-before check.
pub const MAX_MEMBERS: usize = 1 << 18;
/// Matches drawn for the groups estimate.
pub const HT_SAMPLE: usize = 1_024;
/// Distinct keys held for an exact groups count at most; past it, the
/// estimate stands.
pub const GROUPS_EXACT_CAP: usize = 1 << 20;

/// Group keys are already hashes (FNV-1a of the title identity), so sets
/// and maps of them hash with the key itself, not SipHash: the exact
/// groups count inserts every match's key.
#[derive(Default, Clone, Copy)]
struct KeyHasher(u64);

impl std::hash::Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn write_u64(&mut self, k: u64) {
        // A final mix, so keys that differ only in high bits still spread
        // over a table's low bits.
        self.0 = (k ^ (k >> 29)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    }
}

type Keyed = std::hash::BuildHasherDefault<KeyHasher>;
type KeySet = HashSet<u64, Keyed>;
type KeyMap<V> = HashMap<u64, V, Keyed>;

/// Hits read from the ranked order per step of a page.
const CHUNK_MIN: usize = 64;
const CHUNK_MAX: usize = 1_024;

/// Why a search asked to collapse answered ungrouped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// `group_key` or `title_identity` isn't Ready in the lease's
    /// snapshot: written for every torrent only once its walk or the
    /// rebuild completes.
    GroupKeyNotReady,
}

impl Unavailable {
    /// The API's machine-readable reason.
    pub fn code(self) -> &'static str {
        match self {
            Unavailable::GroupKeyNotReady => "group_key_not_ready",
        }
    }
}

/// Whether `lease` can be collapsed.
pub fn available(lease: &Lease) -> std::result::Result<(), Unavailable> {
    let index = lease.index();
    let ready = ["group_key", "title_identity"]
        .iter()
        .all(|f| index.availability_in(f, lease) == Availability::Ready);
    if ready {
        Ok(())
    } else {
        Err(Unavailable::GroupKeyNotReady)
    }
}

/// One row of a collapsed page.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// The group's best version under the query's order.
    pub hit: Hit,
    /// The group's other matches.
    pub others: u64,
}

/// A collapsed page, and where the next starts (`None`: the last).
#[derive(Debug, Clone, PartialEq)]
pub struct CollapsedPage {
    pub rows: Vec<Row>,
    pub next: Option<Position>,
}

/// Up to `limit` groups of `lease`'s results from `start` (`Offset`
/// counts groups). The caller has checked [`available`].
pub fn page(
    lease: &Lease,
    start: Start,
    limit: usize,
    cancel: &CancelToken,
) -> Result<CollapsedPage> {
    let limit = limit.max(1);
    let (after, mut skip) = match start {
        Start::After(p) => (Some(p), 0),
        Start::Offset(o) => (None, o),
    };
    let cap = MAX_SCAN_GROUPS.max(skip + limit);
    let mut keys = Keys::new(lease.searcher());
    // Groups decided on this page: served, or skipped as served before.
    let mut seen = KeySet::default();
    let mut rows = Vec::with_capacity(limit);
    // An offset counts groups: the walk starts from the top.
    let mut from = match start {
        Start::After(p) => Start::After(p),
        Start::Offset(_) => Start::Offset(0),
    };
    loop {
        cancel.check()?;
        let need = skip + limit - rows.len();
        let chunk = (need * 2).clamp(CHUNK_MIN, CHUNK_MAX);
        let (pg, addrs, positions) = lease.page_ranked(from, chunk, cancel)?;
        let found: Vec<Option<u64>> = addrs.iter().map(|a| keys.get(*a)).collect::<Result<_>>()?;
        // This chunk's groups not yet decided, in order.
        let mut fresh = Vec::new();
        let mut listed = KeySet::default();
        for k in found.iter().flatten() {
            if !seen.contains(k) && listed.insert(*k) {
                fresh.push(*k);
            }
        }
        let members = members(lease, &fresh, after.is_some(), cancel)?;
        let served_before = match &after {
            Some(p) => served_before(lease, &members, p, cancel)?,
            None => KeySet::default(),
        };
        for (i, hit) in pg.hits.iter().enumerate() {
            let others = match found[i] {
                None => 0,
                Some(k) => {
                    if !seen.insert(k) || served_before.contains(&k) {
                        continue;
                    }
                    members
                        .count
                        .get(&k)
                        .copied()
                        .unwrap_or(1)
                        .saturating_sub(1)
                }
            };
            if skip > 0 {
                skip -= 1;
                continue;
            }
            rows.push(Row {
                hit: hit.clone(),
                others,
            });
            if rows.len() == limit {
                let more = i + 1 < pg.hits.len() || pg.next.is_some();
                return Ok(CollapsedPage {
                    rows,
                    next: more.then_some(positions[i]),
                });
            }
        }
        let Some(next) = pg.next else {
            return Ok(CollapsedPage { rows, next: None });
        };
        if seen.len() >= cap {
            // A short page: the next carries on after the last hit decided.
            return Ok(CollapsedPage {
                rows,
                next: Some(next),
            });
        }
        from = Start::After(next);
    }
}

/// The groups count for `lease`, kept on it for later pages. `exact`
/// asks for the exact count on demand (under [`EXACT_BUDGET`]; out of
/// time, the estimate stands, as for torrents).
pub fn groups(lease: &Lease, exact: bool, cancel: &CancelToken) -> Result<Total> {
    if let Some(t) = lease.kept_groups()
        && (!t.approx || !exact)
    {
        return Ok(t);
    }
    let torrents = if exact {
        lease.exact_total(cancel)?
    } else {
        lease.total()
    };
    let t = groups_with(lease, torrents, exact, count::SAMPLE_ABOVE, cancel)?;
    lease.keep_groups(t);
    Ok(t)
}

pub(crate) fn groups_with(
    lease: &Lease,
    torrents: Total,
    exact: bool,
    sample_above: u32,
    cancel: &CancelToken,
) -> Result<Total> {
    if torrents.value == 0 {
        return Ok(Total {
            value: 0,
            approx: torrents.approx,
        });
    }
    if !torrents.approx {
        let counted = if exact {
            match exact_groups(lease, &cancel.with_budget(EXACT_BUDGET)) {
                Err(Error::OutOfTime) => None,
                r => r?,
            }
        } else {
            exact_groups(lease, cancel)?
        };
        if let Some(value) = counted {
            return Ok(Total {
                value,
                approx: false,
            });
        }
    }
    estimate(lease, torrents.value, sample_above, cancel)
}

/// Every match's group counted, or `None` past [`GROUPS_EXACT_CAP`]
/// distinct keys.
fn exact_groups(lease: &Lease, cancel: &CancelToken) -> Result<Option<usize>> {
    let searcher = lease.searcher();
    let compiled = lease.compiled();
    let weight = compiled
        .query
        .weight(EnableScoring::disabled_from_searcher(searcher))?;
    let mut distinct = KeySet::default();
    let mut alone = 0usize;
    for reader in searcher.segment_readers() {
        cancel.check()?;
        let col = key_column(reader)?;
        let mut m = Matcher::new(weight.as_ref(), reader, compiled, cancel)?;
        let mut over = false;
        m.visit_until(TERMINATED, &mut |docs: &[DocId]| {
            for &d in docs {
                match col.as_ref().and_then(|c| c.first(d)) {
                    Some(k) => {
                        distinct.insert(k);
                    }
                    None => alone += 1,
                }
            }
            over |= distinct.len() > GROUPS_EXACT_CAP;
        })
        .map_err(|e| cancel::stopped_or(cancel, e))?;
        if over {
            return Ok(None);
        }
    }
    Ok(Some(distinct.len() + alone))
}

/// The Horvitz–Thompson estimate (module docs).
fn estimate(
    lease: &Lease,
    torrents: usize,
    sample_above: u32,
    cancel: &CancelToken,
) -> Result<Total> {
    let searcher = lease.searcher();
    let compiled = lease.compiled();
    let weight = compiled
        .query
        .weight(EnableScoring::disabled_from_searcher(searcher))?;
    // Keyless matches are groups of one: their share is counted over
    // every match the slices visit (weighted), not drawn, so the few
    // documents that are most of the groups don't make the variance.
    // The keyed ones go to an A-Res weighted reservoir: each gets the key
    // ln(u)/w, u a fixed scramble of its torrent id (the same sample
    // whatever the segments' order), and the HT_SAMPLE largest are kept.
    let mut heap: BinaryHeap<Drawn> = BinaryHeap::with_capacity(HT_SAMPLE + 1);
    let (mut visited, mut alone) = (0f64, 0f64);
    for reader in searcher.segment_readers() {
        cancel.check()?;
        let col = key_column(reader)?;
        let ids = reader.fast_fields().u64("id")?;
        let max = reader.max_doc();
        let mut m = Matcher::new(weight.as_ref(), reader, compiled, cancel)?;
        let plan = count::slices(max, sample_above);
        let w = plan.as_ref().map_or(1.0, |p| count::scale(max, p));
        let mut offer = |docs: &[DocId]| {
            for &d in docs {
                visited += w;
                let Some(group) = col.as_ref().and_then(|c| c.first(d)) else {
                    alone += w;
                    continue;
                };
                let u = unit(ids.first(d).unwrap_or(u64::from(d)));
                let drawn = Drawn {
                    key: u.ln() / w,
                    group,
                };
                if heap.len() < HT_SAMPLE {
                    heap.push(drawn);
                } else if heap.peek().is_some_and(|low| drawn.key > low.key) {
                    heap.pop();
                    heap.push(drawn);
                }
            }
        };
        let run = match &plan {
            None => m.visit_until(TERMINATED, &mut offer).map(|_| ()),
            Some(slices) => slices.iter().try_for_each(|s| {
                m.seek(s.start);
                m.visit_until(s.end, &mut offer).map(|_| ())
            }),
        };
        run.map_err(|e| cancel::stopped_or(cancel, e))?;
    }
    if visited == 0.0 {
        return Ok(Total {
            value: 0,
            approx: true,
        });
    }
    let alone_share = alone / visited;
    let mut mean = 0.0;
    if !heap.is_empty() {
        let drawn: Vec<Drawn> = heap.into_vec();
        let mut keys: Vec<u64> = drawn.iter().map(|d| d.group).collect();
        keys.sort_unstable();
        keys.dedup();
        let sizes = members(lease, &keys, false, cancel)?.count;
        mean = drawn
            .iter()
            .map(|d| 1.0 / sizes.get(&d.group).copied().unwrap_or(1).max(1) as f64)
            .sum::<f64>()
            / drawn.len() as f64;
    }
    let n = torrents as f64;
    let value = (n * alone_share + n * (1.0 - alone_share) * mean).round() as usize;
    Ok(Total {
        value: value.clamp(1, torrents),
        approx: true,
    })
}

/// A match drawn for the estimate; the heap's top is the smallest key.
#[derive(Debug, Clone, Copy)]
struct Drawn {
    key: f64,
    group: u64,
}

impl PartialEq for Drawn {
    fn eq(&self, other: &Self) -> bool {
        self.key.total_cmp(&other.key).is_eq()
    }
}

impl Eq for Drawn {}

impl PartialOrd for Drawn {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Drawn {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.key.total_cmp(&self.key)
    }
}

/// A fixed scramble of a torrent id into (0, 1] (SplitMix64).
fn unit(id: u64) -> f64 {
    let mut z = id.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    ((z >> 11) as f64 + 1.0) / (1u64 << 53) as f64
}

/// A segment's `group_key` column (`None`: the segment has none).
fn key_column(reader: &tantivy::SegmentReader) -> Result<Option<Column<u64>>> {
    match reader.fast_fields().u64("group_key") {
        Ok(c) => Ok(Some(c)),
        Err(tantivy::TantivyError::FieldNotFound(_)) => Ok(None),
        Err(tantivy::TantivyError::SchemaError(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Reads hits' keys, one column per segment.
struct Keys<'a> {
    searcher: &'a Searcher,
    columns: HashMap<u32, Option<Column<u64>>>,
}

impl<'a> Keys<'a> {
    fn new(searcher: &'a Searcher) -> Keys<'a> {
        Keys {
            searcher,
            columns: HashMap::new(),
        }
    }

    fn get(&mut self, a: DocAddress) -> Result<Option<u64>> {
        let col = match self.columns.entry(a.segment_ord) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(key_column(self.searcher.segment_reader(a.segment_ord))?)
            }
        };
        Ok(col.as_ref().and_then(|c| c.first(a.doc_id)))
    }
}

/// Groups' members among the query's matches.
struct Members {
    count: KeyMap<u64>,
    /// Their addresses, when asked for, at most [`MAX_MEMBERS`] in all.
    addrs: Vec<(u64, DocAddress)>,
}

/// The matches in each of `keys`' groups: the `title_identity` term's
/// postings, checked against the query in doc order, segment by segment.
fn members(lease: &Lease, keys: &[u64], keep: bool, cancel: &CancelToken) -> Result<Members> {
    let mut out = Members {
        count: KeyMap::with_capacity_and_hasher(keys.len(), Keyed::default()),
        addrs: Vec::new(),
    };
    if keys.is_empty() {
        return Ok(out);
    }
    let field = lease.index().fields().title_identity;
    let searcher = lease.searcher();
    let compiled = lease.compiled();
    let weight = compiled
        .query
        .weight(EnableScoring::disabled_from_searcher(searcher))?;
    let mut since = 0u32;
    for (ord, reader) in searcher.segment_readers().iter().enumerate() {
        cancel.check()?;
        let inverted = reader.inverted_index(field)?;
        let mut targets: Vec<(DocId, u64)> = Vec::new();
        for &k in keys {
            let term = Term::from_field_u64(field, k);
            if let Some(mut p) = inverted
                .read_postings(&term, IndexRecordOption::Basic)
                .map_err(Error::Io)?
            {
                let mut d = p.doc();
                while d != TERMINATED {
                    targets.push((d, k));
                    d = p.advance();
                    since += 1;
                    if since >= CHECK_EVERY {
                        since = 0;
                        cancel.check()?;
                    }
                }
            }
        }
        if targets.is_empty() {
            continue;
        }
        targets.sort_unstable();
        let mut m = Matcher::new(weight.as_ref(), reader, compiled, cancel)?;
        for (d, k) in targets {
            if m.contains(d).map_err(|e| cancel::stopped_or(cancel, e))? {
                *out.count.entry(k).or_default() += 1;
                if keep && out.addrs.len() < MAX_MEMBERS {
                    out.addrs.push((k, DocAddress::new(ord as u32, d)));
                }
            }
        }
    }
    Ok(out)
}

/// The groups among `members` with a member at or before `p`.
fn served_before(
    lease: &Lease,
    members: &Members,
    p: &Position,
    cancel: &CancelToken,
) -> Result<KeySet> {
    let addrs: Vec<DocAddress> = members.addrs.iter().map(|(_, a)| *a).collect();
    let before = lease.ranks_at_or_before(&addrs, p, cancel)?;
    Ok(members
        .addrs
        .iter()
        .zip(before)
        .filter(|(_, b)| *b)
        .map(|((k, _), _)| *k)
        .collect())
}

#[cfg(test)]
mod tests;
