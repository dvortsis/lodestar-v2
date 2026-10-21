//! Exactly-once accounting (§19.8) on random interleavings: claims from
//! several workers, commits and losses with current and stale tokens,
//! heartbeats, silence and expiry with re-grants.

use std::collections::HashSet;

use lodestar_cluster::{Claim, Claimed, LeaseTable, Token, WorkerId};
use lodestar_metainfo::InfoHash;
use lodestar_store::SourceId;
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Op {
    Claim {
        worker: u32,
        key: u8,
        source: u8,
    },
    /// End the key's lease with its current token (a commit or a loss).
    Finish {
        key: u8,
    },
    /// Report with a token that was valid once.
    Stale {
        pick: usize,
        key: u8,
    },
    Heartbeat {
        worker: u32,
    },
    Tick {
        secs: i64,
    },
    /// Expire silent workers and hand their keys to worker 0.
    Expire,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..3u32, 0..6u8, 0..4u8).prop_map(|(worker, key, source)| Op::Claim { worker, key, source }),
        2 => (0..6u8).prop_map(|key| Op::Finish { key }),
        2 => (any::<usize>(), 0..6u8).prop_map(|(pick, key)| Op::Stale { pick, key }),
        1 => (0..3u32).prop_map(|worker| Op::Heartbeat { worker }),
        1 => (1..400i64).prop_map(|secs| Op::Tick { secs }),
        1 => Just(Op::Expire),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn every_grant_ends_exactly_once(ops in proptest::collection::vec(op(), 0..120)) {
        let mut t = LeaseTable::new(7, 600);
        let mut now = 0i64;
        let (mut granted, mut ended) = (0usize, 0usize);
        let mut issued: Vec<Token> = Vec::new();
        let mut spent: HashSet<Token> = HashSet::new();
        for op in ops {
            match op {
                Op::Claim { worker, key, source } => {
                    let c = Claim {
                        key: InfoHash([key; 20]),
                        source: SourceId::new(source).unwrap(),
                        strategy: 0,
                        at: now,
                    };
                    match t.claim(WorkerId(worker), c, now) {
                        Claimed::Granted(token) => {
                            prop_assert!(!issued.contains(&token), "token reused");
                            issued.push(token);
                            granted += 1;
                        }
                        Claimed::Held { token, .. } => {
                            prop_assert!(t.is_current(&c.key, token));
                        }
                    }
                }
                Op::Finish { key } => {
                    let key = InfoHash([key; 20]);
                    if let Some(token) = t.get(&key).map(|l| l.token) {
                        prop_assert!(t.release(&key, token).is_some());
                        spent.insert(token);
                        ended += 1;
                    }
                }
                Op::Stale { pick, key } => {
                    let key = InfoHash([key; 20]);
                    if !issued.is_empty() {
                        let token = issued[pick % issued.len()];
                        if spent.contains(&token) {
                            prop_assert!(t.release(&key, token).is_none(), "a spent token released");
                        }
                    }
                }
                Op::Heartbeat { worker } => t.heartbeat(WorkerId(worker), now),
                Op::Tick { secs } => now += secs,
                Op::Expire => {
                    for (key, lease) in t.expire(now) {
                        // The old holder's token is dead from here on.
                        spent.insert(lease.token);
                        let token = t.regrant(WorkerId(0), key, lease, now);
                        prop_assert!(!issued.contains(&token));
                        issued.push(token);
                    }
                }
            }
            // Conservation: harvested = ended + still in flight.
            prop_assert_eq!(granted, ended + t.len());
        }
    }
}
