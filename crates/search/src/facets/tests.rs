use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use lodestar_classify::Category;
use lodestar_store::flags;
use tantivy::indexer::NoMergePolicy;

use super::*;
use crate::cancel::CHECK_EVERY;
use crate::count::Total;
use crate::doc::{DocEnrichment, TorrentDoc};
use crate::index::{Error, SearchIndex};
use crate::query::{Node, ShareBy};
use crate::testing::tripwire;

const NOW: i64 = 1_700_000_100;
/// The first torrent's discovery time; one every three hours after.
const T0: i64 = 1_690_000_000;
const TYPES: [&str; 5] = ["movie", "tv", "music", "software", "adult"];
const RES: [&str; 3] = ["1080p", "2160p", "720p"];
const LANGS: [&str; 4] = ["en", "fr", "de", "ja"];
const EXTS: [&str; 6] = ["mkv", "mp4", "srt", "flac", "nfo", "exe"];

/// Torrent `i` of the fixture: every facet field varies with `i`, some
/// multi-valued, some missing.
fn doc(i: u64) -> TorrentDoc {
    let mut tags = vec![format!("type:{}", TYPES[(i % 5) as usize])];
    if i % 5 == 4 {
        tags.push(format!("adult:{}", ["scene", "vr"][(i % 2) as usize]));
    }
    // A Not-adult correction keeps the kind the classifier gave (§20.9).
    if i % 5 == 3 && i % 7 == 0 {
        tags.push("adult:scene".into());
    }
    if i % 4 != 3 {
        tags.push(format!("res:{}", RES[((i / 2) % 3) as usize]));
    }
    tags.push(format!("lang:{}", LANGS[(i % 4) as usize]));
    if i % 3 == 0 {
        tags.push(format!("lang:{}", LANGS[((i + 1) % 4) as usize]));
    }
    tags.push(format!("group:g{}", i % 13));
    if i % 5 == 0 {
        tags.push("has:nfo".into());
    }
    if i % 7 == 0 {
        tags.push("has:subs".into());
    }
    let mut exts = vec![EXTS[(i % 6) as usize].to_owned()];
    if i % 2 == 0 {
        exts.push(EXTS[((i + 3) % 6) as usize].to_owned());
    }
    let mut bits = 0;
    for (on, bit) in [
        (i % 4 == 0, flags::PRIVATE),
        (i % 6 == 0, flags::V2),
        (i % 9 == 0, flags::IMPORTED),
        (i % 10 == 0, flags::BACKFILL),
    ] {
        if on {
            bits |= bit;
        }
    }
    let years = match i % 10 {
        0 | 3 | 6 => Vec::new(),
        1 => vec![2000, 2001, 2002],
        _ => vec![1990 + i % 30],
    };
    let mut shares = Vec::new();
    if i % 2 == 0 {
        shares.push((Category::Video, 50, 80));
    }
    if i % 3 == 0 {
        shares.push((Category::Audio, 10, 5));
    }
    if i % 4 == 0 {
        shares.push((Category::Subtitle, 5, 1));
    }
    let name = format!(
        "Show{i} {}{}",
        if i % 2 == 0 { "Common" } else { "Odd" },
        if i % 3 == 0 { " Third" } else { "" }
    );
    TorrentDoc {
        id: i + 1,
        hashes: Vec::new(),
        name: name.clone(),
        path_entries: vec![format!("{name}.{}", exts[0])],
        exts,
        flags: bits,
        size: 1 << (10 + i % 32),
        files: 1 + i % 7,
        largest: 1 << (10 + i % 32),
        piece_len: 1 << 16,
        discovered_at: T0 + i as i64 * 3 * 3600,
        metadata_at: T0 + i as i64 * 3 * 3600 + 5,
        enriched: DocEnrichment {
            titles: Vec::new(),
            tags,
            years,
            seasons: Vec::new(),
            episodes: Vec::new(),
            junk: i % 100,
            ambiguous: i % 8 == 0,
            shares,
        },
    }
}

/// The values torrent `d` has for facet `f`.
fn values(f: Facet, d: &TorrentDoc) -> Vec<String> {
    // The adult facet counts within the Adult class only.
    if f == Facet::Adult && !d.enriched.tags.iter().any(|t| t == "type:adult") {
        return Vec::new();
    }
    match f.source() {
        Source::Tag(k) => {
            let prefix = format!("{}:", k.as_str());
            d.enriched
                .tags
                .iter()
                .filter_map(|t| t.strip_prefix(&prefix).map(str::to_owned))
                .collect()
        }
        Source::Ext => d.exts.clone(),
        Source::Flag(name) => {
            let on = if name == "ambiguous" {
                d.enriched.ambiguous
            } else {
                crate::doc::flag_names(d.flags).any(|n| n == name)
            };
            vec![yes_no(on).to_owned()]
        }
        Source::Category => d
            .enriched
            .shares
            .iter()
            .filter(|s| s.1 > 0)
            .map(|s| s.0.as_str().to_owned())
            .collect(),
    }
}

/// An index of `docs`, in `segments` segments that are never merged.
fn build(docs: &[TorrentDoc], segments: usize) -> Arc<SearchIndex> {
    let index = SearchIndex::in_memory().unwrap();
    add(&index, docs, segments);
    Arc::new(index)
}

