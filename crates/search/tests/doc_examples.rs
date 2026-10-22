//! Every example in `docs/query-language.md` parses (or fails, where the
//! page says so) with the real parser.

use lodestar_search::parse;

const EXAMPLES: &str = include_str!("../../../docs/query-language-examples.txt");

#[test]
fn examples_parse_as_documented() {
    let mut checked = 0;
    let mut problems = Vec::new();
    for (n, line) in EXAMPLES.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (should_fail, query) = match line.strip_prefix('!') {
            // `!ext:exe` is a query; failing ones are written `!` + query
            // and only listed after the "fail" marker comment below.
            Some(rest) if n >= first_failing_line() => (true, rest),
            _ => (false, line),
        };
        let result = parse(query);
        if should_fail {
            if result.is_ok() {
                problems.push(format!("line {}: {query:?} should not parse", n + 1));
            }
        } else {
            match result {
                Err(e) => problems.push(format!("line {}: {query:?}: {e}", n + 1)),
                // Text and AST agree: the canonical form parses back the same.
                Ok(node) if parse(&node.to_query()).as_ref() != Ok(&node) => {
                    problems.push(format!("line {}: {query:?} doesn't round-trip", n + 1));
                }
                Ok(_) => {}
            }
        }
        checked += 1;
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert!(checked > 100, "only {checked} examples checked");
}

/// The first line of the "Things that fail" section, where a leading `!`
/// means "must fail" rather than the `!` negation alias.
fn first_failing_line() -> usize {
    EXAMPLES
        .lines()
        .position(|l| l.starts_with("# Things that fail"))
        .expect("the failing section")
}
