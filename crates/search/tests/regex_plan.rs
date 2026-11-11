//! RX-0's regex plans: golden plans for typical patterns, and the property
//! that makes a prefilter safe: every name a pattern matches has the grams
//! the plan requires, as the `name_trigram` analyzer makes them.

use std::collections::BTreeSet;

use lodestar_search::regex::{MAX_PLAN_NODES, Plan, parse};
use lodestar_search::schema::trigram_analyzer;
use proptest::prelude::*;
use regex_syntax::hir::{Class, Hir, HirKind};

/// The grams the index would hold for `name`.
fn name_grams(name: &str) -> BTreeSet<String> {
    let mut a = trigram_analyzer();
    let mut stream = a.token_stream(name);
    let mut out = BTreeSet::new();
    while let Some(t) = stream.next() {
        out.insert(t.text.clone());
    }
    out
}

fn admits(plan: &Plan, name: &str) -> bool {
    let grams = name_grams(name);
    plan.admits(&mut |g| grams.contains(g))
}

const GOLDEN: &[(&str, &str)] = &[
    // Exact literals: every gram, in order of the sort.
    ("(?-i)matrix", r#""atr" & "mat" & "rix" & "tri""#),
    ("1080p", r#""080" & "108" & "80p""#),
    // Case-insensitive by default: s and k fold to more than one
    // lowercase form (ſ, the Kelvin sign lowercases to k).
    ("sk", "ALL"),
    ("ska", r#""ska" | "ſka""#),
    // Episode tags: the classes stay exact while small. Grams are
    // lowercase even when the pattern is case-sensitive.
    (
        r"(?-i)S0[1-3]E\d+",
        r#"("01e" & "s01") | ("02e" & "s02") | ("03e" & "s03")"#,
    ),
    // Large classes are "any character".
    (r"(?-i)x26\d", r#""x26""#),
    (r"(?-i)\d{3,4}p", "ALL"),
    (
        r"(?-i)(720|1080)p",
        r#"("080" & "108" & "80p") | ("20p" & "720")"#,
    ),
    // Alternation of whole words.
    (
        r"(?-i)\b(bluray|remux)\b",
        r#"("blu" & "lur" & "ray" & "ura") | ("emu" & "mux" & "rem")"#,
    ),
    // Wildcards in the middle keep both sides.
    (r"(?-i)dune.*part", r#""art" & "dun" & "par" & "une""#),
    // Anchors match the empty string.
    (r"(?-i)^the\s", r#""the""#),
    // Optional parts.
    (
        r"(?-i)colou?r",
        r#"("col" & "lor" & "olo") | ("col" & "lou" & "olo" & "our")"#,
    ),
    // Nothing to use: verify only.
    (r".", "ALL"),
    (r"\w+", "ALL"),
    (r"a|bcd", "ALL"),
    // Matches nothing at all.
    (r"[^\s\S]", "NONE"),
    // Non-Latin text folds per character, as the analyzer does.
    ("(?-i)千と千尋", r#""と千尋" & "千と千""#),
    ("(?-i)ΣΟΦ", r#""σοφ""#),
];

#[test]
fn golden_plans() {
    let mut wrong = Vec::new();
    for (pattern, want) in GOLDEN {
        let got = parse(pattern).expect(pattern).plan().to_string();
        if got != *want {
            wrong.push(format!("{pattern:?}\n  want {want}\n  got  {got}"));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

#[test]
fn golden_plans_admit_their_examples() {
    for (pattern, name) in [
        (r"S0[1-9]E\d+", "Show.Name.s02e11.1080p"),
        (r"S0[1-9]E\d+", "Show.Name.ſ02E11"),
        ("ska", "ſ\u{212A}A"),
        ("ska", "SKA"),
        ("(?-i)ΣΟΦ", "ΣΟΦΙΑ"),
        (r"\b(bluray|remux)\b", "Film.2010.BluRay.x264"),
        (r"colou?r", "Colour.Out.Of.Space"),
    ] {
        let p = parse(pattern).unwrap();
        assert!(
            admits(p.plan(), name),
            "{pattern:?} on {name:?}: {}",
            p.plan()
        );
    }
    let p = parse(r"(?-i)dune.*part").unwrap();
    assert!(!admits(p.plan(), "Dune Two"));
    // The prefilter is case-blind; the verifier rejects this one.
    assert!(admits(p.plan(), "Dune Part Two"));
}

/// A small deterministic generator, so a proptest case is just a seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

/// Appends a random string `h` matches (look-arounds are skipped: the
/// planner treats them as empty, and a string without them is still one
/// the unanchored pattern matches). `None` if `h` matches nothing.
fn sample(h: &Hir, rng: &mut Rng, out: &mut String) -> Option<()> {
    match h.kind() {
        HirKind::Empty | HirKind::Look(_) => {}
        HirKind::Literal(l) => out.push_str(std::str::from_utf8(&l.0).ok()?),
        HirKind::Class(Class::Unicode(c)) => {
            let ranges = c.ranges();
            if ranges.is_empty() {
                return None;
            }
            let r = ranges[rng.below(ranges.len() as u64) as usize];
            let span = u64::from(r.end()) - u64::from(r.start()) + 1;
            let mut v = u32::from(r.start()) + rng.below(span) as u32;
            // Ranges never span the surrogates, but step off them anyway.
            while char::from_u32(v).is_none() {
                v += 1;
            }
            out.push(char::from_u32(v)?);
        }
        HirKind::Class(Class::Bytes(_)) => return None,
        HirKind::Repetition(r) => {
            let extra = match r.max {
                Some(m) => u64::from(m - r.min).min(3),
                None => 3,
            };
            let n = u64::from(r.min) + rng.below(extra + 1);
            for i in 0..n {
                let mark = out.len();
                if sample(&r.sub, rng, out).is_none() {
                    // Fewer copies may still do (`[^\s\S]*` matches "").
                    if i < u64::from(r.min) {
                        return None;
                    }
                    out.truncate(mark);
                    break;
                }
            }
        }
        HirKind::Capture(c) => sample(&c.sub, rng, out)?,
        HirKind::Concat(hs) => {
            for h in hs {
                sample(h, rng, out)?;
            }
        }
        HirKind::Alternation(hs) => {
            // Try a random branch first, then the others (one may match
            // nothing).
            let start = rng.below(hs.len() as u64) as usize;
            let mark = out.len();
            for i in 0..hs.len() {
                out.truncate(mark);
                if sample(&hs[(start + i) % hs.len()], rng, out).is_some() {
                    return Some(());
                }
            }
            return None;
        }
    }
    Some(())
}

const PIECES: &[&str] = &[
    "a", "b", "c", "ab", "abc", "the", "S", "s", "k", "K", "ſ", "\u{212A}", "ß", "İ", "Σ", "σ",
    "ς", "千", "と", "0", "1", "e", "E", ".", r"\d", r"\w", r"\s", r"\b", "^", "$", "[a-c]",
    "[Ss]", "[^a]", "[0-9]", "[xyz]", r"\pL", "(?-i)", "(?i)", "(", ")", "(?:", "|", "?", "*", "+",
    "{2}", "{0,1}", "{1,3}", "{3,}", "{5}", "{2,6}",
];

fn pattern() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(PIECES), 1..14).prop_map(|v| v.concat())
}

fn affix() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop::sample::select(&["", "x", "S", "ſ", "Ab", "千", ".", " "][..]),
        0..4,
    )
    .prop_map(|v| v.concat())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    /// Plan soundness: any string the pattern matches, inside any name,
    /// passes the plan.
    #[test]
    fn every_match_passes_the_plan(p in pattern(), seed in 1u64.., before in affix(), after in affix()) {
        let Ok(parsed) = parse(&p) else { return Ok(()) };
        prop_assert!(parsed.plan().size() <= MAX_PLAN_NODES);
        let hir = regex_syntax::parse(&parsed.verify_pattern()).expect("it parsed once");
        let mut rng = Rng(seed);
        for _ in 0..8 {
            let mut m = before.clone();
            if sample(&hir, &mut rng, &mut m).is_none() {
                prop_assert_eq!(parsed.plan(), &Plan::Nothing);
                break;
            }
            m.push_str(&after);
            prop_assert!(admits(parsed.plan(), &m), "{:?} matches {:?} but the plan is {}", p, m, parsed.plan());
        }
    }

    /// Arbitrary text never panics, and anything accepted has a capped plan.
    #[test]
    fn arbitrary_text_is_bounded(p in ".{0,80}") {
        if let Ok(parsed) = parse(&p) {
            prop_assert!(parsed.plan().size() <= MAX_PLAN_NODES);
        }
    }
}

/// Patterns at the limits, built to make sets and plans as large as they
/// can be: each parses within the caps.
#[test]
fn worst_cases_stay_within_the_caps() {
    let alts = |item: &str| vec![item; 32].join("|");
    let cases = [
        "s".repeat(250),
        "[sk]".repeat(60),
        "(s|k|ſ)".repeat(30),
        format!("({})", alts("sksksk")),
        format!("({}){{4}}", alts("[a-p]")),
        format!("(({}){{10}}){{100}}", alts("[a-p]s")),
        r"(\w|\pL|\d){100}".repeat(3),
        "(((((ab)+c)+d)+e)+f)".to_owned(),
    ];
    for p in &cases {
        let parsed = parse(p).unwrap_or_else(|e| panic!("{p:?}: {e}"));
        assert!(parsed.plan().size() <= MAX_PLAN_NODES, "{p:?}");
    }
}