fn add(index: &SearchIndex, docs: &[TorrentDoc], segments: usize) {
    let f = *index.fields();
    let (raw, reader) = index.raw();
    let per = docs.len().div_ceil(segments.max(1)).max(1);
    for chunk in docs.chunks(per) {
        // Heap enough for each chunk to stay one segment.
        let mut w: tantivy::IndexWriter = raw.writer_with_num_threads(1, 200 << 20).unwrap();
        w.set_merge_policy(Box::new(NoMergePolicy));
        for d in chunk {
            w.add_document(d.to_document(&f)).unwrap();
        }
        w.commit().unwrap();
    }
    reader.reload().unwrap();
}

fn corpus(n: u64) -> Vec<TorrentDoc> {
    (0..n).map(doc).collect()
}

fn lease(index: &Arc<SearchIndex>, node: &Node) -> Lease {
    Lease::new(
        index.clone(),
        node,
        NOW,
        crate::Sort::Relevance,
        &CancelToken::new(),
    )
    .unwrap()
}

fn run(index: &Arc<SearchIndex>, q: &str, params: &FacetParams) -> (Node, Lease, Facets) {
    let node = crate::parse(q).unwrap();
    let l = lease(index, &node);
    let f = facets(&l, &node, &[], params, &CancelToken::new()).unwrap();
    (node, l, f)
}

/// The ids `node` matches, from the index.
fn ids_of(index: &SearchIndex, node: &Node) -> Vec<u64> {
    index
        .matching_ids(&index.query(node, NOW), usize::MAX)
        .unwrap()
        .0
}

fn total(index: &SearchIndex, node: &Node) -> usize {
    index.search(&index.query(node, NOW), 1).unwrap().total
}

/// A query matching value `v` of facet `f`.
fn leaf(f: Facet, v: &str) -> Node {
    if f == Facet::Adult {
        return Node::And {
            of: vec![
                Node::Tag {
                    key: TagKey::Adult,
                    any: vec![v.to_owned()],
                },
                adult_class(),
            ],
        };
    }
    match f.source() {
        Source::Tag(key) => Node::Tag {
            key,
            any: vec![v.to_owned()],
        },
        Source::Ext => Node::Ext {
            any: vec![v.to_owned()],
        },
        Source::Flag(name) => Node::Flag {
            name: name.to_owned(),
            value: v == "yes",
        },
        Source::Category => Node::Share {
            by: ShareBy::Count,
            cat: v.to_owned(),
            lo: 1,
            hi: 100,
        },
    }
}

/// Every value's count for facet `f` over the torrents `ids`, by brute
/// force.
fn truth(docs: &[TorrentDoc], ids: &[u64], f: Facet) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for id in ids {
        for v in values(f, &docs[*id as usize - 1]) {
            *out.entry(v).or_default() += 1;
        }
    }
    out
}

fn all_values() -> FacetParams {
    FacetParams {
        limits: Facet::ALL.into_iter().map(|f| (f, MAX_LIMIT)).collect(),
        ..FacetParams::default()
    }
}

/// The analytics-validation checks (§10.17) for one query, every facet:
/// - each value's count equals a brute-force count over the torrents the
///   query matches without the facet's own clauses, and the index's own
///   count of that query narrowed to the value;
/// - single-valued facets (`type`, the flags) sum to that query's count,
///   and over the selected values to the result count;
/// - histogram bins plus `missing` sum to the result count, and match a
///   brute-force binning.
fn validate(docs: &[TorrentDoc], index: &Arc<SearchIndex>, q: &str) {
    let (node, l, got) = run(index, q, &all_values());
    let result = ids_of(index, &node);
    assert_eq!(l.total().value, result.len(), "{q}");
    assert_eq!(got.matches, result.len() as u64, "{q}");
    for (f, counts) in &got.terms {
        let variant = without_facet(&node, *f);
        let base = variant.clone().unwrap_or_else(|| node.clone());
        let ids = ids_of(index, &base);
        let want = truth(docs, &ids, *f);
        let listed: BTreeMap<String, u64> = counts
            .values
            .iter()
            .filter(|v| v.count > 0 || matches!(f.source(), Source::Flag(_)))
            .map(|v| (v.value.clone(), v.count))
            .collect();
        let want_nonzero: BTreeMap<String, u64> = want
            .iter()
            .filter(|(_, c)| **c > 0)
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        let listed_nonzero: BTreeMap<String, u64> = listed
            .iter()
            .filter(|(_, c)| **c > 0)
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        assert_eq!(listed_nonzero, want_nonzero, "{q}: {}", f.name());
        assert_eq!(counts.other, 0, "{q}: {} lists everything", f.name());
        assert!(!counts.approx);
        for v in &counts.values {
            let narrowed = Node::And {
                of: vec![base.clone(), leaf(*f, &v.value)],
            };
            assert_eq!(
                v.count as usize,
                total(index, &narrowed),
                "{q}: {}={}",
                f.name(),
                v.value
            );
        }
        if matches!(f.source(), Source::Flag(_)) || *f == Facet::Type {
            let sum: u64 = counts.values.iter().map(|v| v.count).sum();
            assert_eq!(sum as usize, ids.len(), "{q}: {} sums", f.name());
            // The result count is the selected values' disjunctive counts.
            let picked: Vec<&ValueCount> = counts.values.iter().filter(|v| v.selected).collect();
            let only_positive = f.selection(&node).iter().all(|(_, negated)| !negated);
            if !picked.is_empty() && only_positive {
                let sum: u64 = picked.iter().map(|v| v.count).sum();
                assert_eq!(sum as usize, result.len(), "{q}: {} selected", f.name());
            }
        }
    }
    for (h, hist) in &got.histograms {
        let sum: u64 = hist.bins.iter().map(|b| b.count).sum::<u64>() + hist.missing;
        assert_eq!(sum as usize, result.len(), "{q}: {} sums", h.name());
        for b in &hist.bins {
            let inside = |v: i64| v >= b.lo && b.hi.is_none_or(|hi| v < hi);
            let want = result
                .iter()
                .map(|id| &docs[*id as usize - 1])
                .filter(|d| match h {
                    Histo::Size => inside(d.size as i64),
                    Histo::Discovered => inside(d.discovered_at),
                    Histo::Year => d.enriched.years.first().is_some_and(|y| inside(*y as i64)),
                })
                .count();
            assert_eq!(b.count as usize, want, "{q}: {} bin {b:?}", h.name());
        }
    }
}

