//! `?explain=1` (architecture §8): each hit's score taken apart, from the
//! same lease and page a search serves, so explaining never changes the
//! ranking.
//!
//! Relevance today is two-phase (§8, B5a):
//! - **Phase 1** is BM25 over the combined name-and-paths field (`text`),
//!   the scores of fielded words and constant scores of filters, plus
//!   [`STEM_SCORE`] for a word matched only by its stem in the title
//!   (the plurals fallback).
//! - **Phase 2** re-ranks windows of [`RERANK_WINDOW`] phase-1 candidates
//!   by adding each unfielded word's **name** boost (BM25 on `name`,
//!   ×[`NAME_BOOST`]) and **title** boost (BM25 on the parsed title,
//!   ×[`TITLE_BOOST`]).
//!
//! So `score = phase1 + name + title`, to float rounding. The other §8
//! features (phrase/proximity, exact name, the file contribution cap)
//! and the quality priors (seeders, freshness, junk, trackers, swarm
//! consistency) aren't part of ranking yet; [`Explained`] has no value
//! for them, and the API reports them as null. Under a field sort the
//! order is the field's, and no hit has relevance components.

use tantivy::query::{BoostQuery, EnableScoring, Query, Weight};
use tantivy::{DocId, DocSet, SegmentReader};

use crate::cancel::CancelToken;
use crate::compile;
use crate::index::{self, RERANK_WINDOW, Result, Sort};
use crate::lease::{Lease, Page, Start};
use crate::query::{Node, TextField};
use crate::schema::{NAME_BOOST, PATH_BOOST, STEM_SCORE, TITLE_BOOST};
use crate::tokenizer;

type Q = Box<dyn Query>;

/// Hits explained per request, at most: the first page only.
pub const EXPLAIN_MAX: usize = 100;

/// The ranking's constants (`schema.rs`, `index.rs`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weights {
    pub name: f32,
    pub path: f32,
    pub title: f32,
    pub stem_score: f32,
    pub rerank_window: usize,
}

pub const WEIGHTS: Weights = Weights {
    name: NAME_BOOST,
    path: PATH_BOOST,
    title: TITLE_BOOST,
    stem_score: STEM_SCORE,
    rerank_window: RERANK_WINDOW,
};

/// One hit's score in parts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Explained {
    pub id: u64,
    /// `None` under a field sort: the order isn't by score.
    pub parts: Option<Parts>,
    /// The score the hit was ranked by (0 under a field sort).
    pub score: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Parts {
    pub phase1: f32,
    /// Phase 2's name boosts, summed over the unfielded words (0 when
    /// there's no phase 2).
    pub name: f32,
    /// Phase 2's title boosts, likewise.
    pub title: f32,
}

/// A first page and its explanation, hit by hit in the page's order.
#[derive(Debug, Clone, PartialEq)]
pub struct Explanation {
    pub page: Page,
    /// The sort the lease ranks by.
    pub sort: Sort,
    /// Whether phase 2 ran: the query has unfielded words.
    pub two_phase: bool,
    pub hits: Vec<Explained>,
}

/// The first `limit` hits of `lease` (at most [`EXPLAIN_MAX`]) and how
/// each was scored. The page is exactly [`Lease::page`]'s.
pub fn explain(lease: &Lease, limit: usize, cancel: &CancelToken) -> Result<Explanation> {
    let limit = limit.clamp(1, EXPLAIN_MAX);
    let (page, addrs) = lease.page_at(Start::Offset(0), limit, cancel)?;
    let sort = lease.sort();
    let two_phase = lease.compiled().boosts.is_some();
    let by_score = index::sort_field(sort).is_none();
    let searcher = lease.searcher();
    let mut hits = Vec::with_capacity(page.hits.len());
    if by_score {
        let index = lease.index();
        let fields = index.fields();
        let (names, titles): (Vec<Q>, Vec<Q>) = if two_phase {
            boost_words(lease.node())
                .iter()
                .map(|toks| {
                    let q = |field, boost| -> Q {
                        Box::new(BoostQuery::new(
                            compile::word_query(field, toks, true),
                            boost,
                        ))
                    };
                    (q(fields.name, NAME_BOOST), q(fields.title, TITLE_BOOST))
                })
                .unzip()
        } else {
            (Vec::new(), Vec::new())
        };
        // Each part's weight once; then per segment, one scorer per part
        // walking that segment's hits in doc order (as `index::rerank`
        // does), so the cost is a few seeks per hit.
        let enable = EnableScoring::enabled_from_searcher(searcher);
        let phase1 = lease.compiled().query.weight(enable)?;
        let names = weights(&names, enable)?;
        let titles = weights(&titles, enable)?;
        let mut order: Vec<usize> = (0..addrs.len()).collect();
        order.sort_unstable_by_key(|&i| addrs[i]);
        let mut parts = vec![Parts::default(); addrs.len()];
        let mut at = 0;
        while at < order.len() {
            cancel.check()?;
            let seg = addrs[order[at]].segment_ord;
            let end = at
                + order[at..]
                    .iter()
                    .take_while(|&&i| addrs[i].segment_ord == seg)
                    .count();
            let docs: Vec<(usize, DocId)> = order[at..end]
                .iter()
                .map(|&i| (i, addrs[i].doc_id))
                .collect();
            let reader = searcher.segment_reader(seg);
            for (i, v) in scores(phase1.as_ref(), reader, &docs)? {
                parts[i].phase1 = v;
            }
            for w in &names {
                for (i, v) in scores(w.as_ref(), reader, &docs)? {
                    parts[i].name += v;
                }
            }
            for w in &titles {
                for (i, v) in scores(w.as_ref(), reader, &docs)? {
                    parts[i].title += v;
                }
            }
            at = end;
        }
        hits.extend(page.hits.iter().zip(parts).map(|(h, p)| Explained {
            id: h.id,
            parts: Some(p),
            score: h.score,
        }));
    } else {
        hits.extend(page.hits.iter().map(|h| Explained {
            id: h.id,
            parts: None,
            score: h.score,
        }));
    }
    Ok(Explanation {
        page,
        sort,
        two_phase,
        hits,
    })
}

