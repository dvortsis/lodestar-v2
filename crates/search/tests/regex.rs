//! Regex search end to end (architecture §8, RX-1): `/…/` and
//! `name:/…/` parse, run the trigram prefilter (`marker OR plan`) and
//! verify the stored name; what they find is exactly what the pattern
//! matches, whether or not the prefilter ran, and a stop is reported.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_search::{CancelToken, Node, SearchIndex, parse, regex_verify};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig};
use proptest::prelude::*;

const NOW: i64 = 1_767_225_600;

fn info(name: &str, i: usize) -> Vec<u8> {
    let d = |e: Vec<(&str, OwnedValue)>| {
        OwnedValue::Dict(
            e.into_iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v))
                .collect(),
        )
    };
    to_vec(&d(vec![
        ("length", OwnedValue::Int(1 << 20)),
        ("name", OwnedValue::Bytes(name.as_bytes().to_vec())),
        ("piece length", OwnedValue::Int(1 << 18)),
        // Distinct pieces, so equal names are distinct torrents.
        (
            "pieces",
            OwnedValue::Bytes((i as u32).to_le_bytes().repeat(5)),
        ),
    ]))
}

fn index_of(names: &[String]) -> SearchIndex {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let raws: Vec<Vec<u8>> = names.iter().enumerate().map(|(i, n)| info(n, i)).collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .map(|(info, raw)| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: NOW,
            metadata_at: NOW,
            extra_flags: 0,
            source: SourceId::new(0).unwrap(),
        })
        .collect();
    store.insert(&batch).unwrap();
    let index = SearchIndex::in_memory().unwrap();
    let mut w = index.writer(15 << 20).unwrap();
    w.catch_up(&mut store, 1000).unwrap();
    w.commit().unwrap();
    index
}

fn found(index: &SearchIndex, ast: &Node) -> Vec<String> {
    let hits = index.search(&index.query(ast, NOW), 1000).unwrap().hits;
    let mut names = index.names(&hits).unwrap();
    names.sort();
    names
}

fn find(index: &SearchIndex, q: &str) -> Vec<String> {
    found(index, &parse(q).unwrap_or_else(|e| panic!("{q}: {e}")))
}

/// What `pattern` matches among `names`, by brute force.
fn expected(names: &[String], pattern: &str) -> Vec<String> {
    let re = regex::Regex::new(&format!("(?i){pattern}")).unwrap();
    let mut out: Vec<String> = names.iter().filter(|n| re.is_match(n)).cloned().collect();
    out.sort();
    out
}

fn long_name() -> String {
    // Past the trigram prefix: its grams aren't indexed, the marker is.
    format!("{} Tail.Zqxv.Part", "Filler.Words.".repeat(10))
}