#[test]
fn counts_reconcile_with_brute_force_and_the_index() {
    let docs = corpus(600);
    let index = build(&docs, 3);
    for q in [
        "",
        "common",
        "common res:1080p,720p",
        "res:1080p | res:2160p",
        "-res:720p third",
        "-(res:720p | res:1080p) ext:mkv",
        "(res:2160p | type:movie) lang:fr",
        "type:movie,tv private:no",
        "common -private:yes v2:yes",
        "cat:video ext:mkv,flac -ext:exe",
        "cat:audio=0 common",
        "cat:video<60 cat:subtitle",
        "cat:video>=80% common",
        "lang:ja lang:en has:nfo",
        "group:g3 | group:g4",
        "ambiguous:yes imported:no backfill:no",
        "adult:scene",
        "size:>1GB discovered:<30d",
        "year:2000..2010 common",
    ] {
        validate(&docs, &index, q);
    }
}

#[test]
fn own_clauses_are_top_level_and_single_field() {
    let p = |q: &str| crate::parse(q).unwrap();
    let res = Facet::Res;
    // A leaf, an OR of the field, a negation, a parenthesised AND.
    for q in [
        "res:1080p",
        "res:1080p | res:720p",
        "-res:720p",
        "-(res:720p | res:1080p)",
        "!(res:720p res:1080p)",
    ] {
        let n = p(q);
        assert!(res.owns(&n), "{q}");
        assert_eq!(without_facet(&n, res), Some(Node::All), "{q}");
    }
    // Other fields stay, the hidden clause too.
    let n = p("dark -res:720p ext:mkv").without_hidden();
    assert_eq!(
        without_facet(&n, res),
        Some(p("dark ext:mkv").without_hidden())
    );
    assert_eq!(
        without_facet(&n, Facet::Ext),
        Some(p("dark -res:720p").without_hidden())
    );
    // Mixed fields, nested below the top level, or inside a file clause:
    // nobody's.
    for q in [
        "dark (res:1080p | type:movie)",
        "dark -(res:1080p ext:mkv)",
        "dark file:(ext:mkv)",
        "dark (ext:mkv (res:720p | type:tv))",
    ] {
        let n = p(q);
        for f in Facet::ALL {
            assert_eq!(without_facet(&n, f), None, "{q}: {}", f.name());
        }
    }
    // Flags and categories.
    assert!(Facet::Private.owns(&p("private:no")));
    assert!(Facet::Private.owns(&p("-private:yes")));
    assert!(!Facet::V2.owns(&p("private:no")));
    assert!(Facet::Category.owns(&p("cat:video | cat:audio")));
    assert!(!Facet::Category.owns(&p("bytes:video>50")));
    // Shares: "has some" (what the facet counts) is a selection, "has
    // none" an exclusion, any other range a plain filter.
    for (q, want) in [
        ("cat:video", Some(false)),
        ("cat:video>0", Some(false)),
        ("cat:video>=80%", None),
        ("cat:video=10..50", None),
        ("cat:video=0", Some(true)),
        ("-cat:video=0", Some(false)),
        ("cat:video<50", None),
        ("cat:video=0..100", None),
    ] {
        let n = p(q);
        let got = Facet::Category.selection(&n).first().map(|(v, neg)| {
            assert_eq!(v, "video");
            *neg
        });
        assert_eq!(got, want, "{q}");
        assert_eq!(Facet::Category.owns(&n), want.is_some(), "{q}");
    }
    assert!(Facet::Has.owns(&p("has:nfo")));
    assert!(Facet::Adult.owns(&p("adult:scene,vr")));
    // Selections, with negations.
    let n = p("dark (res:1080p | res:720p) -res:480p -(-res:2160p)");
    assert_eq!(
        res.selection(&n),
        [
            ("1080p".to_owned(), false),
            ("720p".to_owned(), false),
            ("480p".to_owned(), true),
            ("2160p".to_owned(), false),
        ]
    );
}

