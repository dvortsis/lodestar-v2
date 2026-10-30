//! A phrase query whose scorer accepts `seek_danger` behind its current
//! document (docs/decisions/2026-10-30-X-ci-health.md).
//!
//! Tantivy 0.27's `Exclude` (a `MustNot` clause) and `Disjunction` call
//! `seek_danger(target)` on a child with the parent's candidate, which
//! can be behind the child's own document: a phrase that starts on a
//! later document, or one that matches nothing in the segment and is
//! already `TERMINATED`. The `DocSet` contract allows that, and the
//! default `seek_danger` answers it, but `PhraseScorer::seek_danger`
//! asserts `target >= doc()`, so a debug build panics on `-"a b"`.
//! In release the call falls through to the posting lists' default
//! `seek_danger`, which answers correctly; the guard makes both builds
//! take the same path.

use tantivy::query::{EnableScoring, Explanation, PhraseQuery, Query, Scorer, Weight};
use tantivy::{DocId, DocSet, Score, SeekDangerResult, SegmentReader, Term};

/// A phrase whose scorers answer a `seek_danger`
/// behind them the way the default does: not found, with their current
/// document as the lower bound.
#[derive(Debug, Clone)]
pub(crate) struct SeekGuard(pub PhraseQuery);

impl Query for SeekGuard {
    fn weight(&self, scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        Ok(Box::new(GuardWeight(self.0.weight(scoring)?)))
    }

    fn query_terms<'a>(&'a self, visitor: &mut dyn FnMut(&'a Term, bool)) {
        self.0.query_terms(visitor);
    }
}

struct GuardWeight(Box<dyn Weight>);

impl Weight for GuardWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        Ok(Box::new(GuardScorer(self.0.scorer(reader, boost)?)))
    }

    fn scorer_danger(
        &self,
        reader: &SegmentReader,
        target: DocId,
        boost: Score,
    ) -> tantivy::Result<(SeekDangerResult, Box<dyn Scorer>)> {
        let (found, scorer) = self.0.scorer_danger(reader, target, boost)?;
        Ok((found, Box::new(GuardScorer(scorer))))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        self.0.explain(reader, doc)
    }
}

struct GuardScorer(Box<dyn Scorer>);

impl DocSet for GuardScorer {
    fn advance(&mut self) -> DocId {
        self.0.advance()
    }

    fn seek(&mut self, target: DocId) -> DocId {
        self.0.seek(target)
    }

    fn seek_danger(&mut self, target: DocId) -> SeekDangerResult {
        // A phrase's `doc()` is its first posting list's document, which
        // only moves forward to a seek target or past a document checked
        // for the phrase, so no match lies between `target` and it: it's
        // a valid lower bound (TERMINATED included).
        let doc = self.0.doc();
        if target < doc {
            return SeekDangerResult::SeekLowerBound(doc);
        }
        self.0.seek_danger(target)
    }

    fn doc(&self) -> DocId {
        self.0.doc()
    }

    fn size_hint(&self) -> u32 {
        self.0.size_hint()
    }

    fn cost(&self) -> u64 {
        self.0.cost()
    }
}

impl Scorer for GuardScorer {
    fn score(&mut self) -> Score {
        self.0.score()
    }
}
