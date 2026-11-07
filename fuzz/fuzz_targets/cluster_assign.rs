//! The `Assign` payload (WK-5, architecture §19.5): arbitrary bytes never
//! panic, an unknown version or a wrong length is refused, and whatever
//! decodes is a valid assignment that encodes back to the same bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_cluster::assign::{ASSIGN_V1_LEN, ASSIGN_VERSION, Assignment};
use lodestar_cluster::proto;

fuzz_target!(|data: &[u8]| {
    if let Ok(a) = Assignment::decode(data) {
        assert_eq!(data.len(), ASSIGN_V1_LEN);
        assert_eq!(data[0], ASSIGN_VERSION);
        assert!(a.validate().is_ok());
        assert_eq!(a.encode().as_deref(), Some(data));
        let m = a.to_message().expect("a decoded assignment encodes");
        assert_eq!(Assignment::from_message(&m), Ok(Some(a)));
    }
    // The same bytes as a message under any revision: only the payload's
    // own revision is accepted.
    if data.len() >= 8 {
        let revision = u64::from_le_bytes(data[..8].try_into().unwrap());
        let m = proto::Assign {
            revision,
            payload: data[8..].to_vec(),
        };
        if let Ok(Some(a)) = Assignment::from_message(&m) {
            assert_eq!(a.revision, revision);
        }
    }
});
