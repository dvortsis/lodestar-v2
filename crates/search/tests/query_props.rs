//! Parser invariants: never panics, and whatever parses renders back to
//! text that parses to the same AST (the UI builder depends on it).

use lodestar_search::query::NumField;
use lodestar_search::{Node, Units, parse, parse_with};
use proptest::prelude::*;

/// A byte bound: near whole units, binary (the brush's edges) or decimal
/// (typed sizes), near `u64::MAX`, or any.
fn bound() -> impl Strategy<Value = Option<u64>> {
    let near_unit =
        (0u32..64, -2i64..=2).prop_map(|(shift, d)| (1u64 << shift).wrapping_add_signed(d));
    let near_decimal = (1u64..2000, 1u32..=5, -1i64..=1)
        .prop_map(|(n, k, d)| (n * 1000u64.pow(k)).wrapping_add_signed(d));
    let near_max = (0u64..3).prop_map(|d| u64::MAX - d);
    prop::option::of(prop_oneof![near_unit, near_decimal, near_max, any::<u64>()])
}

const UNITS: &[(&str, u64, u64)] = &[
    // (unit, stored multiplier, typed multiplier on size: and largest:)
    ("", 1, 1),
    ("b", 1, 1),
    ("k", 1 << 10, 1 << 10),
    ("kb", 1 << 10, 1_000),
    ("KB", 1 << 10, 1_000),
    ("kib", 1 << 10, 1 << 10),
    ("m", 1 << 20, 1 << 20),
    ("MB", 1 << 20, 1_000_000),
    ("MiB", 1 << 20, 1 << 20),
    ("G", 1 << 30, 1 << 30),
    ("GB", 1 << 30, 1_000_000_000),
    ("gB", 1 << 30, 1_000_000_000),
    ("GiB", 1 << 30, 1 << 30),
    ("t", 1 << 40, 1 << 40),
    ("TB", 1 << 40, 1_000_000_000_000),
    ("TiB", 1 << 40, 1 << 40),
];

const FRAGMENTS: &[&str] = &[
    "dark",
    "knight",
    "web-dl",
    "x265",
    "千と",
    "(",
    ")",
    "|",
    "OR",
    "AND",
    "-",
    "!",
    "\"",
    "NEAR/3",
    "NEAR",
    "name:",
    "path:",
    "size:>1GB",
    "size:1MB..2GB",
    "size:1GiB..<4GiB",
    "size:<1GB",
    "size:1.5TB",
    "largest:700MB..<1GB",
    "piece:<16MiB",
    "files:<=3",
    "largest:>=1.5GB",
    "ext:mkv,mp4",
    "comput*",
    "~knigth",
    "discovered:<7d",
    "age:>2w",
    "discovered:2026-01-01",
    "discovered:2025-12-01..2026-01-31",
    "discovered:>2026-02-01",
    "private:no",
    "v2:yes",
    "partial:yes",
    "Re:Zero",
    "*",
    "~",
    "sise:1",
    "res:1080p",
    "res:4k",
    "codec:x265,h264",
    "hdr:dovi",
    "audio:DD+",
    "lang:french",
    "group:NTb",
    "type:movie,tv",
    "has:nfo",
    "resolved:files",
    "ambiguous:yes",
    "title:",
    "year:2019..2021",
    "s:2",
    "e:>5",
    "junk:<20",
    "seeders:>=5",
    "leechers:0",
    "trackers:3..20",
    "dht:>100",
    "seen:<24h",
    "seen:2026-01-01..2026-01-31",
    "time:22:00..02:00",
    "time:9..17",
    "time:7",
    "cat:video>=80%",
    "cat:software=0",
    "bytes:video=10..50",
    "cat:audio",
    "verified:no",
    "verified:yes,no",
    "verified:maybe",
    "source:nyaa",
    "source:Nyaa,dht",
    "source:-x",
    "network:bt",
    "network:bt-i2p,ipfs",
    "network:tor",
    "file:(",
    "file:(ext:flac size:>50MB)",
    "file:sample",
    "file:\"x y\"",
    "0123456789abcdef0123456789abcdef01234567",
    "sig:7a9f3c12dd0e44b1",
    "sig:7A9F3C12DD0E44B1",
    "sig:0123456789abcdef0123456789abcdef01234567",
    "sig:magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
    "sig:7a9f3c12",
    "sig:",
];

