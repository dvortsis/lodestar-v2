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

use crate::cancel::{self, CancelToken};
use crate::count::{self, Matcher, Total};
use crate::cursor::Position;
use crate::index::{Error, Hit, Result};
use crate::lease::{EXACT_BUDGET, Lease, Start, WindowBound};
use crate::population::Availability;

/// Groups decided per page at most (served, or skipped as served
/// before): bounds a page's work and memory whatever the query.
pub const MAX_SCAN_GROUPS: usize = 4_096;
/// Hits walked per page at most: groups already decided (repeats of
/// one, or served before) don't count against [`MAX_SCAN_GROUPS`], so
/// this bounds the walk through them.
pub const MAX_SCAN_HITS: usize = 16_384;
/// Member addresses held at once for the served-before check: past it
/// they're checked and let go a batch at a time. Members are found by
/// leapfrogging, never loaded whole, so this bounds a page's memory for
/// the check however large a group is.
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
///
/// A page can be short, or empty, and still have a next: it ended at a
/// bound ([`MAX_SCAN_GROUPS`], [`MAX_SCAN_HITS`]), and the next page
/// carries on after the last hit it decided.
#[derive(Debug, Clone, PartialEq)]
pub struct CollapsedPage {
    pub rows: Vec<Row>,
    pub next: Option<Position>,
    /// Groups an `Offset` start passed over before the first row: the
    /// offset itself, or fewer when the page ended at a bound first
    /// (its next page then starts at that many groups in). 0 after a
    /// cursor.
    pub skipped: usize,
}

/// Up to `limit` groups of `lease`'s results from `start` (`Offset`
/// counts groups). The caller has checked [`available`].
pub fn page(
    lease: &Lease,
    start: Start,
    limit: usize,
    cancel: &CancelToken,
) -> Result<CollapsedPage> {
    page_with(lease, start, limit, Bounds::default(), cancel)
}

/// What one page may decide and walk before it ends short.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bounds {
    pub groups: usize,
    pub hits: usize,
    /// Member addresses held at once for the served-before check.
    pub members: usize,
}

impl Default for Bounds {
    fn default() -> Self {
        Bounds {
            groups: MAX_SCAN_GROUPS,
            hits: MAX_SCAN_HITS,
            members: MAX_MEMBERS,
        }
    }
}

pub(crate) fn page_with(
    lease: &Lease,
    start: Start,
    limit: usize,
    bounds: Bounds,
    cancel: &CancelToken,
) -> Result<CollapsedPage> {
    let limit = limit.max(1);
    let (after, offset) = match start {
        Start::After(p) => (Some(p), 0),
        Start::Offset(o) => (None, o),
    };
    let mut skip = offset;
    let cap = bounds.groups.max(skip + limit);
    let mut keys = Keys::new(lease.searcher());
    // Groups decided on this page: served, or skipped as served before.
    let mut seen = KeySet::default();
    let mut rows = Vec::with_capacity(limit);
    let mut walked = 0usize;
    // The cursor's two-phase window bound, found once for the page.
    let mut bound = WindowBound::default();
    // An offset counts groups: the walk starts from the top.
    let mut from = match start {
        Start::After(p) => Start::After(p),
        Start::Offset(_) => Start::Offset(0),
    };
    let short = |rows: Vec<Row>, skip: usize, next: Position| CollapsedPage {
        rows,
        next: Some(next),
        skipped: offset - skip,
    };
    loop {
        cancel.check()?;
        let need = skip + limit - rows.len();
        let chunk = (need * 2).clamp(CHUNK_MIN, CHUNK_MAX);
        let (pg, addrs, positions) = lease.page_ranked(from, chunk, cancel)?;
        walked += pg.hits.len();
        let found: Vec<Option<u64>> = addrs.iter().map(|a| keys.get(*a)).collect::<Result<_>>()?;
        // This chunk's groups not yet decided, in order.
        let mut fresh = Vec::new();
        let mut listed = KeySet::default();
        for k in found.iter().flatten() {
            if !seen.contains(k) && listed.insert(*k) {
                fresh.push(*k);
            }
        }
        let members = members(
            lease,
            &fresh,
            after.as_ref(),
            bounds.members,
            &mut bound,
            cancel,
        )?;
        for (i, hit) in pg.hits.iter().enumerate() {
            // Whether this hit starts a row: keyless, or its group's first
            // hit here and not served before.
            let fresh_row = match found[i] {
                None => true,
                Some(k) => !seen.contains(&k) && !members.served.contains(&k),
            };
            if let Some(k) = found[i] {
                seen.insert(k);
            }
            if !fresh_row {
                continue;
            }
            let others = found[i].map_or(0, |k| {
                members
                    .count
                    .get(&k)
                    .copied()
                    .unwrap_or(1)
                    .saturating_sub(1)
            });
            if skip > 0 {
                skip -= 1;
                continue;
            }
            rows.push(Row {
                hit: hit.clone(),
                others,
            });
            if rows.len() == limit {
                // More only if a later hit here starts a row, or more
                // hits follow (which may all be groups already served).
                let mut ahead = KeySet::default();
                let more = (i + 1..pg.hits.len()).any(|j| match found[j] {
                    None => true,
                    Some(k) => {
                        !seen.contains(&k) && !members.served.contains(&k) && ahead.insert(k)
                    }
                }) || pg.next.is_some();
                return Ok(CollapsedPage {
                    rows,
                    next: more.then_some(positions[i]),
                    skipped: offset,
                });
            }
        }
        let Some(next) = pg.next else {
            return Ok(CollapsedPage {
                rows,
                next: None,
                skipped: offset - skip,
            });
        };
        if seen.len() >= cap || walked >= bounds.hits {
            // A short page: the next carries on after the last hit
            // decided, so nothing is skipped.
            return Ok(short(rows, skip, next));
        }
        from = Start::After(next);
    }
}