#[test]
fn always_on_filters_hold_in_every_pass() {
    // Adult hidden (§20.9): the visibility clause is a top-level
    // `-type:adult`, which would read as the `type` facet's own clause.
    let docs = corpus(500);
    let index = build(&docs, 2);
    let hide = vec![crate::parse("-type:adult").unwrap()];
    for q in [
        "",
        "type:movie,tv",
        "res:1080p private:no",
        "-type:music common",
    ] {
        let node = crate::parse(q).unwrap();
        let full = with_filters(node.clone(), &hide);
        let l = lease(&index, &full);
        let got = facets(&l, &node, &hide, &all_values(), &CancelToken::new()).unwrap();
        let types = got.term(Facet::Type).unwrap();
        assert!(
            types.values.iter().all(|v| v.value != "adult"),
            "{q}: {types:?}"
        );
        assert!(got.term(Facet::Adult).is_none(), "{q}");
        // Every pass counts within the filter: each facet's query, with
        // the filter, as the index counts it.
        for (f, t) in &got.terms {
            let base = with_filters(without_facet(&node, *f).unwrap_or(node.clone()), &hide);
            for v in &t.values {
                let narrowed = Node::And {
                    of: vec![base.clone(), leaf(*f, &v.value)],
                };
                assert_eq!(
                    v.count as usize,
                    total(&index, &narrowed),
                    "{q}: {}={}",
                    f.name(),
                    v.value
                );
            }
        }
        assert_eq!(got.matches as usize, l.total().value, "{q}");
    }
}

#[test]
fn adult_facet_follows_the_class() {
    let docs = corpus(500);
    let index = build(&docs, 2);
    let stale = Node::And {
        of: vec![
            crate::parse("adult:scene").unwrap(),
            Node::Not {
                of: Box::new(adult_class()),
            },
        ],
    };
    assert!(
        total(&index, &stale) > 0,
        "the fixture has corrected torrents"
    );
    // Shown: only Adult-class torrents count, selected or not.
    for q in ["", "common", "adult:scene", "type:adult,software"] {
        let (node, _, got) = run(&index, q, &all_values());
        let adult = got.term(Facet::Adult).unwrap();
        let base = without_facet(&node, Facet::Adult).unwrap_or(node.clone());
        for v in &adult.values {
            let narrowed = Node::And {
                of: vec![base.clone(), leaf(Facet::Adult, &v.value)],
            };
            assert_eq!(
                v.count as usize,
                total(&index, &narrowed),
                "{q}: {}",
                v.value
            );
        }
        assert!(adult.values.iter().any(|v| v.count > 0), "{q}");
    }
    // A class without Adult torrents lists nothing, stale kinds or not.
    let (_, _, got) = run(&index, "type:software", &all_values());
    assert!(got.term(Facet::Adult).is_none());
    // Hidden: never served, even when the query names a kind.
    let hide = vec![crate::parse("-type:adult").unwrap()];
    let params = FacetParams {
        hide_adult: true,
        ..all_values()
    };
    for q in ["", "type:movie", "adult:scene"] {
        let node = crate::parse(q).unwrap();
        let l = lease(&index, &with_filters(node.clone(), &hide));
        let got = facets(&l, &node, &hide, &params, &CancelToken::new()).unwrap();
        assert!(got.term(Facet::Adult).is_none(), "{q}");
        assert!(!got.unavailable.contains(&"adult".to_owned()), "{q}");
        let types = got.term(Facet::Type).unwrap();
        assert!(types.values.iter().all(|v| v.value != "adult"), "{q}");
    }
}

#[test]
fn selected_values_outside_the_top_are_listed() {
    let docs = corpus(260);
    let index = build(&docs, 1);
    let params = FacetParams {
        limits: [(Facet::Group, 2), (Facet::Ext, 1)].into_iter().collect(),
        ..FacetParams::default()
    };
    // g12 is the least common group; zz names none.
    let (_, _, got) = run(&index, "group:g12,zz -ext:exe", &params);
    let groups = got.term(Facet::Group).unwrap();
    let names: Vec<&str> = groups.values.iter().map(|v| v.value.as_str()).collect();
    let top: Vec<&str> = names[..2].to_vec();
    let outside = ["g12", "zz"].iter().filter(|g| !top.contains(g)).count();
    assert_eq!(names.len(), 2 + outside, "{names:?}");
    assert!(names.contains(&"g12") && names.contains(&"zz"));
    assert!(groups.values[0].count >= groups.values[1].count);
    let zz = groups.values.iter().find(|v| v.value == "zz").unwrap();
    assert_eq!((zz.count, zz.selected, zz.excluded), (0, true, false));
    assert!(
        groups
            .values
            .iter()
            .find(|v| v.value == "g12")
            .unwrap()
            .selected
    );
    assert_eq!(groups.distinct, 13);
    let listed: u64 = groups.values.iter().map(|v| v.count).sum();
    let without_groups = total(&index, &crate::parse("-ext:exe").unwrap());
    assert_eq!(
        (listed + groups.other) as usize,
        without_groups,
        "the group facet ignores its own clause"
    );
    // The top value of `ext`, and the excluded one.
    let ext = got.term(Facet::Ext).unwrap();
    let exe = ext.values.iter().find(|v| v.value == "exe").unwrap();
    assert!(exe.excluded && !exe.selected && exe.count > 0);
    assert_eq!(
        ext.values.len(),
        if ext.values[0].value == "exe" { 1 } else { 2 }
    );
    // Default limit: 8 of 13 groups.
    let (_, _, got) = run(&index, "", &FacetParams::default());
    assert_eq!(got.term(Facet::Group).unwrap().values.len(), DEFAULT_LIMIT);
}

