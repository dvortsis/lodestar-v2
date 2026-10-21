//! Top-k by a fast field, with a per-segment result cache (architecture
//! §8: browse is "cached per index generation").
//!
//! A filter-only browse visits every match, which at 10M torrents costs
//! far more than its budget. Segments never change once written apart
//! from deletions, so one segment's top-k and match count for a query
//! stay valid until its deletions change. Caching per segment rather than
//! per searcher means a near-real-time commit, which adds a small
//! segment, only costs a search of that segment.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::{Arc, Mutex};

use tantivy::index::SegmentId;
use tantivy::query::EnableScoring;
use tantivy::{DocAddress, DocId, Searcher, SegmentReader};

use crate::filter;
use crate::index::{Compiled, Result};

/// Cached entries before the cache is emptied.
const CACHE_ENTRIES: usize = 4096;
/// Deeper pages than this aren't cached.
const CACHE_MAX_K: usize = 1000;

/// A fast field to order by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Order {
    pub field: &'static str,
    pub signed: bool,
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

/// The page `offset..offset + limit` of `query` ordered by `order`, and
/// the number of matches.
pub(crate) fn search(
    searcher: &Searcher,
    cache: &SegmentCache,
    query: &Compiled,
    order: Order,
    limit: usize,
    offset: usize,
) -> Result<(Vec<DocAddress>, usize)> {
    let k = offset + limit;
    let weight = query
        .query
        .weight(EnableScoring::disabled_from_searcher(searcher))?;
    let segments = searcher.segment_readers().iter().enumerate();
    let tops = searcher.index().search_executor().map(
        |(ord, reader): (usize, &SegmentReader)| {
            let key = (
                query.key.clone(),
                order,
                k,
                reader.segment_id(),
                reader.num_deleted_docs(),
            );
            let cached = cache.map.lock().expect("cache lock").get(&key).cloned();
            let top = match cached {
                Some(top) => top,
                None => {
                    let top = Arc::new(segment_top(weight.as_ref(), reader, query, order, k)?);
                    if k <= CACHE_MAX_K {
                        let mut map = cache.map.lock().expect("cache lock");
                        if map.len() >= CACHE_ENTRIES {
                            map.clear();
                        }
                        map.insert(key, top.clone());
                    }
                    top
                }
            };
            Ok((ord as u32, top))
        },
        segments,
    )?;
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
    k: usize,
) -> tantivy::Result<SegmentTop> {
    let fast = reader.fast_fields();
    let value: Box<dyn Fn(DocId) -> Option<u64>> = if order.signed {
        let col = fast.i64(order.field)?;
        Box::new(move |doc| col.first(doc).map(|v| v as u64))
    } else {
        let col = fast.u64(order.field)?;
        Box::new(move |doc| col.first(doc))
    };
    let checks = filter::checks(&query.filters, reader)?;
    let alive = reader.alive_bitset();
    let mut heap: BinaryHeap<Reverse<Rank>> = BinaryHeap::with_capacity(k + 1);
    let mut count = 0;
    weight.for_each_no_score(reader, &mut |docs| {
        for &doc in docs {
            if alive.is_some_and(|a| !a.is_alive(doc)) || !checks.iter().all(|c| c.passes(doc)) {
                continue;
            }
            count += 1;
            if k == 0 {
                continue;
            }
            let v = value(doc);
            let mut key = v.map_or(0, |v| sortable(v, order.signed));
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
    })?;
    let top = heap
        .into_iter()
        .map(|Reverse(r)| (r.present, r.value, r.addr.0.1))
        .collect();
    Ok((top, count))
}
