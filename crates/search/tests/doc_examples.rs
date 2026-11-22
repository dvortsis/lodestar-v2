//! Every example in `docs/query-language.md` is checked with the real
//! parser. The page is the source: a code block tagged `query` must
//! parse (and round-trip), and one tagged `query-fail` lists
//! `query => message fragment` lines that must fail with that message.
//! Mermaid and `text` blocks (a request body, say) are skipped; any
//! other code block is an error, so nothing on the page goes unchecked.
//! The page describes what a user types, so examples read with the
//! typed-unit reading (§10.12); each also still parses as stored text.

use lodestar_search::{Node, ParseError, Units, parse_with};

fn parse(q: &str) -> Result<Node, ParseError> {
    parse_with(q, Units::Typed)
}

const PAGE: &str = include_str!("../../../docs/query-language.md");

#[test]
fn examples_parse_as_documented() {
    let mut problems = Vec::new();
    let mut checked = 0;
    let mut lang: Option<&str> = None;
    for (n, line) in PAGE.lines().enumerate() {
        let at = n + 1;
        if let Some(tag) = line.strip_prefix("```") {
            lang = match lang {
                Some(_) => None,
                None => Some(tag.trim()),
            };
            if let Some(tag) = lang
                && !matches!(tag, "query" | "query-fail" | "mermaid" | "text")
            {
                problems.push(format!("line {at}: code block tagged {tag:?}"));
            }
            continue;
        }
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        match lang {
            Some("query") => {
                checked += 1;
                match parse(line) {
                    Err(e) => problems.push(format!("line {at}: {line:?}: {e}")),
                    Ok(node) => {
                        // Text and AST agree: the stored text parses back the
                        // same under both readings, the typed echo as typed.
                        let stored = node.to_query();
                        let back = |units| parse_with(&stored, units).ok();
                        if back(Units::Stored).as_ref() != Some(&node)
                            || back(Units::Typed).as_ref() != Some(&node)
                            || parse(&node.to_query_typed()).as_ref() != Ok(&node)
                        {
                            problems.push(format!("line {at}: {line:?} doesn't round-trip"));
                        }
                        // The stored path stays covered.
                        if let Err(e) = parse_with(line, Units::Stored) {
                            problems.push(format!("line {at}: {line:?} stored: {e}"));
                        }
                    }
                }
            }
            Some("query-fail") => {
                checked += 1;
                let Some((query, want)) = line.rsplit_once(" => ") else {
                    problems.push(format!("line {at}: expected `query => message`"));
                    continue;
                };
                match parse(query) {
                    Ok(_) => problems.push(format!("line {at}: {query:?} should not parse")),
                    Err(e) if !e.message.contains(want) => {
                        problems.push(format!(
                            "line {at}: {query:?} failed with {:?}, not {want:?}",
                            e.message
                        ));
                    }
                    Err(_) => {}
                }
            }
            _ => {}
        }
    }
    assert!(lang.is_none(), "a code block is never closed");
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert!(checked > 120, "only {checked} examples checked");
}