/// The groups count for `lease`, kept on it for later pages. `exact`
/// asks for the exact count on demand: see [`counts`].
pub fn groups(lease: &Lease, exact: bool, cancel: &CancelToken) -> Result<Total> {
    Ok(counts(lease, exact, cancel)?.1)
}

/// The torrents and groups counts of a collapsed search, the groups count
/// kept on the lease for later pages. With `exact` (on demand), both are
/// counted under **one** [`EXACT_BUDGET`]: the torrents count first (kept
/// on the lease, as `Lease::exact_total` keeps it), then the groups. Out
/// of time, an estimate stands for what wasn't counted, as for torrents.
/// A collapsed search reads its torrents count here, not from
/// `Lease::exact_total`, so nothing is counted twice or under a second
/// budget.
pub fn counts(lease: &Lease, exact: bool, cancel: &CancelToken) -> Result<(Total, Total)> {
    let budget = exact.then(|| cancel.with_budget(EXACT_BUDGET));
    counts_within(lease, budget.as_ref(), cancel)
}

/// [`counts`] under `budget` (`None`: no count on demand).
pub(crate) fn counts_within(
    lease: &Lease,
    budget: Option<&CancelToken>,
    cancel: &CancelToken,
) -> Result<(Total, Total)> {
    if let Some(t) = lease.kept_groups()
        && (!t.approx || budget.is_none())
    {
        return Ok((lease.total(), t));
    }
    let torrents = match budget {
        Some(b) => lease.exact_total_within(b, EXACT_BUDGET)?,
        None => lease.total(),
    };
    let groups = groups_with(lease, torrents, budget, count::SAMPLE_ABOVE, cancel)?;
    lease.keep_groups(groups);
    Ok((torrents, groups))
}