#[test]
fn histograms_have_explicit_edges() {
    let docs = corpus(600);
    let index = build(&docs, 2);
    let first_run = T0 + 30 * 86_400;
    let params = FacetParams {
        first_run: Some(first_run),
        ..FacetParams::default()
    };
    let (_, _, got) = run(&index, "", &params);
    let size = got.histogram(Histo::Size).unwrap();
    assert_eq!(size.unit, Unit::Bytes);
    assert_eq!(size.bins.len(), SIZE_BINS);
    assert_eq!((size.bins[0].lo, size.bins[0].hi), (0, Some(1 << 20)));
    assert_eq!((size.bins[1].lo, size.bins[1].hi), (1 << 20, Some(1 << 21)));
    assert_eq!(
        (size.bins[20].lo, size.bins[20].hi),
        (1 << 39, Some(1 << 40))
    );
    assert_eq!((size.bins[21].lo, size.bins[21].hi), (1 << 40, None));
    for w in size.bins.windows(2) {
        assert_eq!(w[0].hi, Some(w[1].lo));
    }
    // 600 torrents three hours apart: 75 days, in day bins.
    let disc = got.histogram(Histo::Discovered).unwrap();
    assert_eq!(disc.unit, Unit::Day);
    assert_eq!(disc.bins[0].lo, T0.div_euclid(DAY) * DAY);
    for w in disc.bins.windows(2) {
        assert_eq!(w[0].hi, Some(w[1].lo));
        assert_eq!(w[0].hi.unwrap() - w[0].lo, DAY);
    }
    for b in &disc.bins {
        assert_eq!(b.backfill, b.hi.unwrap() <= first_run, "{b:?}");
    }
    assert!(disc.bins.iter().any(|b| b.backfill) && disc.bins.iter().any(|b| !b.backfill));
    let year = got.histogram(Histo::Year).unwrap();
    assert_eq!(year.unit, Unit::Year);
    // `1990 + i % 30` where a year is the only one: 1992 is the first.
    assert_eq!(year.bins.first().map(|b| b.lo), Some(1992));
    assert_eq!(year.bins.last().map(|b| b.hi), Some(Some(2020)));
    assert_eq!(year.missing, 180, "no year in three of ten");
    // No first run known: nothing marked.
    let (_, _, got) = run(&index, "", &FacetParams::default());
    let disc = got.histogram(Histo::Discovered).unwrap();
    assert!(disc.bins.iter().all(|b| !b.backfill));
    // Nothing matches: no bins to choose from, but the size edges.
    let (_, _, got) = run(&index, "nosuchword", &FacetParams::default());
    assert!(got.histogram(Histo::Discovered).unwrap().bins.is_empty());
    assert!(got.histogram(Histo::Year).unwrap().bins.is_empty());
    assert!(
        got.histogram(Histo::Size)
            .unwrap()
            .bins
            .iter()
            .all(|b| b.count == 0)
    );
    assert!(
        got.terms
            .iter()
            .all(|(f, t)| *f != Facet::Adult && t.values.iter().all(|v| v.count == 0))
    );
}

#[test]
fn discovered_bins_follow_the_range() {
    let at = |s: i64| {
        let mut m = HashMap::new();
        m.insert(hour(T0), 1.0);
        m.insert(hour(T0 + s), 2.0);
        m
    };
    let unit = |s: i64| discovered_bins(&at(s), None).0;
    assert_eq!(unit(10 * HOUR), Unit::Hour);
    assert_eq!(unit(3 * DAY), Unit::Day);
    assert_eq!(unit(200 * DAY), Unit::Week);
    assert_eq!(unit(3 * 365 * DAY), Unit::Month);
    // Weeks start on a Thursday at 00:00 UTC (1970-01-01 was one).
    let (_, bins) = discovered_bins(&at(200 * DAY), None);
    assert!(bins.iter().all(|b| b.lo.rem_euclid(WEEK) == 0));
    assert_eq!(bins.iter().map(|b| b.count).sum::<u64>(), 3);
    // Months are calendar months: 2023-07-22 is in July's bin.
    let (_, bins) = discovered_bins(&at(3 * 365 * DAY), None);
    assert_eq!(bins[0].lo, days_from_civil(2023, 7, 1) * DAY);
    assert_eq!(bins[0].hi, Some(days_from_civil(2023, 8, 1) * DAY));
    assert_eq!(bins.len(), 37);
    for d in [-800_000, -1, 0, 59, 365, 11_016, 19_000, 2_000_000] {
        let (y, m, day) = civil(d);
        assert_eq!(days_from_civil(y, m, day), d);
    }
    assert_eq!(civil(0), (1970, 1, 1));
    assert_eq!(civil(days_from_civil(2024, 2, 29)), (2024, 2, 29));
}

