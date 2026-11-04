//! Property tests for the full-scrape parser (`full.rs`): it never panics,
//! never passes its caps, and the way a body is split into chunks never
//! changes what it finds.

use lodestar_metainfo::InfoHash;
use lodestar_tracker_scrape::Error;
use lodestar_tracker_scrape::full::{LIMITS, Limits, MAX_BATCH, Parser, Summary, parse};
use proptest::prelude::*;

/// One generated entry: its key, and whether its value is a dictionary
/// within the entry cap (the parser must then emit it).
#[derive(Debug, Clone)]
struct Gen {
    key: Vec<u8>,
    value: Vec<u8>,
    good: bool,
}

fn int() -> impl Strategy<Value = Vec<u8>> {
    any::<i64>().prop_map(|n| format!("i{n}e").into_bytes())
}

fn string() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..40).prop_map(|s| {
        let mut v = format!("{}:", s.len()).into_bytes();
        v.extend_from_slice(&s);
        v
    })
}

/// A bencoded value nested up to `depth`, with sorted-or-not keys.
fn value(depth: u32) -> BoxedStrategy<Vec<u8>> {
    let leaf = prop_oneof![int(), string()];
    leaf.prop_recursive(depth, 32, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(|items| {
                let mut v = b"l".to_vec();
                items.iter().for_each(|i| v.extend_from_slice(i));
                v.push(b'e');
                v
            }),
            proptest::collection::vec((string(), inner), 0..4).prop_map(|items| {
                let mut v = b"d".to_vec();
                for (k, i) in items {
                    v.extend_from_slice(&k);
                    v.extend_from_slice(&i);
                }
                v.push(b'e');
                v
            }),
        ]
    })
    .boxed()
}

fn entry() -> impl Strategy<Value = Gen> {
    let key = prop_oneof![
        8 => proptest::collection::vec(any::<u8>(), 20..=20),
        1 => proptest::collection::vec(any::<u8>(), 0..40),
    ];
    (key, value(2)).prop_map(|(key, value)| {
        let good = key.len() == 20
            && value.first() == Some(&b'd')
            && value.len() as u64 <= LIMITS.max_entry_bytes
            && depth(&value) + 2 <= LIMITS.max_depth;
        Gen { key, value, good }
    })
}

/// The deepest nesting in a well-formed value (1 for a container of
/// scalars, 0 for a scalar).
fn depth(v: &[u8]) -> usize {
    let (mut d, mut max, mut i) = (0usize, 0usize, 0usize);
    while i < v.len() {
        match v[i] {
            b'd' | b'l' => {
                d += 1;
                max = max.max(d);
                i += 1;
            }
            b'e' => {
                d -= 1;
                i += 1;
            }
            b'i' => i = i + v[i..].iter().position(|b| *b == b'e').unwrap() + 1,
            _ => {
                let colon = i + v[i..].iter().position(|b| *b == b':').unwrap();
                let len: usize = std::str::from_utf8(&v[i..colon]).unwrap().parse().unwrap();
                i = colon + 1 + len;
            }
        }
    }
    max
}

fn reply(entries: &[Gen]) -> Vec<u8> {
    let mut v = b"d5:filesd".to_vec();
    for e in entries {
        v.extend_from_slice(format!("{}:", e.key.len()).as_bytes());
        v.extend_from_slice(&e.key);
        v.extend_from_slice(&e.value);
    }
    v.extend_from_slice(b"e5:flagsd20:min_request_intervali3600eee");
    v
}

