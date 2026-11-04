//! Property tests for the shared JSON reader: it reads back any
//! well-formed document, holds each cap exactly, never panics, and
//! walking a document agrees with skipping it.
//! `fuzz/fuzz_targets/json.rs` adds a check against `serde_json`.

use lodestar_sources_feeds::json::{Limits, R, Reader, Stop, TextError, Walk, decode};
use proptest::prelude::*;

const WIDE: Limits = Limits {
    max_depth: 64,
    max_values: usize::MAX,
    max_array: usize::MAX,
    max_text: usize::MAX,
};

/// A JSON value, written out by [`write`].
#[derive(Debug, Clone, PartialEq)]
enum V {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Arr(Vec<V>),
    Obj(Vec<(String, V)>),
}

/// `s` as a JSON string: quotes, backslashes and control characters
/// escaped, other characters escaped as `\uXXXX` units when `esc`.
fn quote(s: &str, esc: bool, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 || (esc && !c.is_ascii_alphanumeric()) => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{u:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write(v: &V, esc: bool, out: &mut String) {
    match v {
        V::Null => out.push_str("null"),
        V::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        V::Num(n) => out.push_str(n),
        V::Str(s) => quote(s, esc, out),
        V::Arr(a) => {
            out.push_str("[ ");
            for (n, e) in a.iter().enumerate() {
                if n > 0 {
                    out.push_str(" ,\n");
                }
                write(e, esc, out);
            }
            out.push(']');
        }
        V::Obj(m) => {
            out.push('{');
            for (n, (k, e)) in m.iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                quote(k, false, out);
                out.push_str("\t: ");
                write(e, esc, out);
            }
            out.push_str("\r\n}");
        }
    }
}

/// Values read through the walking API, with strings decoded; a number
/// as written.
fn read(r: &mut Reader<'_>) -> R<V> {
    Ok(match r.peek() {
        Some(b'{') => {
            let mut m = Vec::new();
            r.object(|r, k| {
                let k = decode(k).expect("a key the test wrote");
                m.push((k, read(r)?));
                Ok(())
            })?;
            V::Obj(m)
        }
        Some(b'[') => {
            let mut a = Vec::new();
            r.array(|r, _| {
                a.push(read(r)?);
                Ok(())
            })?;
            V::Arr(a)
        }
        Some(b'"') => match r.text()? {
            Ok(s) => V::Str(s),
            Err(TextError::OverCap) => V::Str("<over>".into()),
            Err(TextError::Malformed) => V::Str("<bad>".into()),
        },
        Some(b'-' | b'0'..=b'9') => {
            r.value()?;
            V::Num(r.number()?.to_owned())
        }
        Some(b't') => {
            r.skip()?;
            V::Bool(true)
        }
        Some(b'f') => {
            r.skip()?;
            V::Bool(false)
        }
        _ => {
            r.skip()?;
            V::Null
        }
    })
}

fn read_all(s: &str, l: Limits) -> R<V> {
    let mut r = Reader::new(s, l);
    let v = read(&mut r)?;
    r.end()?;
    Ok(v)
}

fn skip_all(s: &str, l: Limits) -> R<bool> {
    let mut r = Reader::new(s, l);
    let over = r.skip()?;
    r.end()?;
    Ok(over)
}

fn depth(v: &V) -> usize {
    match v {
        V::Arr(a) => 1 + a.iter().map(depth).max().unwrap_or(0),
        V::Obj(m) => 1 + m.iter().map(|(_, e)| depth(e)).max().unwrap_or(0),
        _ => 0,
    }
}

fn values(v: &V) -> usize {
    match v {
        V::Arr(a) => 1 + a.iter().map(values).sum::<usize>(),
        V::Obj(m) => 1 + m.iter().map(|(_, e)| values(e)).sum::<usize>(),
        _ => 1,
    }
}

fn longest_array(v: &V) -> usize {
    match v {
        V::Arr(a) => a.iter().map(longest_array).max().unwrap_or(0).max(a.len()),
        V::Obj(m) => m.iter().map(|(_, e)| longest_array(e)).max().unwrap_or(0),
        _ => 0,
    }
}

fn number() -> impl Strategy<Value = String> {
    prop_oneof![
        any::<i64>().prop_map(|n| n.to_string()),
        "-?(0|[1-9][0-9]{0,30})(\\.[0-9]{1,8})?([eE][+-]?[0-9]{1,3})?",
    ]
}

fn value() -> impl Strategy<Value = V> {
    let text = "[a-zA-Z0-9 é—😀\"\\\\/\u{1}\u{1f}\u{7f}\u{2028}]{0,16}";
    let leaf = prop_oneof![
        Just(V::Null),
        any::<bool>().prop_map(V::Bool),
        number().prop_map(V::Num),
        text.prop_map(V::Str),
    ];
    leaf.prop_recursive(6, 64, 6, move |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..6).prop_map(V::Arr),
            proptest::collection::vec((text, inner), 0..6).prop_map(V::Obj),
        ]
    })
}