#[test]
fn stray_dates_go_in_edge_bins() {
    // 1,000 torrents over 60 days, one at 1970 (a source's created_at of
    // 0) and one in 9999.
    let mut hours: HashMap<i64, f64> = (0..1_000).map(|i| (hour(T0 + i * 5_184), 1.0)).collect();
    let far = days_from_civil(9999, 6, 1) * DAY;
    hours.insert(0, 1.0);
    hours.insert(hour(far), 1.0);
    let (unit, bins) = discovered_bins(&hours, Some(T0 + 10 * DAY));
    assert_eq!(unit, Unit::Day, "chosen from the bulk's range");
    let (first, last) = (&bins[0], &bins[bins.len() - 1]);
    assert_eq!((first.lo, first.count), (0, 1));
    assert!(first.backfill);
    assert_eq!((last.hi, last.count), (None, 1));
    assert_eq!(last.lo, bins[bins.len() - 2].hi.unwrap());
    assert!(bins.len() <= 64, "61 days and two edges: {}", bins.len());
    assert_eq!(bins.iter().map(|b| b.count).sum::<u64>(), 1_002);
    // Bins tile: each starts where the last ended.
    for w in bins.windows(2) {
        assert_eq!(w[0].hi, Some(w[1].lo));
    }
    // A bulk spread over centuries is capped, in years.
    let wide: HashMap<i64, f64> = (0..2_000)
        .map(|y| (hour(days_from_civil(1000 + y, 3, 1) * DAY), 1.0))
        .collect();
    let (unit, bins) = discovered_bins(&wide, None);
    assert_eq!(unit, Unit::Year);
    assert!(bins.len() <= MAX_DISCOVERED_BINS, "{}", bins.len());
    assert_eq!(bins.iter().map(|b| b.count).sum::<u64>(), 2_000);
    assert_eq!(bins[1].hi, Some(bins[2].lo));
    // And no stray date makes a segment's hour counts span decades.
    let h = Hours::for_column(None, T0);
    assert!(h.dense.is_empty());
}

#[test]
fn size_bins_are_powers_of_two() {
    assert_eq!(size_bin(0), 0);
    assert_eq!(size_bin((1 << 20) - 1), 0);
    assert_eq!(size_bin(1 << 20), 1);
    assert_eq!(size_bin((1 << 21) - 1), 1);
    assert_eq!(size_bin(1 << 21), 2);
    assert_eq!(size_bin((1 << 40) - 1), 20);
    assert_eq!(size_bin(1 << 40), 21);
    assert_eq!(size_bin(u64::MAX), 21);
}

#[test]
fn large_counts_are_sampled_within_tolerance() {
    // 40,000 torrents in one segment, sampled with a 2,048-document
    // segment threshold, and "large" above 1,000 matches.
    let docs = corpus(40_000);
    let index = build(&docs, 1);
    let sampled = FacetParams {
        policy: Policy {
            sample_above: 1_000,
            segment_sample_above: 2_048,
        },
        ..all_values()
    };
    let exact = all_values();
    for q in ["", "common res:1080p", "third", "lang:fr | lang:de"] {
        let (_, _, est) = run(&index, q, &sampled);
        let (_, _, want) = run(&index, q, &exact);
        assert!(want.sample_rate.is_none(), "{q}: exact below the threshold");
        let rate = est.sample_rate.expect("sampled");
        assert!((rate - 1.0 / 16.0).abs() < 0.01, "{q}: rate {rate}");
        // Six standard deviations of a count scaled by 16 (module docs),
        // plus the edges of a stratum or two.
        let close = |got: u64, want: u64| {
            let bound = 6.0 * (16.0 * want as f64).sqrt() + 32.0;
            (got as f64 - want as f64).abs() <= bound
        };
        assert!(
            close(est.matches, want.matches),
            "{q}: {} vs {}",
            est.matches,
            want.matches
        );
        for (f, t) in &est.terms {
            assert!(t.approx, "{q}: {}", f.name());
            let w = want.term(*f).unwrap();
            for v in &w.values {
                let got = t
                    .values
                    .iter()
                    .find(|x| x.value == v.value)
                    .map_or(0, |x| x.count);
                assert!(
                    close(got, v.count),
                    "{q}: {}={} {got} vs {}",
                    f.name(),
                    v.value,
                    v.count
                );
            }
        }
        for (h, hist) in &est.histograms {
            assert!(hist.approx);
            let w = want.histogram(*h).unwrap();
            let sum = |h: &Histogram| h.bins.iter().map(|b| b.count).sum::<u64>() + h.missing;
            assert!(close(sum(hist), sum(w)), "{q}: {}", h.name());
        }
    }
    // A small result is exact even with sampling on: its own pass and the
    // shared one are under the threshold.
    let (_, _, small) = run(&index, "size:<2KB junk:<5", &sampled);
    assert!(small.sample_rate.is_none());
    assert!(small.terms.iter().all(|(_, t)| !t.approx));
}

