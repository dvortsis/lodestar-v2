//! Parser invariants: never panics, and whatever parses renders back to
//! text that parses to the same AST (the UI builder depends on it).

use lodestar_search::{Node, parse};
use proptest::prelude::*;

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
