//! Property tests: never panics, never exceeds a cap, uses an item whole
//! or not at all, and reads back what a well-formed answer says. The
//! last test runs the fuzz target's checks over a long mutated corpus,
//! for machines without cargo-fuzz.

use lodestar_metainfo::{InfoHash, hidden_char};
use lodestar_sources_feeds::archive_org::{
    DETAILS, Feed, LIMITS, Limits, MAX_COLLECTIONS, MAX_CURSOR, MAX_IDENTIFIER, MAX_PATH, MAX_REF,
    MAX_TITLE, parse, parse_each,
};
use proptest::prelude::*;

fn with(body: &[u8], limits: &Limits) -> Feed {
    let mut items = Vec::new();
    let page = parse_each(body, limits, |i| items.push(i));
    Feed { items, page }
}

/// What holds for any input: `fuzz/fuzz_targets/archive_org.rs` checks
/// the same.
fn check(f: &Feed, limits: &Limits) {
    let s = &f.page.summary;
    assert_eq!(s.kept as usize, f.items.len());
    assert!((s.kept + s.skipped.total()) as usize <= limits.max_items);
    assert_eq!(s.error, None);
    assert!(f.page.cursor.as_ref().is_none_or(|c| c.len() <= MAX_CURSOR));
    for it in &f.items {
        assert!(it.hashes.v1.is_some() && it.hashes.v2.is_none());
        assert!(
            it.title
                .as_ref()
                .is_none_or(|t| !t.is_empty() && t.len() <= MAX_TITLE)
        );
        assert!(!it.title.iter().flat_map(|t| t.chars()).any(hidden_char));
        assert!(
            it.identifier
                .as_ref()
                .is_none_or(|i| i.len() <= MAX_IDENTIFIER)
        );
        assert_eq!(
            it.link,
            it.identifier.as_ref().map(|i| format!("{DETAILS}{i}"))
        );
        assert!(it.link.as_ref().is_none_or(|l| l.len() <= MAX_REF));
        assert!(it.collections.len() <= MAX_COLLECTIONS);
        assert!(it.files.len() <= limits.max_files);
        let names: usize = it.files.iter().map(|f| f.path.len()).sum();
        assert!(names <= limits.max_name_bytes);
        for f in &it.files {
            assert!(!f.path.is_empty() && f.path.len() <= MAX_PATH);
            assert!(!f.path.chars().any(hidden_char));
        }
    }
}

/// Tight caps picked by one byte, as the fuzz target does.
fn small(pick: u8) -> Limits {
    Limits {
        max_bytes: 1 + usize::from(pick) * 64,
        max_depth: 1 + usize::from(pick % 8),
        max_values: 1 + usize::from(pick) * 4,
        max_items: 1 + usize::from(pick % 4),
        max_array: usize::from(pick % 6),
        max_text: usize::from(pick) * 4,
        max_files: usize::from(pick % 5),
        max_name_bytes: usize::from(pick) * 2,
    }
}

/// A JSON escape of `c`: a backslash, `u` and four hex digits a unit.
fn escape(c: char, out: &mut String) {
    let mut units = [0u16; 2];
    for u in c.encode_utf16(&mut units) {
        out.push_str(&format!("{}u{:04x}", '\\', u));
    }
}