#[test]
fn a_cancelled_facet_pass_stops_early() {
    // Cancelled on the 1,000th of 50,000 documents, it stops within one
    // check interval per segment.
    let docs = corpus(50_000);
    let index = build(&docs, 2);
    let cancel = CancelToken::new();
    let (compiled, visits) = tripwire(1_000, &cancel);
    let l = Lease::raw(
        index.clone(),
        compiled,
        Total {
            value: 50_000,
            approx: false,
        },
    );
    let r = facets(&l, &Node::All, &[], &FacetParams::default(), &cancel);
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    let went = visits.load(Ordering::Relaxed);
    // Segments run in parallel, each checking on its own.
    let segments = l.searcher().segment_readers().len();
    let bound = 1_000 + segments * CHECK_EVERY as usize;
    assert!(bound < 50_000);
    assert!(went <= bound, "visited {went} of 50,000, bound {bound}");
    // Cancelled before it starts, it does nothing.
    let done = CancelToken::new();
    done.cancel();
    let l = lease(&index, &Node::All);
    assert!(matches!(
        facets(&l, &Node::All, &[], &FacetParams::default(), &done),
        Err(Error::Cancelled)
    ));
    // Out of its budget, it says so.
    let slow = crate::testing::slow(&index, std::time::Duration::from_micros(20));
    let l = Lease::raw(
        index.clone(),
        slow,
        Total {
            value: 50_000,
            approx: false,
        },
    );
    let budget = CancelToken::new().with_budget(std::time::Duration::from_millis(20));
    assert!(matches!(
        facets(&l, &Node::All, &[], &FacetParams::default(), &budget),
        Err(Error::OutOfTime)
    ));
}

#[test]
fn a_lease_keeps_its_snapshot_across_a_commit() {
    let docs = corpus(300);
    let index = build(&docs[..200], 1);
    let node = crate::parse("common res:1080p").unwrap();
    let l = lease(&index, &node);
    add(&index, &docs[200..], 1);
    let old = facets(&l, &node, &[], &all_values(), &CancelToken::new()).unwrap();
    let new_lease = lease(&index, &node);
    let new = facets(&new_lease, &node, &[], &all_values(), &CancelToken::new()).unwrap();
    let in_first = |n: u64| (0..n).filter(|i| i % 2 == 0 && i % 4 != 3).count();
    // `res` drops its own clause: every `common` torrent with a res.
    let res_total = |f: &Facets| -> u64 {
        f.term(Facet::Res)
            .unwrap()
            .values
            .iter()
            .map(|v| v.count)
            .sum()
    };
    assert_eq!(res_total(&old) as usize, in_first(200));
    assert_eq!(res_total(&new) as usize, in_first(300));
    assert_eq!(old.matches as usize, l.total().value);
    assert!(new.matches > old.matches);
}

#[test]
fn unavailable_groups_are_named_not_zeroed() {
    let docs = corpus(50);
    let index = build(&docs, 1);
    let (_, _, got) = run(&index, "", &FacetParams::default());
    // Swarm buckets are behind a flag until M4's fields are indexed.
    const { assert!(!SWARM_BUCKETS) };
    assert_eq!(got.unavailable, ["seeders", "trackers"]);
    // Every other facet is served, in decision order; adult only when
    // there's something to list.
    let names: Vec<&str> = got.terms.iter().map(|(f, _)| f.name()).collect();
    assert_eq!(
        names,
        [
            "type",
            "ext",
            "res",
            "category",
            "lang",
            "group",
            "codec",
            "audio",
            "hdr",
            "private",
            "v2",
            "has",
            "ambiguous",
            "imported",
            "backfill",
            "adult"
        ]
    );
    let (_, _, got) = run(&index, "-type:adult", &FacetParams::default());
    assert!(got.term(Facet::Adult).is_none());
    // Fields no torrent has: an empty list, not zeros.
    assert!(got.term(Facet::Codec).unwrap().values.is_empty());
    assert_eq!(
        Facet::ALL.map(|f| Facet::from_name(f.name())),
        Facet::ALL.map(Some)
    );
    assert_eq!(Facet::from_name("seeders"), None);
}

mod rebuild {
    //! The schema bump: an index of the previous version serves (without
    //! the year histogram) while this build's is built, then facets come
    //! from the new one.

    use std::path::Path;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use lodestar_store::{Store, StoreConfig};
    use tantivy::TantivyDocument;

    use super::*;
    use crate::rebuild::{IndexVersion, Serving, ServingConfig};
    use crate::schema::{SCHEMA_VERSION, TOKENIZER_VERSION};
    use crate::{LiveConfig, compile};

    const HEAP: usize = 15 << 20;

    fn add_torrent(store: &mut Store, name: &str) {
        use lodestar_bencode::{OwnedValue, to_vec};
        let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
        let raw = to_vec(&OwnedValue::Dict(
            [
                ("length", OwnedValue::Int(2_000_000_000)),
                ("name", b(name)),
                ("piece length", OwnedValue::Int(1 << 16)),
                ("pieces", OwnedValue::Bytes(vec![0; 20])),
            ]
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
        ));
        let info = lodestar_metainfo::Info::parse(&raw).unwrap();
        store
            .insert(&[lodestar_store::NewTorrent {
                info: &info,
                raw_info: Some(&raw),
                discovered_at: 1_700_000_000,
                metadata_at: 1_700_000_001,
                extra_flags: 0,
                source: lodestar_store::SourceId::new(0).unwrap(),
            }])
            .unwrap();
    }

