//! Content-filtering state (§20.7).

use lodestar_store::{FilterState, Filtered, Held, Store, StoreConfig, WriteBatch};

fn store() -> Store {
    Store::open_in_memory(StoreConfig::default()).unwrap()
}

const A: [u8; 20] = [1; 20];
const B: [u8; 20] = [2; 20];
const C: [u8; 20] = [3; 20];

#[test]
fn cs_entries_and_switches_round_trip() {
    let mut s = store();
    assert_eq!(s.filter_state().unwrap(), FilterState::default());
    s.add_cs_hashes(&[A, B], 2, 10).unwrap();
    // A confirmation outranks the import; an import never downgrades.
    s.add_cs_hashes(&[A], 0, 11).unwrap();
    s.add_cs_hashes(&[A], 2, 12).unwrap();
    s.add_cs_sig(u64::MAX, 10).unwrap();
    s.set_core_switch("CS-4a", false).unwrap();
    let mut st = s.filter_state().unwrap();
    st.cs_hashes.sort();
    assert_eq!(st.cs_hashes, vec![(A, 0), (B, 2)]);
    assert_eq!(st.cs_sigs, vec![u64::MAX]);
    assert_eq!(st.switches, vec![("CS-4a".to_owned(), false)]);
    assert_eq!(s.remove_cs_hashes(&[B, C]).unwrap(), 1);
    assert!(s.remove_cs_sig(u64::MAX).unwrap());
    assert!(!s.remove_cs_sig(u64::MAX).unwrap());
    s.set_core_switch("CS-4a", true).unwrap();
    let st = s.filter_state().unwrap();
    assert_eq!(st.cs_hashes, vec![(A, 0)]);
    assert_eq!(st.switches, vec![("CS-4a".to_owned(), true)]);
}

#[test]
fn filtered_torrents_are_held_without_names() {
    let mut s = store();
    let sealed = b"opaque".to_vec();
    let filtered = [
        Filtered {
            hashes: &[A, B],
            rule: "CS-1a",
            action: 4,
            held: Some(Held::Tombstone),
            sealed: None,
            evidence: &[1],
            at: 100,
            expires_at: 0,
        },
        Filtered {
            hashes: &[C],
            rule: "CS-2a",
            action: 3,
            held: Some(Held::Quarantine),
            sealed: Some(&sealed),
            evidence: &[2, 0],
            at: 200,
            expires_at: 300,
        },
    ];
    s.write(&WriteBatch {
        filtered: &filtered,
        ..WriteBatch::default()
    })
    .unwrap();
    let mut held = s.filter_state().unwrap().held;
    held.sort();
    assert_eq!(
        held,
        vec![
            (A, Held::Tombstone),
            (B, Held::Tombstone),
            (C, Held::Quarantine)
        ]
    );
    let q = s.quarantined(10).unwrap();
    assert_eq!(q.len(), 1);
    assert_eq!((q[0].hash, q[0].rule.as_str()), (C, "CS-2a"));
    assert_eq!(q[0].sealed.as_deref(), Some(&b"opaque"[..]));
    let hits = s.filter_hits(0).unwrap();
    assert_eq!(hits.len(), 2, "one hit per torrent, not per hash");
    let h = &hits[0];
    assert_eq!(
        (h.hash, h.rule.as_str(), h.action, h.at, &h.evidence[..]),
        (A, "CS-1a", 4, 100, &[1u8][..])
    );

    // Expiry releases the quarantine; old hits age out.
    let (expired, purged) = s.purge_filter(300, 150).unwrap();
    assert_eq!((expired, purged), (vec![C], 1));
    assert!(s.quarantined(10).unwrap().is_empty());
    assert!(s.release(&A).unwrap());
    assert!(!s.release(&A).unwrap());
    assert_eq!(s.filter_state().unwrap().held, vec![(B, Held::Tombstone)]);
}