/// `s` as a JSON string, non-ASCII escaped when `esc` says so.
fn json(s: &str, esc: bool) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if c < ' ' || (esc && !c.is_ascii()) => escape(c, &mut out),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Pieces of IA-shaped JSON, well-formed or not.
fn fragment() -> impl Strategy<Value = String> {
    let h = "0123456789abcdef0123456789abcdef01234567";
    prop_oneof![
        Just("{".to_string()),
        Just("}".to_string()),
        Just("[".to_string()),
        Just("]".to_string()),
        Just(",".to_string()),
        Just(":".to_string()),
        Just(r#""docs":["#.to_string()),
        Just(r#""files":["#.to_string()),
        Just(r#""metadata":{"#.to_string()),
        Just(r#""identifier":"item","#.to_string()),
        Just(format!(r#""btih":"{h}","#)),
        Just(format!(
            r#"{{"name":"item_archive.torrent","btih":"{h}"}},"#
        )),
        Just(r#"{"name":"a/b.mp4","size":"12"},"#.to_string()),
        Just(r#""title":["a","b"],"#.to_string()),
        Just(r#""collection":"c","#.to_string()),
        Just(r#""item_size":1e9,"#.to_string()),
        Just(r#""publicdate":"2026-11-01 00:00:00","#.to_string()),
        Just(r#""cursor":"abc=","#.to_string()),
        Just(format!(r#""title":"{}d800","#, '\\')),
        Just(format!(r#""title":"{}u202e{}u200b","#, '\\', '\\')),
        "[a-z{}\\[\\]\":,0-9\\\\ \u{202E}\u{200B}\u{FEFF}\u{2028}]{0,12}",
    ]
}

proptest! {
    #[test]
    fn any_bytes_are_safe(body in proptest::collection::vec(any::<u8>(), 0..512)) {
        check(&parse(&body), &LIMITS);
    }

    #[test]
    fn any_fragments_are_safe(
        parts in proptest::collection::vec(fragment(), 0..60),
        pick in any::<u8>(),
    ) {
        for root in [r#"{"response":{"docs":["#, r#"{"files":["#, r#"{"items":["#, ""] {
            let body = format!("{root}{}", parts.concat());
            check(&parse(body.as_bytes()), &LIMITS);
            let limits = small(pick);
            check(&with(body.as_bytes(), &limits), &limits);
        }
    }

    #[test]
    fn docs_round_trip(
        docs in proptest::collection::vec(
            (
                "[A-Za-z0-9._-]{1,100}",
                "[a-zA-Z0-9 é—😀\"\\\\/\u{1}]{0,40}",
                any::<[u8; 20]>().prop_filter("not zero", |h| *h != [0; 20]),
                any::<u64>(),
                any::<(bool, bool, bool)>(),
            ),
            0..20,
        ),
    ) {
        let body: Vec<String> = docs
            .iter()
            .map(|(id, title, h, size, (esc, upper, size_str))| {
                let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
                let hex = if *upper { hex.to_uppercase() } else { hex };
                let size = if *size_str { format!("\"{size}\"") } else { size.to_string() };
                format!(
                    r#"{{"identifier":{},"title":{},"btih":"{hex}","item_size":{size},"x":[{{}},[]]}}"#,
                    json(id, *esc),
                    json(title, *esc),
                )
            })
            .collect();
        let body = format!(r#"{{"response":{{"docs":[{}]}}}}"#, body.join(","));
        let f = parse(body.as_bytes());
        prop_assert_eq!(f.page.summary.stop, None);
        prop_assert_eq!(f.items.len(), docs.len());
        for (it, (id, title, h, size, _)) in f.items.iter().zip(&docs) {
            prop_assert_eq!(it.hashes.v1, Some(InfoHash(*h)));
            prop_assert_eq!(it.identifier.as_ref(), Some(id));
            let want: String = title.trim().chars().filter(|&c| !hidden_char(c)).collect();
            prop_assert_eq!(it.title.clone(), Some(want).filter(|t| !t.is_empty()));
            prop_assert_eq!(it.size, Some(*size));
        }
    }
}

/// xorshift64*: a fixed, dependency-free source of mutations.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// The fuzz target's checks over 200,000 inputs: random bytes, and the
/// seeds with bytes flipped, inserted, deleted, duplicated and spliced.
#[test]
fn mutated_corpus() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/archive_org");
    let mut seeds: Vec<Vec<u8>> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| std::fs::read(e.unwrap().path()).unwrap())
        .collect();
    seeds.sort();
    let tokens: [&[u8]; 12] = [
        b"{",
        b"}",
        b"[",
        b"]",
        b",",
        b"\"",
        b"\\",
        b":",
        b"\"btih\"",
        b".torrent",
        b"\xff",
        b"1e999",
    ];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (mut kept, mut files) = (0, 0);
    for n in 0..200_000 {
        let mut input = if n % 10 == 0 {
            (0..rng.below(256)).map(|_| rng.next() as u8).collect()
        } else {
            seeds[rng.below(seeds.len())].clone()
        };
        for _ in 0..1 + rng.below(8) {
            let at = rng.below(input.len() + 1);
            match rng.below(6) {
                0 if at < input.len() => input[at] ^= 1 << rng.below(8),
                1 => input.insert(at, rng.next() as u8),
                2 if at < input.len() => {
                    input.remove(at);
                }
                3 => {
                    let t = tokens[rng.below(tokens.len())];
                    input.splice(at..at, t.iter().copied());
                }
                4 => {
                    let end = (at + rng.below(64)).min(input.len());
                    let chunk = input[at..end].to_vec();
                    input.splice(at..at, chunk);
                }
                _ => {
                    let other = &seeds[rng.below(seeds.len())];
                    let from = rng.below(other.len());
                    input.truncate(at);
                    input.extend_from_slice(&other[from..]);
                }
            }
        }
        let pick = rng.next() as u8;
        let f = parse(&input);
        check(&f, &LIMITS);
        kept += f.items.len();
        files += f.items.iter().map(|i| i.files.len()).sum::<usize>();
        let limits = small(pick);
        check(&with(&input, &limits), &limits);
    }
    // The mutations leave many answers whole enough to reach the items.
    eprintln!("kept {kept} items with {files} files");
    assert!(kept > 30_000 && files > 15_000, "{kept} {files}");
}