    /// The previous version's index in `dir`: this schema without
    /// `release_year`, filled from `store`.
    fn create_previous(dir: &Path, store: &mut Store) {
        std::fs::create_dir_all(dir.join("files")).unwrap();
        let (current, _) = crate::schema::build();
        let mut b = tantivy::schema::Schema::builder();
        for (_, entry) in current.fields() {
            if entry.name() != "release_year" {
                b.add_field(entry.clone());
            }
        }
        tantivy::Index::create_in_dir(dir, b.build()).unwrap();
        tantivy::Index::builder()
            .schema(crate::files::schema().0)
            .settings(crate::files::settings())
            .create_in_dir(dir.join("files"))
            .unwrap();
        let (probe, _) = SearchIndex::open_existing(dir).unwrap().unwrap();
        assert_eq!(probe.missing_fields(), ["release_year"]);
        let (fields, file_fields) = (probe.fields(), probe.file_fields());
        let open = |d: &Path| {
            let index = tantivy::Index::open_in_dir(d).unwrap();
            index.tokenizers().register(
                crate::tokenizer::NAME_TOKENIZER,
                crate::tokenizer::NameTokenizer::default(),
            );
            index.writer::<TantivyDocument>(HEAP).unwrap()
        };
        let (mut w, mut fw) = (open(dir), open(&dir.join("files")));
        for id in store.ids_after(0, 10_000).unwrap() {
            let row = store.torrent(id).unwrap().unwrap();
            let tree = store.files(id).unwrap().unwrap();
            let mut doc = TorrentDoc::new(&row, &tree);
            let e = store.enrichment(id).unwrap().unwrap();
            let record = lodestar_classify::Record::decode(&e.data).unwrap();
            doc.enriched = DocEnrichment::new(&record, None);
            let mut d = TantivyDocument::new();
            for (f, v) in doc.to_document(fields).field_values() {
                if f != compile::ABSENT {
                    d.add_field_value(f, v);
                }
            }
            w.add_document(d).unwrap();
            for fd in crate::files::file_docs(id as u64, &tree) {
                fw.add_document(fd.to_document(file_fields)).unwrap();
            }
        }
        w.commit().unwrap();
        fw.commit().unwrap();
    }

    #[test]
    fn the_previous_version_is_rebuilt_and_serves_facets_after_the_swap() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("index");
        let db = tmp.path().join("s.db");
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        for i in 0..30 {
            add_torrent(&mut store, &format!("Film.{i}.{}.1080p.mkv", 2000 + i % 3));
        }
        lodestar_pipeline::enrich::enrich_stored(&mut store, 0, 100)
            .unwrap()
            .unwrap();
        let previous = IndexVersion {
            schema: SCHEMA_VERSION - 1,
            tokenizer: TOKENIZER_VERSION,
        };
        create_previous(&root.join(previous.dir_name()), &mut store);

        let hold = Arc::new(AtomicBool::new(true));
        let config = ServingConfig {
            live: LiveConfig {
                heap_bytes: HEAP,
                poll: Duration::from_millis(20),
                commit_every: Duration::from_millis(50),
                batch: 10,
                ..LiveConfig::default()
            },
            rebuild_commit_docs: 10,
            hold_before_finish: Some(hold.clone()),
            ..ServingConfig::default()
        };
        let serving = Serving::open_or_rebuild(&root, &db, config).unwrap();
        assert_eq!(serving.progress().unwrap().from, previous);
        let facets_now = || {
            let index = serving.index();
            let node = crate::parse("film").unwrap();
            let l = Lease::new(
                index,
                &node,
                NOW,
                crate::Sort::Relevance,
                &CancelToken::new(),
            )
            .unwrap();
            facets(&l, &node, &[], &FacetParams::default(), &CancelToken::new()).unwrap()
        };
        // The old index serves every facet it can; the year histogram
        // needs the new field.
        let before = facets_now();
        assert_eq!(before.matches, 30);
        assert_eq!(
            before.unavailable,
            ["category", "year", "seeders", "trackers"]
        );
        assert!(before.term(Facet::Category).is_none());
        assert!(before.histogram(Histo::Year).is_none());
        let res = before.term(Facet::Res).unwrap();
        assert_eq!(res.values[0].value, "1080p");
        assert_eq!(res.values[0].count, 30);

        hold.store(false, Ordering::Relaxed);
        let start = std::time::Instant::now();
        while serving.rebuilding() {
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "the rebuild finishes"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            serving.progress().and_then(|p| p.error),
            None,
            "the swap succeeded"
        );
        let after = facets_now();
        assert_eq!(after.unavailable, ["seeders", "trackers"]);
        let year = after.histogram(Histo::Year).unwrap();
        let counts: Vec<(i64, u64)> = year.bins.iter().map(|b| (b.lo, b.count)).collect();
        assert_eq!(counts, [(2000, 10), (2001, 10), (2002, 10)]);
        assert_eq!(after.term(Facet::Res).unwrap().values[0].count, 30);
        let video = after.term(Facet::Category).unwrap();
        assert_eq!(
            (video.values[0].value.as_str(), video.values[0].count),
            ("video", 30)
        );
        serving.shutdown().unwrap();
    }
}
