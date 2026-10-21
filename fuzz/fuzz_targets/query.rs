//! The query parser never panics on any input, and anything it accepts
//! renders back to text that parses to the same AST (when that text is
//! within the length limit).
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_search::parse;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(ast) = parse(text) {
        let rendered = ast.to_query();
        // Canonical text can be longer than what was typed (quoting,
        // `NEAR/5`, exact byte counts); past the limit it can't re-parse.
        if rendered.len() > lodestar_search::query::MAX_QUERY_BYTES {
            return;
        }
        assert_eq!(parse(&rendered).as_ref().ok(), Some(&ast), "{text:?} → {rendered:?}");
    }
});
