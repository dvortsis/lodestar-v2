//! The shared bounded JSON reader never panics, holds its caps, and
//! agrees with `serde_json` on what it accepts. The first byte picks
//! tight caps; the rest is the document. `crates/sources-feeds/tests/
//! json_props.rs` runs the reader-only checks over generated input.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_sources_feeds::json::{Limits, R, Reader, Stop, TextError, Walk, decode};
use serde_json::{Map, Value};

/// Caps wide enough for anything `serde_json` reads (it stops at 128
/// levels).
const WIDE: Limits = Limits {
    max_depth: 256,
    max_values: usize::MAX,
    max_array: usize::MAX,
    max_text: usize::MAX,
};

/// Reads the next value through the walking API into a tree, decoding
/// every string and key.
fn build(r: &mut Reader<'_>, depth: usize) -> R<Value> {
    assert!(depth <= r.limits().max_depth, "walked past max_depth");
    Ok(match r.peek() {
        Some(b'{') => {
            let mut m = Map::new();
            r.object(|r, k| {
                let k = decode(k).unwrap_or_default();
                let v = build(r, depth + 1)?;
                m.insert(k, v);
                Ok(())
            })?;
            Value::Object(m)
        }
        Some(b'[') => {
            let mut a = Vec::new();
            r.array(|r, _| {
                a.push(build(r, depth + 1)?);
                Ok(())
            })?;
            Value::Array(a)
        }
        Some(b'"') => match r.text()? {
            Ok(s) => Value::String(s),
            Err(TextError::OverCap) => Value::Null,
            Err(TextError::Malformed) => Value::Bool(false),
        },
        Some(b'-' | b'0'..=b'9') => {
            let n = r.num()?;
            serde_json::from_str(n).unwrap_or(Value::Null)
        }
        Some(c) => {
            r.skip()?;
            match c {
                b't' => Value::Bool(true),
                b'f' => Value::Bool(false),
                _ => Value::Null,
            }
        }
        None => {
            r.skip()?;
            Value::Null
        }
    })
}

fn skip_all(s: &str, l: Limits) -> R<bool> {
    let mut r = Reader::new(s, l);
    let over = r.skip()?;
    r.end()?;
    Ok(over)
}

fn walk_all(s: &str, l: Limits) -> R<Value> {
    let mut r = Reader::new(s, l);
    let v = build(&mut r, 0)?;
    r.end()?;
    Ok(v)
}

fuzz_target!(|data: &[u8]| {
    let (pick, body) = data.split_first().map_or((0, data), |(p, b)| (*p, b));
    let Ok(text) = std::str::from_utf8(body) else {
        return;
    };
    let small = Limits {
        max_depth: 1 + usize::from(pick % 8),
        max_values: 1 + usize::from(pick) * 4,
        max_array: usize::from(pick % 6),
        max_text: usize::from(pick) * 4,
    };
    for l in [WIDE, small] {
        let skipped = skip_all(text, l);
        let walked = walk_all(text, l);
        // Walking and skipping read the same grammar under the same
        // structural caps.
        assert_eq!(skipped.as_ref().err(), walked.as_ref().err(), "{text:?}");
        if let Err(e) = skipped {
            assert!(matches!(e, Stop::Malformed | Stop::Depth | Stop::Elements));
        }
    }
    // Whatever serde_json accepts, the reader accepts and reads the same.
    if let Ok(want) = serde_json::from_str::<Value>(text) {
        assert_eq!(skip_all(text, WIDE), Ok(false), "{text:?}");
        let got = walk_all(text, WIDE).expect("serde_json accepted it");
        assert_eq!(got, want, "{text:?}");
    }
});
