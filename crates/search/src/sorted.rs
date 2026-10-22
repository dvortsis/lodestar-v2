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
use tantivy::index::SegmentId;
use tantivy::query::EnableScoring;
use tantivy::{DocAddress, DocId, DocSet, Searcher, SegmentReader};

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
    let value: Box<dyn Fn(DocId) -> Option<u64>> = match order.by {
        By::Fast {
            field,
            signed: true,
        } => {
            let col = fast.i64(field)?;
            Box::new(move |doc| col.first(doc).map(|v| v as u64))
        }
        By::Fast {
            field,
            signed: false,
        } => {
            let col = fast.u64(field)?;
            Box::new(move |doc| col.first(doc))
        }
        By::Hot(c) => {
            let ids = fast.u64("id")?;
            let hot = hot.cloned();
            Box::new(move |doc| {
                let hot = hot.as_ref()?;
                let v = hot.get(c, ids.first(doc)?);
                (v != NEVER).then_some(u64::from(v))
            })
        }
    };
    let signed = matches!(order.by, By::Fast { signed: true, .. });
    let checks = filter::checks(&query.filters, reader, hot)?;
    let alive = reader.alive_bitset();
    let mut heap: BinaryHeap<Reverse<Rank>> = BinaryHeap::with_capacity(k + 1);
    let mut count = 0;
    let mut each = |docs: &[DocId]| {
        for &doc in docs {
            if alive.is_some_and(|a| !a.is_alive(doc)) || !checks.iter().all(|c| c.passes(doc)) {
                continue;
            }
            count += 1;
            if k == 0 {
                continue;
            }
            let v = value(doc);
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
    };
    // This piece's documents only: seek to its start, stop at its end.
    let mut scorer = weight.scorer(reader, 1.0)?;
    let mut doc = if scorer.doc() >= range.start {
        scorer.doc()
    } else {
        scorer.seek(range.start)
    };
    let mut buf = [0; 64];
    while doc < range.end {
        let mut n = 0;
        while doc < range.end && n < buf.len() {
            buf[n] = doc;
            n += 1;
            doc = scorer.advance();
        }
        each(&buf[..n]);
    }
    let top = heap
        .into_iter()
        .map(|Reverse(r)| (r.present, r.value, r.addr.0.1))
        .collect();
    Ok((top, count))
}
