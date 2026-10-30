//! A phrase query whose scorer accepts `seek_danger` behind its current
//! document (docs/decisions/2026-10-30-X-ci-health.md).
//!
//! Tantivy 0.27's `Exclude` (a `MustNot` clause) calls
//! `seek_danger(target)` on the excluded scorer with the required side's
//! candidate, and `Intersection`, `BoostQuery`, `ConstScoreQuery` and
//! `RequiredOptional` forward that call to their children (`-(a "b c")`).
//! The target can be behind the phrase's own document: a phrase that starts on a
//! later document, or one that matches nothing in the segment and is
//! already `TERMINATED`. The `DocSet` contract allows that, and the
//! default `seek_danger` answers it, but `PhraseScorer::seek_danger`
//! asserts `target >= doc()`, so a debug build panics on `-"a b"`.
//! In release the call falls through to the intersection's lead posting
//! list's default `seek_danger`, which answers correctly; the guard makes both builds
//! take the same path.

use tantivy::query::{EmptyScorer, EnableScoring, Explanation, PhraseQuery, Query, Scorer, Weight};
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
        Ok(guard(self.0.scorer(reader, boost)?))
    }

    fn scorer_danger(
        &self,
        reader: &SegmentReader,
        target: DocId,
        boost: Score,
    ) -> tantivy::Result<(SeekDangerResult, Box<dyn Scorer>)> {
        let (found, scorer) = self.0.scorer_danger(reader, target, boost)?;
        Ok((found, guard(scorer)))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        self.0.explain(reader, doc)
    }
}

/// Wraps a phrase's scorer, except an `EmptyScorer` (a term the segment
/// lacks): `BooleanWeight` recognises that one and prunes the clause.
fn guard(scorer: Box<dyn Scorer>) -> Box<dyn Scorer> {
    if scorer.is::<EmptyScorer>() {
        scorer
    } else {
        Box::new(GuardScorer(scorer))
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
        // A phrase's `doc()` is its intersection's lead posting list's
        // document, which only moves forward to a seek target or past a document checked
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

#[cfg(test)]
mod tests {
    use tantivy::query::{BooleanQuery, Occur, TermQuery};
    use tantivy::schema::{Field, IndexRecordOption, Schema, TEXT};
    use tantivy::{Index, IndexWriter, TERMINATED, TantivyDocument, doc};

    use super::*;

    /// "a b" matches docs 2 and 4, "x b" docs 0 and 3, "b a" doc 3; "b x"
    /// has both terms but never in that order; "a zz" has a term no
    /// document holds.
    const DOCS: &[&str] = &["a x b", "c d", "a b", "x b a", "a b c", "x"];

    fn index() -> (Index, Field) {
        let mut schema = Schema::builder();
        let body = schema.add_text_field("body", TEXT);
        let index = Index::create_in_ram(schema.build());
        let mut w: IndexWriter<TantivyDocument> = index.writer(15 << 20).unwrap();
        for d in DOCS {
            w.add_document(doc!(body => *d)).unwrap();
        }
        w.commit().unwrap();
        (index, body)
    }

    fn phrase(body: Field, words: &str) -> SeekGuard {
        SeekGuard(PhraseQuery::new(
            words
                .split(' ')
                .map(|w| Term::from_field_text(body, w))
                .collect(),
        ))
    }

    fn scorer(index: &Index, q: &dyn Query) -> Box<dyn Scorer> {
        let searcher = index.reader().unwrap().searcher();
        let weight = q
            .weight(EnableScoring::disabled_from_searcher(&searcher))
            .unwrap();
        weight.scorer(searcher.segment_reader(0), 1.0).unwrap()
    }

    fn matches(index: &Index, q: &dyn Query) -> Vec<DocId> {
        let mut s = scorer(index, q);
        let mut out = Vec::new();
        while s.doc() != TERMINATED {
            out.push(s.doc());
            s.advance();
        }
        out
    }

    #[test]
    fn a_target_behind_the_phrase_is_a_lower_bound() {
        let (index, body) = index();
        let mut s = scorer(&index, &phrase(body, "a b"));
        assert_eq!(s.doc(), 2);
        // Unguarded, each of these asserted in a debug build.
        assert_eq!(s.seek_danger(0), SeekDangerResult::SeekLowerBound(2));
        assert_eq!(s.seek_danger(1), SeekDangerResult::SeekLowerBound(2));
        assert_eq!(s.seek_danger(2), SeekDangerResult::Found);
        assert_eq!(s.advance(), 4);
        assert_eq!(s.seek_danger(3), SeekDangerResult::SeekLowerBound(4));
        assert_eq!(s.seek_danger(4), SeekDangerResult::Found);
    }

    #[test]
    fn a_phrase_that_never_occurs_is_terminated_from_the_start() {
        let (index, body) = index();
        let mut s = scorer(&index, &phrase(body, "b x"));
        assert!(s.is::<GuardScorer>());
        assert_eq!(s.doc(), TERMINATED);
        assert_eq!(
            s.seek_danger(0),
            SeekDangerResult::SeekLowerBound(TERMINATED)
        );
        assert_eq!(
            s.seek_danger(3),
            SeekDangerResult::SeekLowerBound(TERMINATED)
        );
    }

    #[test]
    fn a_missing_term_stays_an_empty_scorer() {
        let (index, body) = index();
        let s = scorer(&index, &phrase(body, "a zz"));
        assert!(s.is::<EmptyScorer>());
    }

    /// Every increasing run of targets, on one scorer, agrees with the
    /// phrase's matches: `Found` exactly on a match, else a bound in
    /// `(target, next match]` or TERMINATED.
    #[test]
    fn seek_danger_agrees_with_the_matches() {
        let (index, body) = index();
        let n = DOCS.len() as DocId;
        for words in ["a b", "b x", "x b", "b a"] {
            let q = phrase(body, words);
            let hits = matches(&index, &q);
            for mask in 1u32..(1 << n) {
                let mut s = scorer(&index, &q);
                for t in (0..n).filter(|t| mask & (1 << t) != 0) {
                    let next = hits.iter().copied().find(|&h| h >= t).unwrap_or(TERMINATED);
                    match s.seek_danger(t) {
                        SeekDangerResult::Found => assert_eq!(next, t, "{words} {mask:b} {t}"),
                        SeekDangerResult::SeekLowerBound(b) => {
                            assert_ne!(next, t, "{words} {mask:b} {t}");
                            assert!(b > t && (b <= next || b == TERMINATED), "{words} {t} {b}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn an_excluded_phrase_removes_exactly_its_matches() {
        let (index, body) = index();
        for words in ["a b", "b x", "x b", "a zz"] {
            let excluded = matches(&index, &phrase(body, words));
            for word in ["a", "b", "x"] {
                let required =
                    TermQuery::new(Term::from_field_text(body, word), IndexRecordOption::Basic);
                let both = BooleanQuery::new(vec![
                    (Occur::Must, Box::new(required.clone()) as Box<dyn Query>),
                    (Occur::MustNot, Box::new(phrase(body, words))),
                ]);
                let expected: Vec<DocId> = matches(&index, &required)
                    .into_iter()
                    .filter(|d| !excluded.contains(d))
                    .collect();
                assert_eq!(matches(&index, &both), expected, "{word} -\"{words}\"");
            }
        }
    }
}
