//! Every example in `docs/query-language.md` is checked with the real
//! parser. The page is the source: a code block tagged `query` must
//! parse (and round-trip), and one tagged `query-fail` lists
//! `query => message fragment` lines that must fail with that message.
//! Mermaid blocks are skipped; any other code block is an error, so
//! nothing on the page goes unchecked.

use lodestar_search::parse;

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
            if let Some(tag) = lang {
                if !matches!(tag, "query" | "query-fail" | "mermaid") {
                    problems.push(format!("line {at}: code block tagged {tag:?}"));
                }
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
                    // Text and AST agree: the canonical form parses back the same.
                    Ok(node) if parse(&node.to_query()).as_ref() != Ok(&node) => {
                        problems.push(format!("line {at}: {line:?} doesn't round-trip"));
                    }
                    Ok(_) => {}
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