/// Feeds `body` split at `cuts`, checking every batch against the caps.
fn run(body: &[u8], limits: Limits, cuts: &[usize]) -> (Vec<InfoHash>, Result<Summary, Error>) {
    let mut p = Parser::new(limits);
    let batch = p.limits().batch;
    let mut out = Vec::new();
    let mut emit = |b: &[InfoHash]| {
        assert!(!b.is_empty() && b.len() <= batch && batch <= MAX_BATCH);
        out.extend_from_slice(b);
    };
    let mut points: Vec<usize> = cuts.iter().map(|c| c % (body.len() + 1)).collect();
    points.sort_unstable();
    let mut start = 0;
    for p_at in points.into_iter().chain([body.len()]) {
        if let Err(e) = p.feed(&body[start..p_at], &mut emit) {
            assert!(p.summary().bytes <= limits.max_bytes);
            return (out, Err(e));
        }
        start = p_at;
    }
    assert!(p.summary().bytes <= limits.max_bytes);
    let r = p.finish(&mut emit);
    (out, r)
}

fn limits() -> impl Strategy<Value = Limits> {
    (1u64..4096, 0u64..64, 0u64..2048, 0usize..12, 0usize..16).prop_map(
        |(max_bytes, max_entries, max_entry_bytes, max_depth, batch)| Limits {
            max_bytes,
            max_entries,
            max_entry_bytes,
            max_depth,
            batch,
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Arbitrary bytes, arbitrary caps, arbitrary chunking: no panic, and
    /// never more keys than the entry cap or bytes than the byte cap.
    #[test]
    fn never_panics_or_passes_its_caps(
        body in proptest::collection::vec(any::<u8>(), 0..2048),
        limits in limits(),
        cuts in proptest::collection::vec(any::<usize>(), 0..8),
    ) {
        let (out, r) = run(&body, limits, &cuts);
        prop_assert!(out.len() as u64 <= limits.max_entries);
        if let Ok(s) = r {
            prop_assert_eq!(s.keys, out.len() as u64);
            prop_assert!(s.keys + s.malformed <= limits.max_entries);
            prop_assert!(s.bytes <= limits.max_bytes);
        }
    }

    /// Mutated valid replies, which get much further than random bytes.
    #[test]
    fn mutated_replies_never_panic(
        entries in proptest::collection::vec(entry(), 0..12),
        flips in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..6),
        limits in limits(),
        cuts in proptest::collection::vec(any::<usize>(), 0..8),
    ) {
        let mut body = reply(&entries);
        for (at, b) in flips {
            let n = body.len();
            body[at % n] = b;
        }
        let (out, _) = run(&body, limits, &cuts);
        prop_assert!(out.len() as u64 <= limits.max_entries);
    }

    /// A valid reply gives exactly its well-shaped keys, in order, and
    /// counts the rest, however it is chunked.
    #[test]
    fn valid_replies_give_their_keys(
        entries in proptest::collection::vec(entry(), 0..24),
        cuts in proptest::collection::vec(any::<usize>(), 0..16),
    ) {
        let body = reply(&entries);
        let (out, r) = run(&body, LIMITS, &cuts);
        let s = r.unwrap();
        let want: Vec<InfoHash> = entries
            .iter()
            .filter(|e| e.good)
            .map(|e| InfoHash(e.key.as_slice().try_into().unwrap()))
            .collect();
        prop_assert_eq!(&out, &want);
        prop_assert_eq!(s.keys, want.len() as u64);
        prop_assert_eq!(s.malformed, (entries.len() - want.len()) as u64);
        prop_assert_eq!(s.bytes, body.len() as u64);
    }

    /// Past the entry cap the parse fails, after emitting no more than
    /// the cap.
    #[test]
    fn the_entry_cap_holds(n in 1usize..40, cap in 0u64..40) {
        let entries: Vec<Gen> = (0..n)
            .map(|i| Gen { key: vec![i as u8; 20], value: b"de".to_vec(), good: true })
            .collect();
        let limits = Limits { max_entries: cap, batch: 3, ..LIMITS };
        let mut out = Vec::new();
        let r = parse(&reply(&entries), limits, 5, &mut |b| out.extend_from_slice(b));
        prop_assert!(out.len() as u64 <= cap);
        prop_assert_eq!(r.is_ok(), n as u64 <= cap);
    }
}
