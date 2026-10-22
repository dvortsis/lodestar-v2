//! Top-k by a fast field or a hot column, with a per-segment result cache
//! (architecture §8: browse is "cached per index generation").
//!
//! A filter-only browse visits every match, which at 10M torrents costs
//! far more than its budget. Segments never change once written apart
//! from deletions, so one segment's top-k and match count for a query
//! stay valid until its deletions change. Caching per segment rather than
//! per searcher means a near-real-time commit, which adds a small
//! segment, only costs a search of that segment.
//!
//! Hot columns (§7) change without the segment changing, so a query or
//! order that reads them carries the hot columns' epoch in its key: its
//! cached tops last until the epoch moves, at most a minute after the
//! scraper writes.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::{Arc, Mutex};

use lodestar_store::{Col, HotColumns, NEVER};
use tantivy::columnar::Column;
use tantivy::index::SegmentId;
use tantivy::query::EnableScoring;
use tantivy::{COLLECT_BLOCK_BUFFER_LEN, DocAddress, DocId, DocSet, Searcher, SegmentReader};

use crate::filter;
use crate::index::{Compiled, Result};

/// Cached entries before the cache is emptied.
const CACHE_ENTRIES: usize = 4096;
/// Deeper pages than this aren't cached.
const CACHE_MAX_K: usize = 1000;
/// Documents per parallel piece of a segment. Merged segments are uneven
/// (one can hold most of the index), so a cold top-k splits each into
/// doc ranges and the executor's threads share them.
pub(crate) const PIECE_DOCS: u32 = 1 << 19;

/// Where an order's values come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum By {
    Fast {
        field: &'static str,
        signed: bool,
    },
    /// A hot column, read through the document's torrent id; "never
    /// scraped" has no value.
    Hot(Col),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Order {
    pub by: By,
    pub desc: bool,
}

/// Higher is better: documents with a value first, then by value in the
/// requested direction, then by address so pages are stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    present: bool,
    value: u64,
    addr: Reverse<(u32, DocId)>,
}

type Key = (String, Order, usize, SegmentId, u32);
/// One segment's best documents as (has a value, sort value, doc) and
/// its number of matches. The segment isn't kept: a searcher numbers it.
type SegmentTop = (Vec<(bool, u64, DocId)>, usize);
type Top = Arc<SegmentTop>;

#[derive(Default)]
pub(crate) struct SegmentCache {
    map: Mutex<HashMap<Key, Top>>,
}

impl SegmentCache {
    pub fn clear(&self) {
        self.map.lock().expect("cache lock").clear();
    }
}

/// Maps a fast value to a `u64` that sorts the same way.
fn sortable(v: u64, signed: bool) -> u64 {
    if signed { v ^ (1 << 63) } else { v }
}

/// The `page` (result positions) of `query` ordered by `order`, and the
/// number of matches. `key` identifies the query for the cache, including
/// anything outside the segment it depends on.
pub(crate) fn search(
    searcher: &Searcher,
    cache: &SegmentCache,
    key: &str,
    query: &Compiled,
    order: Order,
    hot: Option<&Arc<HotColumns>>,
    page: std::ops::Range<usize>,
    piece_docs: u32,
) -> Result<(Vec<DocAddress>, usize)> {
    let (offset, k) = (page.start, page.end);
    let limit = k.saturating_sub(offset);
    let weight = query
        .query
        .weight(EnableScoring::disabled_from_searcher(searcher))?;
    let readers = searcher.segment_readers();
    let keys: Vec<Key> = readers
        .iter()
        .map(|reader| {
            (
                key.to_owned(),
                order,
                k,
                reader.segment_id(),
                reader.num_deleted_docs(),
            )
        })
        .collect();
    let mut tops: Vec<Option<Top>> = {
        let map = cache.map.lock().expect("cache lock");
        keys.iter().map(|key| map.get(key).cloned()).collect()
    };
    // The segments not cached, in pieces the threads share.
    let pieces: Vec<(usize, std::ops::Range<DocId>)> = tops
        .iter()
        .enumerate()
        .filter(|(_, t)| t.is_none())
        .flat_map(|(ord, _)| {
            let max = readers[ord].max_doc();
            (0..max.div_ceil(piece_docs).max(1))
                .map(move |i| (ord, i * piece_docs..((i + 1) * piece_docs).min(max)))
        })
        .collect();
    let found = searcher.index().search_executor().map(
        |(ord, range): (usize, std::ops::Range<DocId>)| {
            let top = segment_top(weight.as_ref(), &readers[ord], query, order, hot, k, range)?;
            Ok((ord, top))
        },
        pieces.into_iter(),
    )?;
    let mut merged: HashMap<usize, SegmentTop> = HashMap::new();
    for (ord, (part, count)) in found {
        let e = merged.entry(ord).or_default();
        e.0.extend(part);
        e.1 += count;
    }
    for (ord, (mut part, count)) in merged {
        // Best first, as `Rank` orders them, then the segment's top k.
        part.sort_unstable_by(|a, b| (b.0, b.1, Reverse(b.2)).cmp(&(a.0, a.1, Reverse(a.2))));
        part.truncate(k);
        let top = Arc::new((part, count));
        if k <= CACHE_MAX_K {
            let mut map = cache.map.lock().expect("cache lock");
            if map.len() >= CACHE_ENTRIES {
                map.clear();
            }
            map.insert(keys[ord].clone(), top.clone());
        }
        tops[ord] = Some(top);
    }
    let tops = tops
        .into_iter()
        .enumerate()
        .map(|(ord, t)| (ord as u32, t.expect("every segment searched")));
    let mut total = 0;
    let mut all = Vec::new();
    for (seg, top) in tops {
        total += top.1;
        all.extend(top.0.iter().map(|&(present, value, doc)| Rank {
            present,
            value,
            addr: Reverse((seg, doc)),
        }));
    }
    all.sort_unstable_by(|a, b| b.cmp(a));
    let page = all
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|r| DocAddress::new(r.addr.0.0, r.addr.0.1))
        .collect();
    Ok((page, total))
}