/// Pieces that make mostly-JSON input.
fn fragment() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("{".to_string()),
        Just("}".to_string()),
        Just("[".to_string()),
        Just("]".to_string()),
        Just(",".to_string()),
        Just(":".to_string()),
        Just("\"k\":".to_string()),
        Just("\"\\ud800\"".to_string()),
        Just("\"\\ud83d\\ude00\"".to_string()),
        Just("-0.5e+3".to_string()),
        Just("null".to_string()),
        Just("true".to_string()),
        "[a-z{}\\[\\]\":,0-9\\\\ .eE+\u{1}-]{0,12}",
    ]
}

proptest! {
    #[test]
    fn reads_back_any_document(v in value(), esc in any::<bool>()) {
        let mut s = String::new();
        write(&v, esc, &mut s);
        prop_assert_eq!(read_all(&s, WIDE), Ok(v.clone()));
        prop_assert_eq!(skip_all(&s, WIDE), Ok(false));
    }

    #[test]
    fn caps_hold_exactly(v in value(), d in 0usize..8, n in 1usize..40, a in 0usize..7) {
        let mut s = String::new();
        write(&v, false, &mut s);
        let at = |l: Limits| (skip_all(&s, l), read_all(&s, l).map(drop));
        let want = |ok: bool, stop: Stop| if ok { Ok(()) } else { Err(stop) };

        let l = Limits { max_depth: d, ..WIDE };
        let (skip, walk) = at(l);
        prop_assert_eq!(skip.map(drop), want(depth(&v) <= d, Stop::Depth));
        prop_assert_eq!(walk, want(depth(&v) <= d, Stop::Depth));

        let l = Limits { max_values: n, ..WIDE };
        let (skip, walk) = at(l);
        prop_assert_eq!(skip.map(drop), want(values(&v) <= n, Stop::Elements));
        prop_assert_eq!(walk, want(values(&v) <= n, Stop::Elements));

        // An over-long array is reported by skip, never a stop.
        let l = Limits { max_array: a, ..WIDE };
        prop_assert_eq!(skip_all(&s, l), Ok(longest_array(&v) > a));
    }

    #[test]
    fn text_cap_is_on_raw_bytes(t in "[a-zé😀\"\\\\]{0,20}", cap in 0usize..40) {
        let mut s = String::new();
        quote(&t, false, &mut s);
        let raw = s.len() - 2;
        let got = Reader::new(&s, Limits { max_text: cap, ..WIDE }).text();
        if raw > cap {
            prop_assert_eq!(got, Ok(Err(TextError::OverCap)));
        } else {
            prop_assert_eq!(got, Ok(Ok(t)));
        }
    }

    #[test]
    fn any_input_is_safe_and_walking_agrees_with_skipping(
        parts in proptest::collection::vec(fragment(), 0..40),
        bytes in proptest::collection::vec(any::<u8>(), 0..64),
        pick in any::<u8>(),
    ) {
        let lossy = String::from_utf8_lossy(&bytes).into_owned();
        let small = Limits {
            max_depth: 1 + usize::from(pick % 8),
            max_values: 1 + usize::from(pick) * 4,
            max_array: usize::from(pick % 6),
            max_text: usize::from(pick) * 4,
        };
        for s in [parts.concat(), lossy] {
            for l in [WIDE, small] {
                let skip = skip_all(&s, l);
                let walk = read_all(&s, l);
                prop_assert_eq!(skip.err(), walk.err(), "{:?}", s);
                if let Err(e) = skip {
                    prop_assert!(matches!(e, Stop::Malformed | Stop::Depth | Stop::Elements));
                }
            }
        }
    }

    #[test]
    fn decoding_never_grows(t in "[a-z\\\\u0-9dD8]{0,24}") {
        if let Some(d) = decode(&t) {
            prop_assert!(d.len() <= t.len());
        }
    }
}

#[test]
fn deep_input_stops_without_recursion() {
    for open in ["[", "{\"a\":"] {
        let s = open.repeat(1_000_000);
        assert_eq!(skip_all(&s, WIDE), Err(Stop::Depth));
        assert_eq!(read_all(&s, WIDE), Err(Stop::Depth));
    }
}

#[test]
fn a_lone_surrogate_is_scanned_but_not_decoded() {
    let s = r#"["\ud800", "\udc00x", "\ud83d\u0041"]"#;
    assert_eq!(skip_all(s, WIDE), Ok(false));
    let got = read_all(s, WIDE).unwrap();
    let bad = V::Str("<bad>".into());
    assert_eq!(got, V::Arr(vec![bad.clone(), bad.clone(), bad]));
}