/// The groups count once the torrents count is `torrents`: exact while
/// that is (under `budget` for a count on demand, which may run out),
/// else the estimate, which runs under `cancel` alone.
pub(crate) fn groups_with(
    lease: &Lease,
    torrents: Total,
    budget: Option<&CancelToken>,
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
        let counted = match budget {
            Some(b) => match exact_groups(lease, b) {
                Err(Error::OutOfTime) => None,
                r => r?,
            },
            None => exact_groups(lease, cancel)?,
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
    exact_groups_capped(lease, GROUPS_EXACT_CAP, cancel)
}

pub(crate) fn exact_groups_capped(
    lease: &Lease,
    cap: usize,
    cancel: &CancelToken,
) -> Result<Option<usize>> {
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
            if over {
                return;
            }
            for &d in docs {
                match col.as_ref().and_then(|c| c.first(d)) {
                    Some(k) => {
                        distinct.insert(k);
                    }
                    None => alone += 1,
                }
            }
            // Past the cap nothing more is held: the rest of the segment
            // is only walked (a visit can't stop partway).
            over |= distinct.len() > cap;
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
        // No match fell in the slices: at least one group, since there
        // are matches.
        return Ok(Total {
            value: torrents.min(1),
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
        let sizes = members(
            lease,
            &keys,
            None,
            MAX_MEMBERS,
            &mut WindowBound::default(),
            cancel,
        )?
        .count;
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
pub(crate) struct Members {
    count: KeyMap<u64>,
    /// The groups with a member at or before the cursor position asked
    /// about: served on an earlier page.
    served: KeySet,
    /// Leapfrog steps taken (a seek of the query or of a posting list):
    /// the walk's cost, for tests.
    pub(crate) steps: u64,
    /// The most member addresses held at once, for tests.
    pub(crate) held: usize,
}

/// The matches in each of `keys`' groups. Per segment, the groups'
/// `title_identity` postings are merged in doc order and leapfrogged
/// against the query: each step seeks whichever side is behind to the
/// other's document, so the cost follows the smaller of a group and the
/// query's matches, and no posting list is ever loaded whole. With
/// `after`, also which groups have a member ranking at or before it,
/// checked `batch` addresses at a time (so at most `batch` are held),
/// and only for groups not already known served.
pub(crate) fn members(
    lease: &Lease,
    keys: &[u64],
    after: Option<&Position>,
    batch: usize,
    bound: &mut WindowBound,
    cancel: &CancelToken,
) -> Result<Members> {
    use std::cmp::Reverse;
    let mut out = Members {
        count: KeyMap::with_capacity_and_hasher(keys.len(), Keyed::default()),
        served: KeySet::default(),
        steps: 0,
        held: 0,
    };
    if keys.is_empty() {
        return Ok(out);
    }
    let batch = batch.max(1);
    let field = lease.index().fields().title_identity;
    let searcher = lease.searcher();
    let compiled = lease.compiled();
    let weight = compiled
        .query
        .weight(EnableScoring::disabled_from_searcher(searcher))?;
    let mut pending: Vec<(u64, DocAddress)> = Vec::new();
    let stopped = |e| cancel::stopped_or(cancel, e);
    for (ord, reader) in searcher.segment_readers().iter().enumerate() {
        cancel.check()?;
        let inverted = reader.inverted_index(field)?;
        let mut lists = Vec::new();
        let mut heads: BinaryHeap<Reverse<(DocId, usize)>> = BinaryHeap::new();
        for &k in keys {
            let term = Term::from_field_u64(field, k);
            if let Some(p) = inverted
                .read_postings(&term, IndexRecordOption::Basic)
                .map_err(Error::Io)?
            {
                let d = p.doc();
                if d != TERMINATED {
                    heads.push(Reverse((d, lists.len())));
                    lists.push((p, k));
                }
            }
        }
        if heads.is_empty() {
            continue;
        }
        let mut m = Matcher::new(weight.as_ref(), reader, compiled, cancel)?;
        while let Some(Reverse((head, i))) = heads.pop() {
            // The query to the group's next member, or past it.
            let at = m.seek_to(head).map_err(stopped)?;
            out.steps += 1;
            if at == TERMINATED {
                break;
            }
            let (list, k) = &mut lists[i];
            let next = if at == head {
                if m.passes_at(head) {
                    *out.count.entry(*k).or_default() += 1;
                    if after.is_some() && !out.served.contains(k) {
                        pending.push((*k, DocAddress::new(ord as u32, head)));
                        out.held = out.held.max(pending.len());
                        if pending.len() >= batch {
                            flush(lease, after, &mut pending, &mut out.served, bound, cancel)?;
                        }
                    }
                }
                list.advance()
            } else {
                // The group's list to the query's next match.
                out.steps += 1;
                list.seek(at)
            };
            if next != TERMINATED {
                heads.push(Reverse((next, i)));
            }
        }
    }
    flush(lease, after, &mut pending, &mut out.served, bound, cancel)?;
    Ok(out)
}

/// Checks `pending` members against the cursor position `after`, marking
/// their groups served, and empties it.
fn flush(
    lease: &Lease,
    after: Option<&Position>,
    pending: &mut Vec<(u64, DocAddress)>,
    served: &mut KeySet,
    bound: &mut WindowBound,
    cancel: &CancelToken,
) -> Result<()> {
    let Some(p) = after else {
        pending.clear();
        return Ok(());
    };
    // Groups already known served need no more checking.
    pending.retain(|(k, _)| !served.contains(k));
    if pending.is_empty() {
        return Ok(());
    }
    let addrs: Vec<DocAddress> = pending.iter().map(|(_, a)| *a).collect();
    let before = lease.ranks_at_or_before(&addrs, p, bound, cancel)?;
    for ((k, _), b) in pending.iter().zip(before) {
        if b {
            served.insert(*k);
        }
    }
    pending.clear();
    Ok(())
}

#[cfg(test)]
mod tests;