fn query() -> impl Strategy<Value = String> {
    prop::collection::vec((prop::sample::select(FRAGMENTS), prop::bool::ANY), 0..14).prop_map(
        |parts| {
            let mut q = String::new();
            for (frag, space) in parts {
                q.push_str(frag);
                if space {
                    q.push(' ');
                }
            }
            q
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    #[test]
    fn round_trips(q in query()) {
        if let Ok(ast) = parse(&q) {
            let text = ast.to_query();
            let again = parse(&text);
            prop_assert_eq!(again.as_ref().ok(), Some(&ast), "{} → {} → {:?}", q, text, again);
            let json = serde_json::to_string(&ast).unwrap();
            prop_assert_eq!(serde_json::from_str::<Node>(&json).unwrap(), ast);
        }
    }

    /// Byte ranges round-trip whatever their bounds, and the printed text
    /// is a fixed point (§10.12's units, `a..<b` for a brush's top edge).
    #[test]
    fn byte_ranges_round_trip(
        lo in bound(),
        hi in bound(),
        field in prop::sample::select(&[NumField::Size, NumField::Largest, NumField::Piece][..]),
    ) {
        let (lo, hi) = match (lo, hi) {
            (None, None) => (Some(0), None),
            (Some(l), Some(h)) if l > h => (Some(h), Some(l)),
            b => b,
        };
        let ast = Node::Range { field, lo, hi };
        let text = ast.to_query();
        prop_assert_eq!(parse(&text).ok(), Some(ast), "{}", text);
        // Stored filter text must parse on the release before (FX-6-I9).
        prop_assert!(!text.contains("..<"), "{}", text);
        let again = parse(&text).unwrap().to_query();
        prop_assert_eq!(again, text);
    }

    #[test]
    fn never_panics(q in "\\PC{0,200}") {
        let _ = parse(&q);
        let _ = parse_with(&q, Units::Typed);
    }

    /// Typed text round-trips through both printers: `to_query()` (what's
    /// stored) reads back the same under both readings, and
    /// `to_query_typed()` (the bar, the URL) under the typed one.
    #[test]
    fn typed_round_trips(q in query()) {
        if let Ok(ast) = parse_with(&q, Units::Typed) {
            let stored = ast.to_query();
            prop_assert_eq!(parse_with(&stored, Units::Stored).ok(), Some(ast.clone()), "{}", stored);
            prop_assert_eq!(parse_with(&stored, Units::Typed).ok(), Some(ast.clone()), "{}", stored);
            let typed = ast.to_query_typed();
            prop_assert_eq!(parse_with(&typed, Units::Typed).ok(), Some(ast), "{} → {}", q, typed);
        }
    }

    /// The size grammar under both readings: `KB`…`TB` in 1,024s stored and
    /// 1,000s typed on `size:`/`largest:`; IEC units, single letters and
    /// bytes the same under both; `piece:` 1,024-based under both.
    #[test]
    fn size_grammar_under_both_readings(
        n in 0u64..100_000,
        unit in prop::sample::select(UNITS),
        field in prop::sample::select(&["size", "largest", "piece"][..]),
    ) {
        let (u, stored, typed) = unit;
        let q = format!("{field}:{n}{u}");
        let at = |units| match parse_with(&q, units) {
            Ok(Node::Range { lo, hi, .. }) => { assert_eq!(lo, hi); lo }
            other => panic!("{q}: {other:?}"),
        };
        prop_assert_eq!(at(Units::Stored), Some(n * stored));
        let want = if field == "piece" { stored } else { typed };
        prop_assert_eq!(at(Units::Typed), Some(n * want));
        prop_assert_eq!(parse(&q).ok(), parse_with(&q, Units::Stored).ok());
    }

    /// Typed byte ranges print a fixed point that reads back as typed.
    #[test]
    fn typed_byte_ranges_round_trip(
        lo in bound(),
        hi in bound(),
        field in prop::sample::select(&[NumField::Size, NumField::Largest, NumField::Piece][..]),
    ) {
        let (lo, hi) = match (lo, hi) {
            (None, None) => (Some(0), None),
            (Some(l), Some(h)) if l > h => (Some(h), Some(l)),
            b => b,
        };
        let ast = Node::Range { field, lo, hi };
        let text = ast.to_query_typed();
        prop_assert_eq!(parse_with(&text, Units::Typed).ok(), Some(ast), "{}", text);
        if field == NumField::Piece {
            prop_assert_eq!(&text, &Node::Range { field, lo, hi }.to_query());
        }
        let again = parse_with(&text, Units::Typed).unwrap().to_query_typed();
        prop_assert_eq!(again, text);
    }
}

/// The generator must exercise the parser, not just its errors.
#[test]
fn most_generated_queries_parse() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let strat = query();
    let ok = (0..2000)
        .filter(|_| parse(&strat.new_tree(&mut runner).unwrap().current()).is_ok())
        .count();
    assert!(ok > 300, "only {ok} of 2000 parsed");
}