/// Where one segment's sort values come from.
enum Values {
    I64(Column<i64>),
    U64(Column<u64>),
    /// The id column, read through the hot columns (`None`: no values).
    Hot(Column<u64>, Option<Arc<HotColumns>>, Col),
}

impl Values {
    /// The values of `docs` into `out`: a block at a time, so the column
    /// reads (and the hot columns' random ones) overlap instead of
    /// waiting on each other.
    fn fill(&self, docs: &[DocId], out: &mut [Option<u64>]) {
        match self {
            Values::U64(col) => col.first_vals(docs, out),
            Values::I64(col) => {
                let mut tmp = [None; COLLECT_BLOCK_BUFFER_LEN];
                col.first_vals(docs, &mut tmp[..docs.len()]);
                for (o, v) in out.iter_mut().zip(&tmp) {
                    *o = v.map(|v| v as u64);
                }
            }
            Values::Hot(ids, hot, c) => {
                let Some(hot) = hot else {
                    out.fill(None);
                    return;
                };
                ids.first_vals(docs, out);
                for o in out.iter_mut() {
                    *o = o.and_then(|id| {
                        let v = hot.get(*c, id);
                        (v != NEVER).then_some(u64::from(v))
                    });
                }
            }
        }
    }
}

fn segment_top(
    weight: &dyn tantivy::query::Weight,
    reader: &SegmentReader,
    query: &Compiled,
    order: Order,
    hot: Option<&Arc<HotColumns>>,
    k: usize,
    range: std::ops::Range<DocId>,
) -> tantivy::Result<SegmentTop> {
    let fast = reader.fast_fields();
    let values = match order.by {
        By::Fast {
            field,
            signed: true,
        } => Values::I64(fast.i64(field)?),
        By::Fast {
            field,
            signed: false,
        } => Values::U64(fast.u64(field)?),
        By::Hot(c) => Values::Hot(fast.u64("id")?, hot.cloned(), c),
    };
    let signed = matches!(order.by, By::Fast { signed: true, .. });
    let checks = filter::checks(&query.filters, reader, hot)?;
    let alive = reader.alive_bitset();
    let mut heap: BinaryHeap<Reverse<Rank>> = BinaryHeap::with_capacity(k + 1);
    let mut count = 0;
    // This piece's documents only: seek to its start, stop at its end.
    let mut scorer = weight.scorer(reader, 1.0)?;
    if scorer.doc() < range.start {
        scorer.seek(range.start);
    }
    let mut buf = [0; COLLECT_BLOCK_BUFFER_LEN];
    let mut kept = [0; COLLECT_BLOCK_BUFFER_LEN];
    let mut vals = [None; COLLECT_BLOCK_BUFFER_LEN];
    loop {
        let n = scorer.fill_buffer(&mut buf);
        let inside = buf[..n].partition_point(|d| *d < range.end);
        let mut m = 0;
        for &doc in &buf[..inside] {
            if alive.is_none_or(|a| a.is_alive(doc)) && checks.iter().all(|c| c.passes(doc)) {
                kept[m] = doc;
                m += 1;
            }
        }
        count += m;
        if k > 0 && m > 0 {
            values.fill(&kept[..m], &mut vals[..m]);
            for (&doc, &v) in kept[..m].iter().zip(&vals[..m]) {
                let mut key = v.map_or(0, |v| sortable(v, signed));
                if !order.desc {
                    key = !key;
                }
                let rank = Rank {
                    present: v.is_some(),
                    value: key,
                    addr: Reverse((0, doc)),
                };
                if heap.len() < k {
                    heap.push(Reverse(rank));
                } else if heap.peek().is_some_and(|worst| rank > worst.0) {
                    heap.pop();
                    heap.push(Reverse(rank));
                }
            }
        }
        if inside < n || n < buf.len() {
            break;
        }
    }
    let top = heap
        .into_iter()
        .map(|Reverse(r)| (r.present, r.value, r.addr.0.1))
        .collect();
    Ok((top, count))
}
