//! RP-4 (#495): SP-3 counts a find against its own source only for the
//! live feeds the pipeline names by plugin id (`LIVE_FEEDS`). The ids
//! live in `crates/sources-plugins`, which the pipeline can't see, so
//! this test, which sees both, keeps a rename from silently switching
//! SP-3's per-source count off.

use lodestar_pipeline::LIVE_FEEDS;
use lodestar_sources_plugins::nyaa;

#[test]
fn live_feeds_name_the_nyaa_plugins() {
    assert_eq!(LIVE_FEEDS, [nyaa::ID, nyaa::SUKEBEI_ID]);
}
