//! The golden corpus (§14 M3 exit criterion: ≥ 95% field accuracy).
//! See `tests/golden/README.md` for the format and how it's scored.
//!
//! `cargo test -p lodestar-release-parser --test golden -- --nocapture`
//! prints per-field accuracy and every miss.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use lodestar_release_parser::parse;

/// The M3 gate.
const MIN_ACCURACY: f64 = 0.95;

struct Case {
    file: String,
    line: usize,
    name: String,
    expected: BTreeMap<String, String>,
}

fn load() -> Vec<Case> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let mut files: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .collect();
    files.sort();
    let mut cases: Vec<Case> = Vec::new();
    for path in files {
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        for (n, line) in fs::read_to_string(&path).unwrap().lines().enumerate() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(field) = line.strip_prefix("  ") {
                let (k, v) = field
                    .split_once(": ")
                    .unwrap_or_else(|| panic!("{file}:{}: bad field line", n + 1));
                let case = cases.last_mut().expect("field before any name");
                let old = case.expected.insert(k.to_owned(), v.to_owned());
                assert!(old.is_none(), "{file}:{}: {k} given twice", n + 1);
            } else {
                cases.push(Case {
                    file: file.clone(),
                    line: n + 1,
                    name: line.to_owned(),
                    expected: BTreeMap::new(),
                });
            }
        }
    }
    cases
}

/// Text fields compare without case or punctuation: the parser keeps the
/// name's own punctuation, and `Mr. Robot` vs `Mr Robot` isn't a miss.
fn normalize(key: &str, value: &str) -> String {
    if !matches!(key, "title" | "alt" | "artist" | "album" | "author") {
        return value.to_owned();
    }
    let mut out = String::new();
    for c in value.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if c == '\'' || c == '’' {
            // `Grey's` and `Greys` read the same.
        } else if !out.ends_with(' ') && !out.is_empty() {
            out.push(' ');
        }
    }
    out.trim_end().to_owned()
}

#[test]
fn golden_corpus() {
    let cases = load();
    assert!(cases.len() >= 200, "the corpus shrank to {}", cases.len());
    let mut total = 0usize;
    let mut correct = 0usize;
    let mut per_field: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut misses = Vec::new();
    for case in &cases {
        assert!(
            !case.expected.is_empty(),
            "{}:{} has no fields",
            case.file,
            case.line
        );
        let parsed: BTreeMap<String, String> = parse(&case.name)
            .fields()
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect();
        let keys: std::collections::BTreeSet<&String> =
            case.expected.keys().chain(parsed.keys()).collect();
        for key in keys {
            let want = case.expected.get(key).map(|v| normalize(key, v));
            let got = parsed.get(key).map(|v| normalize(key, v));
            let ok = want == got;
            total += 1;
            correct += usize::from(ok);
            let f = per_field.entry(key.clone()).or_default();
            f.0 += usize::from(ok);
            f.1 += 1;
            if !ok {
                misses.push(format!(
                    "{}:{} {}\n    {key}: want {:?}, got {:?}",
                    case.file,
                    case.line,
                    case.name,
                    case.expected.get(key),
                    parsed.get(key)
                ));
            }
        }
    }
    let accuracy = correct as f64 / total as f64;
    println!(
        "{} names, {correct}/{total} fields correct: {:.2}%",
        cases.len(),
        accuracy * 100.0
    );
    for (k, (ok, n)) in &per_field {
        println!(
            "  {k:<10} {ok:>4}/{n:<4} {:>6.1}%",
            *ok as f64 * 100.0 / *n as f64
        );
    }
    for m in &misses {
        println!("{m}");
    }
    assert!(
        accuracy >= MIN_ACCURACY,
        "field accuracy {:.2}% is below {:.0}% ({} misses, run with --nocapture)",
        accuracy * 100.0,
        MIN_ACCURACY * 100.0,
        misses.len()
    );
}
