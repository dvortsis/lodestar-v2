//! The query parser never panics on any input, and anything it accepts
//! renders back to text that parses to the same AST (when that text is
//! within the length limit). The same holds for the query a search runs
//! (SRC-3: unverified items hidden by default), and the AST survives
//! JSON, the form the API takes it in.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_search::{Node, parse};

fn round_trips(ast: &Node, text: &str) {
    let rendered = ast.to_query();
    // Canonical text can be longer than what was typed (quoting,
    // `NEAR/5`, exact byte counts); past the limit it can't re-parse.
    if rendered.len() > lodestar_search::query::MAX_QUERY_BYTES {
        return;
    }
    assert_eq!(
        parse(&rendered).as_ref().ok(),
        Some(ast),
        "{text:?} → {rendered:?}"
    );
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(ast) = parse(text) {
        round_trips(&ast, text);
        let json = serde_json::to_string(&ast).expect("an AST serializes");
        assert_eq!(
            serde_json::from_str::<Node>(&json).ok().as_ref(),
            Some(&ast)
        );
        // The default clause goes on only when `verified:` isn't named,
        // and once: applying it twice changes nothing.
        let shown = ast.clone().without_unverified();
        assert!(shown.mentions_verified());
        assert_eq!(shown.clone().without_unverified(), shown);
        if ast.mentions_verified() {
            assert_eq!(shown, ast);
        }
        let _ = ast.population_fields();
        // One clause more can pass the clause limit; below it, the shown
        // query is one the parser produces.
        if let Ok(again) = parse(&shown.to_query()) {
            assert_eq!(again, shown, "{text:?}");
        }
    }
});
