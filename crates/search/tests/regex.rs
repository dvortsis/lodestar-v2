//! Regex search end to end (architecture §8, RX-1): `/…/` and
//! `name:/…/` parse, run the trigram prefilter (`marker OR plan`) and
//! verify the stored name; what they find is exactly what the pattern
//! matches, whether or not the prefilter ran, and a stop is reported.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lodestar_search::{
    CancelToken, Lease, LeaseConfig, Leases, MAX_REGEX_CLAUSES, Node, REGEX_TOO_MANY, SearchIndex,
    Sort, Start, parse, regex_verify,
};
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

/// A token whose search's regex budget is already spent.
fn spent() -> CancelToken {
    let t = CancelToken::new();
    t.regex_deadline(Duration::ZERO);
    t
}

#[test]
fn a_stopped_verify_is_reported_and_answers_consistently() {
    let (ix, _) = corpus();
    let ast = parse("/show/").unwrap();
    let full = ix.query(&ast, NOW);
    assert_eq!(ix.search(&full, 50).unwrap().total, 3);
    assert!(!regex_verify::stopped(&full));
    // The budget runs out before the first candidate: nothing verified,
    // and the result says it's partial, on every pass.
    let cut = ix.query_cancelled(&ast, NOW, 0, &spent()).unwrap();
    for _ in 0..2 {
        let r = ix.search(&cut, 50).unwrap();
        assert_eq!((r.hits.len(), r.total), (0, 0));
        assert!(regex_verify::stopped(&cut));
    }
}

/// S8-10 item 6: a pass verifies under its own token, never under the
/// token of the request that compiled the query, which may be gone.
#[test]
fn a_later_pass_verifies_under_its_own_token_not_the_compiles() {
    let (ix, _) = corpus();
    let compiling = CancelToken::new();
    let q = ix
        .query_cancelled(&parse("/show/").unwrap(), NOW, 0, &compiling)
        .unwrap();
    compiling.cancel();
    let r = ix.search(&q, 50).unwrap();
    assert_eq!(r.total, 3);
    assert!(!regex_verify::stopped(&q));
}

#[test]
fn a_query_holds_at_most_four_regexes() {
    let four = "/a1/ /b2/ (/c3/ | x) -/d4/";
    assert_eq!(parse(four).unwrap().regex_clauses(), MAX_REGEX_CLAUSES);
    let five = format!("{four} name:/e5/");
    let e = parse(&five).unwrap_err();
    assert_eq!(e.reason, Some(REGEX_TOO_MANY));
    assert_eq!(e.message, "Use at most 4 /regex/ terms in one search.");
    // At the fifth.
    assert_eq!(&five[e.start..e.end], "name:/e5/");
    // Refused patterns carry their code as the reason too.
    assert_eq!(parse("/a(b/").unwrap_err().reason, Some("regex_syntax"));
    assert_eq!(parse("//").unwrap_err().reason, None);
}

#[test]
fn a_stored_query_over_the_cap_matches_nothing_and_verifies_nothing() {
    let (ix, _) = corpus();
    let every = |n: usize| Node::And {
        of: (0..n)
            .map(|_| Node::Regex {
                pattern: ".".into(),
            })
            .collect(),
    };
    assert_eq!(found(&ix, &every(MAX_REGEX_CLAUSES)).len(), 8);
    let over = ix.query(&every(MAX_REGEX_CLAUSES + 1), NOW);
    assert_eq!(ix.search(&over, 50).unwrap().total, 0);
    assert!(!regex_verify::stopped(&over));
}

#[test]
fn slashes_delimit_a_regex_only_where_a_pattern_can_start_and_end() {
    let words = |q: &str| !parse(q).unwrap().has_regex();
    // An opening `/` before a space, or a closing one inside a word, is
    // plain text.
    assert!(words("tv / movies / 1080p"));
    assert!(words("/usr/bin"));
    assert!(words("/a/b"));
    assert!(words("x / y/"));
    // Before whitespace, `)` or the end, it closes one.
    for q in [
        "/a/",
        "/a/ b",
        "(/a/)",
        "x (y | /a b/)",
        "name:/a/",
        "/a/(b)",
        "/a/|b",
    ] {
        assert!(parse(q).unwrap().has_regex(), "{q}");
    }
    // Fuzz find: `/or/` before `(` was a word that rendered as a regex.
    let n = parse("/or/(krk:ipfs) x").unwrap();
    assert_eq!(parse(&n.to_query()).unwrap(), n);
    assert!(n.has_regex());
    // A lone `/` is a word, at the end too.
    assert!(words("x /"));
    assert!(words("/ "));
    assert!(words("(x /)"));
    // A `/` before `)` never opens one, even with a `/` further on.
    assert!(words("(x /) y/"));
    let n = parse("(x /) y/").unwrap();
    assert_eq!(parse(&n.to_query()).unwrap(), n);
    // Unclosed, it's plain words too, and the word renders back as itself.
    for q in ["/abc", "x /abc", "/usr", "movies /2023", "/a/b/c"] {
        let n = parse(q).unwrap();
        assert!(!n.has_regex(), "{q}");
        assert_eq!(parse(&n.to_query()).unwrap(), n, "{q}");
    }
    // `name:` is lowercase, like every field: another case is an error,
    // not a silent word search.
    for q in ["Name:/x/", "NAME:/x/ y"] {
        let e = parse(q).unwrap_err();
        assert!(e.message.contains("lowercase"), "{q}: {e}");
    }
    assert!(parse("Name:x").is_ok(), "a plain word, as `Title:x` is");
    // Still an error when it delimits one but is empty.
    assert!(parse("// x").unwrap_err().message.contains("empty"));
    // `file:(…)` is looked inside, though the parser refuses one there.
    let stored = Node::File {
        of: Box::new(Node::Regex {
            pattern: "a".into(),
        }),
    };
    assert!(stored.has_regex());
}

