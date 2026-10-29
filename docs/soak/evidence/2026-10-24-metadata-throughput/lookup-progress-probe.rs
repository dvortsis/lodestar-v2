// Offline diagnostic: the real Lookup implementation, with the send outcome
// chosen by this harness. This does NOT execute the full Actor or network.
#![allow(dead_code)]
pub use lodestar_dht::{bep33, compact, routing};
#[path = "../../../../crates/dht/src/node/lookup.rs"]
mod lookup;

fn main() {
    let mut lookup = lookup::Lookup::new(compact::NodeId([0; 20]), 0);
    lookup.info_hash = Some(lodestar_metainfo::InfoHash([0; 20]));
    for n in 1..=16u8 {
        lookup.add(compact::NodeInfo {
            id: compact::NodeId([n; 20]),
            addr: ([192, 0, 2, n], 6881).into(),
        });
    }
    // Actor::drive_lookup calls next exactly once, then on_failure for
    // each Sent::Failed or non-scrape Sent::Throttled outcome.
    let batch = lookup.next();
    println!("initial_batch={}", batch.len());
    for node in batch {
        lookup.on_failure(&node.id);
    }
    let state = format!("{lookup:?}");
    assert!(state.contains("in_flight: 0"));
    assert!(state.contains("extra_in_flight: 0"));
    println!("in_flight=0 extra_in_flight=0 fresh_candidates={}", state.matches("state: Fresh").count());
    println!("is_done={}", lookup.is_done());
    assert!(!lookup.is_done());
    // No request was sent, so no response or pending timeout will wake this
    // lookup. A manual second drive proves fresh work is still available.
    println!("manual_second_drive_batch={}", lookup.next().len());
}
