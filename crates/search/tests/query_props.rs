//! Parser invariants: never panics, and whatever parses renders back to
//! text that parses to the same AST (the UI builder depends on it).

use lodestar_search::query::NumField;
use lodestar_search::{Node, parse};
use proptest::prelude::*;

/// A byte bound: near whole units (the brush's edges), near `u64::MAX`, or any.
fn bound() -> impl Strategy<Value = Option<u64>> {
    let near_unit =
        (0u32..64, -2i64..=2).prop_map(|(shift, d)| (1u64 << shift).wrapping_add_signed(d));
    let near_max = (0u64..3).prop_map(|d| u64::MAX - d);
    prop::option::of(prop_oneof![near_unit, near_max, any::<u64>()])
}

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
    "file:(",
    "file:(ext:flac size:>50MB)",
    "file:sample",
    "file:\"x y\"",
    "0123456789abcdef0123456789abcdef01234567",
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
