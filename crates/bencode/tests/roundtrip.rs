//! Property tests: canonical round-trips and no panics on arbitrary input.

use std::collections::BTreeMap;

use lodestar_bencode::{OwnedValue, Value, decode, decode_prefix, decode_strict, to_vec};
use proptest::prelude::*;

fn arb_value() -> impl Strategy<Value = OwnedValue> {
    let leaf = prop_oneof![
        any::<i64>().prop_map(OwnedValue::Int),
        // Small alphabet so dictionary keys collide and order matters.
        prop::collection::vec(prop_oneof![Just(b'a'), Just(b'b'), any::<u8>()], 0..12)
            .prop_map(OwnedValue::Bytes),
    ];
    leaf.prop_recursive(6, 96, 8, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..8).prop_map(OwnedValue::List),
            prop::collection::btree_map(prop::collection::vec(any::<u8>(), 0..6), inner, 0..8)
                .prop_map(OwnedValue::Dict),
        ]
    })
}

fn raw_span<'a>(v: &Value<'a>) -> Option<&'a [u8]> {
    match v {
        Value::List(l) => Some(l.raw()),
        Value::Dict(d) => Some(d.raw()),
        _ => None,
    }
}

/// Re-serialises a value with dictionary keys reversed (non-canonical but
/// unambiguous), for exercising the lenient decoder.
fn encode_reversed(v: &OwnedValue, out: &mut Vec<u8>) {
    match v {
        OwnedValue::Dict(d) => {
            out.push(b'd');
            for (k, v) in d.iter().rev() {
                out.extend_from_slice(format!("{}:", k.len()).as_bytes());
                out.extend_from_slice(k);
                encode_reversed(v, out);
            }
            out.push(b'e');
        }
        OwnedValue::List(l) => {
            out.push(b'l');
            for item in l {
                encode_reversed(item, out);
            }
            out.push(b'e');
        }
        other => out.extend_from_slice(&to_vec(other)),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    #[test]
    fn encode_then_strict_decode_roundtrips(v in arb_value()) {
        let bytes = to_vec(&v);
        let decoded = decode_strict(&bytes).unwrap();
        prop_assert_eq!(decoded.to_owned_value(), v.clone());
        prop_assert_eq!(to_vec(&decoded), bytes.clone());
        if let Some(raw) = raw_span(&decoded) {
            prop_assert_eq!(raw, &bytes[..]);
        }
    }

    #[test]
    fn lenient_accepts_unsorted_and_canonicalises(v in arb_value()) {
        let mut bytes = Vec::new();
        encode_reversed(&v, &mut bytes);
        let decoded = decode(&bytes).unwrap();
        prop_assert_eq!(decoded.to_owned_value(), v.clone());
        prop_assert_eq!(to_vec(&decoded), to_vec(&v));
        if let Some(raw) = raw_span(&decoded) {
            prop_assert_eq!(raw, &bytes[..]);
        }
    }

    /// Strict decoding accepts only canonical bytes: success implies the
    /// re-encoding is byte-identical.
    #[test]
    fn strict_success_implies_canonical(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        if let Ok(v) = decode_strict(&bytes) {
            prop_assert_eq!(to_vec(&v), bytes);
        }
    }

    /// Lenient success implies the canonical re-encoding decodes strictly to
    /// the same value. Also the main never-panic check on arbitrary bytes.
    #[test]
    fn lenient_success_is_stable(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        if let Ok(v) = decode(&bytes) {
            let canonical = to_vec(&v);
            let again = decode_strict(&canonical).unwrap();
            prop_assert_eq!(again.to_owned_value(), v.to_owned_value());
        }
    }

    /// Byte-level corruption of valid encodings never panics, and prefix
    /// decoding never reports consuming more than the input.
    #[test]
    fn corrupted_input_never_panics(
        v in arb_value(),
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..4),
        cut in any::<prop::sample::Index>(),
    ) {
        let mut bytes = to_vec(&v);
        for (idx, b) in edits {
            let i = idx.index(bytes.len());
            bytes[i] = b;
        }
        let _ = decode(&bytes);
        let _ = decode_strict(&bytes);
        let truncated = &bytes[..cut.index(bytes.len() + 1)];
        if let Ok((_, used)) = decode_prefix(truncated, &Default::default()) {
            prop_assert!(used <= truncated.len());
        }
    }
}

#[test]
fn owned_dict_is_canonical() {
    let mut d = BTreeMap::new();
    d.insert(b"zz".to_vec(), OwnedValue::Int(1));
    d.insert(b"a".to_vec(), OwnedValue::from("x"));
    assert_eq!(to_vec(&OwnedValue::Dict(d)), b"d1:a1:x2:zzi1ee");
}