fn corpus() -> (SearchIndex, Vec<String>) {
    let names: Vec<String> = [
        "Show.Name.S01E02.1080p.WEB",
        "show name s03e10 720p",
        "Other.Show.S10E01",
        "Movie (2019) [1080p]",
        "ΣΟΦΙΑ documentary",
        "AC/DC Live",
        "ab",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .chain([long_name()])
    .collect();
    (index_of(&names), names)
}

#[test]
fn regex_literals_parse_and_render_back() {
    let n = parse(r"/S0[1-9]E\d+/").unwrap();
    assert_eq!(
        n,
        Node::Regex {
            pattern: r"S0[1-9]E\d+".into()
        }
    );
    assert_eq!(
        parse(r"name:/a b|c/").unwrap(),
        Node::Regex {
            pattern: "a b|c".into()
        }
    );
    // `\/` is a slash in the pattern; other escapes are the regex's.
    assert_eq!(
        parse(r"/AC\/DC\./").unwrap(),
        Node::Regex {
            pattern: r"AC/DC\.".into()
        }
    );
    for q in [r"/AC\/DC/", r"/a\\/", r"x -/y z/ (w | /v+/)", r"/\\\/x/"] {
        let n = parse(q).unwrap();
        assert_eq!(parse(&n.to_query()).unwrap(), n, "{q}");
    }
}

#[test]
fn bad_regexes_are_refused_with_their_code_and_place() {
    let e = parse("x /abc").unwrap_err();
    assert_eq!((e.start, e.end), (2, 6));
    assert!(e.message.contains("closing"), "{e}");
    assert!(parse("//").unwrap_err().message.contains("empty"));
    let e = parse("x /a(b/").unwrap_err();
    assert!(e.message.contains("regex_syntax"), "{e}");
    assert_eq!((e.start, e.end), (2, 7));
    let e = parse(&format!("/{}/", "a".repeat(513))).unwrap_err();
    assert!(e.message.contains("regex_too_long"), "{e}");
    let e = parse(r"/(?s)a/").unwrap_err();
    assert!(e.message.contains("regex_flag_not_allowed"), "{e}");
    // `file:(…)` doesn't take one.
    assert!(parse("file:(/a/)").is_err());
}

#[test]
fn regex_search_finds_what_the_pattern_matches() {
    let (ix, names) = corpus();
    for p in [
        r"S0[1-9]E\d+",
        r"s\d\de\d\d",
        r"(?-i)S\d",
        r"\(\d{4}\)",
        "σοφ",
        "AC/DC",
        "zqxv",
        r"words\.filler",
        ".",
        "ab",
        "[^\\s\\S]",
        "1080p|720p",
    ] {
        let q = format!("/{}/", p.replace('/', "\\/"));
        assert_eq!(find(&ix, &q), expected(&names, p), "{q}");
    }
    // The long name matches past the prefix through the marker.
    assert_eq!(find(&ix, "/zqxv/"), [long_name()]);
    // With other clauses, and negated.
    assert_eq!(find(&ix, r"show /s\d\de1\d/"), ["show name s03e10 720p"]);
    let mut not_web = expected(&names, ".");
    not_web.retain(|n| !n.contains("WEB"));
    assert_eq!(find(&ix, "-/web/"), not_web);
}

#[test]
fn a_stopped_verify_is_reported_and_answers_consistently() {
    let (ix, _) = corpus();
    let ast = parse("/show/").unwrap();
    let full = ix.query(&ast, NOW);
    assert_eq!(ix.search(&full, 50).unwrap().total, 3);
    assert!(!regex_verify::stopped(&full));
    // The token stops before the first candidate: nothing verified, and
    // the result says it's partial, on every pass.
    let token = CancelToken::new();
    let cut = ix.query_cancelled(&ast, NOW, 0, &token).unwrap();
    token.cancel();
    for _ in 0..2 {
        let r = ix.search(&cut, 50).unwrap();
        assert_eq!((r.hits.len(), r.total), (0, 0));
        assert!(regex_verify::stopped(&cut));
    }
}

fn arb_name() -> impl Strategy<Value = String> {
    // A small alphabet, so patterns match often; some names run past the
    // trigram prefix.
    prop::collection::vec(
        prop::sample::select(vec!['a', 'B', 'c', '.', ' ', '1', 'ſ', 'K']),
        0..130,
    )
    .prop_map(|cs| cs.into_iter().collect())
}

fn arb_pattern() -> impl Strategy<Value = String> {
    let leaf = prop::sample::select(vec![
        "a", "b", "c", "ab", "bc", "abc", "cab", "\\.", "1", "k", "s", "[ab]", "\\d", ".", "a+",
        "(?-i:B)", "[^a]",
    ]);
    prop::collection::vec(leaf, 1..6).prop_flat_map(|parts| {
        let joined = parts.concat();
        prop::sample::select(vec![
            joined.clone(),
            format!("{joined}|cc"),
            format!("({joined}){{2}}"),
        ])
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// The prefilter never drops a match: what a search finds is exactly
    /// what the pattern matches by brute force.
    #[test]
    fn search_equals_brute_force(
        names in prop::collection::vec(arb_name(), 1..40),
        patterns in prop::collection::vec(arb_pattern(), 1..6),
    ) {
        let ix = index_of(&names);
        // The names as the store keeps them.
        let stored = found(&ix, &Node::All);
        prop_assert_eq!(stored.len(), names.len());
        for p in &patterns {
            let ast = Node::Regex { pattern: p.clone() };
            prop_assert_eq!(found(&ix, &ast), expected(&stored, p), "{}", p);
        }
    }
}
