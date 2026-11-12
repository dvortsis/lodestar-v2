//! `resolved:plugin` is read by the query language even while the
//! catalogue doesn't list it (ENR-10, `docs/decisions/2026-11-12-ENR-10.md`).

use lodestar_search::parse;
use lodestar_search::query::{Node, TagKey};

#[test]
fn resolved_plugin_parses() {
    assert_eq!(
        parse("resolved:plugin").unwrap(),
        Node::Tag {
            key: TagKey::Resolved,
            any: vec!["plugin".into()]
        }
    );
    assert!(parse("resolved:nope").is_err());
}