/// S8-11: the clauses of one search share one deadline. Spent before
/// the search, all four at the cap stop at once, not after a budget
/// each; facets and recovery under the same token share it too
/// (`cache.rs`, `recovery.rs`).
#[test]
fn every_clause_of_a_search_shares_one_regex_budget() {
    let (ix, _) = corpus();
    let ast = parse("/s/ /o/ /w/ /h/").unwrap();
    assert_eq!(found(&ix, &ast).len(), 3);
    let started = Instant::now();
    let q = ix.query_cancelled(&ast, NOW, 0, &spent()).unwrap();
    assert_eq!(ix.search(&q, 50).unwrap().total, 0);
    assert!(regex_verify::stopped(&q));
    assert!(started.elapsed() < regex_verify::VERIFY_BUDGET);
    // A child with a budget of its own shares its parent's regex budget;
    // a regex scope has its own.
    let token = spent();
    let child = token.with_budget(Duration::from_secs(60));
    let q = ix.query_cancelled(&ast, NOW, 0, &child).unwrap();
    assert_eq!(ix.search(&q, 50).unwrap().total, 0);
    let scoped = token.regex_scope();
    let q = ix.query_cancelled(&ast, NOW, 0, &scoped).unwrap();
    assert_eq!(ix.search(&q, 50).unwrap().total, 3);
}