/// The tokens of each word phase 2 boosts: the top-level unfielded words
/// with any tokens, as `compile::compile` picks them.
fn boost_words(node: &Node) -> Vec<Vec<tokenizer::Tok>> {
    let of: Vec<&Node> = match node {
        Node::And { of } => of.iter().collect(),
        n => vec![n],
    };
    of.into_iter()
        .filter_map(|n| match n {
            Node::Word {
                field: TextField::Any,
                word,
            } => Some(tokenizer::tokenize(word)),
            _ => None,
        })
        .filter(|t| !t.is_empty())
        .collect()
}

fn weights(queries: &[Q], enable: EnableScoring<'_>) -> Result<Vec<Box<dyn Weight>>> {
    Ok(queries
        .iter()
        .map(|q| q.weight(enable))
        .collect::<tantivy::Result<_>>()?)
}

/// `weight`'s score of each of `docs` (in doc order) in one segment, with
/// one scorer; 0 where it doesn't match.
fn scores(
    weight: &dyn Weight,
    reader: &SegmentReader,
    docs: &[(usize, DocId)],
) -> Result<Vec<(usize, f32)>> {
    let mut scorer = weight.scorer(reader, 1.0)?;
    let mut out = Vec::with_capacity(docs.len());
    for &(i, doc) in docs {
        if scorer.doc() < doc {
            scorer.seek(doc);
        }
        out.push((
            i,
            if scorer.doc() == doc {
                scorer.score()
            } else {
                0.0
            },
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::testing::index_with;

    const NOW: i64 = 1_700_000_100;

    fn lease(index: &Arc<crate::SearchIndex>, q: &str, sort: Sort) -> Lease {
        let node = crate::parse(q).unwrap();
        Lease::new(index.clone(), &node, NOW, sort, &CancelToken::new()).unwrap()
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() <= 1e-4 * a.abs().max(b.abs()).max(1.0)
    }

    #[test]
    fn the_parts_add_up_to_the_score_and_the_page_is_the_searchs() {
        let (index, _) = index_with(2_000, 3);
        let index = Arc::new(index);
        for q in ["common", "half common", "name:half", "", "half size:>1100"] {
            let l = lease(&index, q, Sort::Relevance);
            let cancel = CancelToken::new();
            let e = explain(&l, 50, &cancel).unwrap();
            assert_eq!(
                e.page,
                l.page(Start::Offset(0), 50, &cancel).unwrap(),
                "{q}"
            );
            assert!(!e.hits.is_empty(), "{q}");
            assert_eq!(e.two_phase, l.compiled().boosts.is_some(), "{q}");
            for h in &e.hits {
                let p = h.parts.expect("relevance");
                if !e.two_phase {
                    assert_eq!((p.name, p.title), (0.0, 0.0), "{q}");
                }
                assert!(
                    close(p.phase1 + p.name + p.title, h.score),
                    "{q}: {p:?} vs {}",
                    h.score
                );
            }
        }
    }

    #[test]
    fn a_field_sort_has_no_relevance_parts_and_explain_is_capped() {
        let (index, _) = index_with(500, 1);
        let index = Arc::new(index);
        let l = lease(&index, "common", Sort::Size { desc: true });
        let e = explain(&l, 50, &CancelToken::new()).unwrap();
        assert!(e.hits.iter().all(|h| h.parts.is_none()));
        let l = lease(&index, "", Sort::Relevance);
        let e = explain(&l, 1_000, &CancelToken::new()).unwrap();
        assert_eq!(e.hits.len(), EXPLAIN_MAX);
        assert_eq!(e.page.hits.len(), EXPLAIN_MAX);
    }
}