/// S8-10: a lease keeps only the verified matches, charged at their
/// size, and its pages agree with its count.
#[test]
fn a_lease_keeps_only_its_verified_matches_and_charges_them() {
    let names: Vec<String> = (0..200).map(|i| format!("Show {i} s01e{i:02}")).collect();
    let ix = Arc::new(index_of(&names));
    let segments = ix.segments();
    let lease = |q: &str, token: &CancelToken| {
        Lease::new(ix.clone(), &parse(q).unwrap(), NOW, Sort::Relevance, token).unwrap()
    };
    // At the cap, every clause matching every name: the worst case is a
    // bitset per clause per segment, plus a fixed overhead.
    let all = lease(r"/show/ /s01/ /e/ /\d/", &CancelToken::new());
    assert_eq!(all.total().value, 200);
    let bound = MAX_REGEX_CLAUSES * (1024 + segments * (64 + 200 / 8 + 8));
    assert!(
        all.regex_bytes() > 0 && all.regex_bytes() <= bound,
        "{}",
        all.regex_bytes()
    );
    // Pages read the kept matches (the verifiers are gone), and agree
    // with the count.
    let mut seen = 0;
    let mut start = Start::Offset(0);
    loop {
        let page = all.page(start, 50, &CancelToken::new()).unwrap();
        seen += page.hits.len();
        match page.next {
            Some(p) => start = Start::After(p),
            None => break,
        }
    }
    assert_eq!(seen, 200);
    // A sparse match keeps a short id list.
    let one = lease("/show 7 /", &CancelToken::new());
    assert_eq!(one.total().value, 1);
    assert!(one.regex_bytes() < all.regex_bytes() / MAX_REGEX_CLAUSES);
    // Stopped: no matches kept, and every page says so.
    let cut = lease("/show/", &spent());
    assert!(cut.regex_stopped());
    assert_eq!(cut.total().value, 0);
    assert!(
        cut.page(Start::Offset(0), 50, &CancelToken::new())
            .unwrap()
            .hits
            .is_empty()
    );
    // Without a regex: nothing charged.
    assert_eq!(lease("show", &CancelToken::new()).regex_bytes(), 0);
    // Leases charge it. Each client may hold a quarter of
    // `max_regex_bytes`, here two leases' worth: a third pushes out its
    // own least recently used, never another client's or a plain lease.
    let at_cap = || Arc::new(lease(r"/show/ /s01/ /e/ /\d/", &CancelToken::new()));
    let bytes = all.regex_bytes();
    let leases = Leases::new(LeaseConfig {
        max_regex_bytes: 4 * 2 * bytes + 3,
        ..LeaseConfig::default()
    });
    let plain = leases.insert(Arc::new(lease("show", &CancelToken::new())), 1);
    let other = leases.insert(at_cap(), 2);
    let a = leases.insert(at_cap(), 1);
    let b = leases.insert(at_cap(), 1);
    // `a` is used again, so `b` is now the least recently used.
    leases.get(&a, 1).unwrap();
    let c = leases.insert(at_cap(), 1);
    assert!(
        leases.get(&b, 1).is_err(),
        "its own least recently used went"
    );
    for (id, client) in [(a, 1), (c, 1), (other, 2), (plain, 1)] {
        assert!(leases.get(&id, client).is_ok());
    }
    // Client 2, under its share, adds one without touching client 1's.
    let other2 = leases.insert(at_cap(), 2);
    for (id, client) in [(a, 1), (c, 1), (other, 2), (other2, 2)] {
        assert!(leases.get(&id, client).is_ok());
    }

    // Past the total with five clients each within its share: the least
    // recently used lease of the client that will hold the most goes. A
    // share of one lease: clients 1 to 4 hold one each, client 1 is used
    // again, and client 5's makes five against room for four. All hold
    // one (client 5 its incoming one, but it has none to give), so the
    // oldest of the others' goes: client 2's.
    let total = |leases: &Leases, ids: &[(lodestar_search::LeaseId, u64)]| {
        ids.iter()
            .filter_map(|(id, c)| leases.get(id, *c).ok())
            .map(|l| l.regex_bytes())
            .sum::<usize>()
    };
    let max = 4 * bytes + 3;
    let leases = Leases::new(LeaseConfig {
        max_regex_bytes: max,
        ..LeaseConfig::default()
    });
    let mut held: Vec<(lodestar_search::LeaseId, u64)> =
        (1..=4).map(|c| (leases.insert(at_cap(), c), c)).collect();
    leases.get(&held[0].0, 1).unwrap();
    held.push((leases.insert(at_cap(), 5), 5));
    let alive: Vec<u64> = held
        .iter()
        .filter(|(id, c)| leases.get(id, *c).is_ok())
        .map(|(_, c)| *c)
        .collect();
    assert_eq!(alive, [1, 3, 4, 5]);
    assert!(total(&leases, &held) <= max);
    // The incoming lease counts for its inserter: with a share of two,
    // clients 1 to 4 hold two each, and client 5's second insert leaves
    // it at two too, so on the tie its own first lease goes, not
    // another client's.
    let max = 8 * bytes + 3;
    let leases = Leases::new(LeaseConfig {
        max_regex_bytes: max,
        ..LeaseConfig::default()
    });
    let mut held: Vec<(lodestar_search::LeaseId, u64)> = Vec::new();
    for c in 1..=4 {
        for _ in 0..2 {
            held.push((leases.insert(at_cap(), c), c));
        }
    }
    let first5 = leases.insert(at_cap(), 5);
    // That one pushed out the oldest of a client with two: client 1's
    // first.
    assert!(leases.get(&held[0].0, 1).is_err());
    let second5 = leases.insert(at_cap(), 5);
    assert!(leases.get(&first5, 5).is_err(), "the inserter's own went");
    assert!(leases.get(&second5, 5).is_ok());
    for (id, c) in &held[1..] {
        assert!(leases.get(id, *c).is_ok(), "client {c} kept its leases");
    }
    held.push((second5, 5));
    assert!(total(&leases, &held) <= max);
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

proptest! {
    #![proptest_config(ProptestConfig { cases: 4000, ..ProptestConfig::default() })]

    /// The `query` fuzz target's round trip, over text full of slashes and
    /// what ends a word: whatever parses renders back to the same AST
    /// under both readings of units.
    #[test]
    fn slashy_text_round_trips(
        text in prop::collection::vec(
            prop::sample::select(vec![
                "/", "a", "or", "(", ")", " ", "\"", "|", "\\", "name:", "x:", "-", "\0", "2023",
                "/2023", "/usr",
            ]),
            0..16,
        )
        .prop_map(|parts| parts.concat()),
    ) {
        use lodestar_search::{Units, parse_with};
        if let Ok(ast) = parse_with(&text, Units::Typed) {
            let stored = ast.to_query();
            for units in [Units::Stored, Units::Typed] {
                let again = parse_with(&stored, units).ok();
                prop_assert_eq!(again.as_ref(), Some(&ast), "{:?} → {:?}", text, stored);
            }
        }
    }
}
