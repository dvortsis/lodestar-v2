//! Parser invariants on arbitrary input (the `release` fuzz target checks
//! the same on raw bytes).

use lodestar_release_parser::{MAX_INPUT_BYTES, is_uninformative, parse};
use proptest::prelude::*;

/// `small`'s letters and digits appear in `big`, in order.
fn alnum_subsequence(small: &str, big: &str) -> bool {
    let mut big = big
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphanumeric());
    small
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphanumeric())
        .all(|c| big.any(|b| b == c))
}

fn names() -> impl Strategy<Value = String> {
    prop_oneof![
        "\\PC{0,120}",
        "[A-Za-z0-9 ._+\\-\\[\\]()]{0,120}",
        // Release-shaped: words and markers joined by dots and dashes.
        proptest::collection::vec(
            prop_oneof![
                "[A-Z][a-z]{1,8}",
                Just("1080p".to_owned()),
                Just("S01E02".to_owned()),
                Just("2019".to_owned()),
                Just("WEB-DL".to_owned()),
                Just("DDP5.1".to_owned()),
                Just("x264-GRP".to_owned()),
                Just("[Group]".to_owned()),
                Just(" - 05".to_owned()),
                Just("(2160p)".to_owned()),
                Just("v1.2.3".to_owned()),
                Just("FRENCH".to_owned()),
                "[0-9]{1,4}",
            ],
            0..12
        )
        .prop_map(|w| w.join(".")),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    #[test]
    fn well_formed(name in names()) {
        let r = parse(&name);
        prop_assert_eq!(&r, &parse(&name), "deterministic");
        // Text fields come from the name.
        for text in [Some(&r.title), r.group.as_ref(), r.artist.as_ref(), r.album.as_ref(), r.author.as_ref()]
            .into_iter()
            .flatten()
            .chain(&r.alt_titles)
        {
            prop_assert!(alnum_subsequence(text, &name), "{text:?} not from {name:?}");
            prop_assert!(text.len() <= name.len() + 8);
            prop_assert_eq!(text.trim(), text.as_str());
        }
        for g in [&r.group, &r.version].into_iter().flatten() {
            prop_assert!(name.contains(g.as_str()), "{g:?} not in {name:?}");
        }
        for s in [r.year, r.season, r.episode, r.absolute].into_iter().flatten() {
            prop_assert!(s.first <= s.last);
        }
        if let Some(d) = r.date {
            prop_assert!((1..=12).contains(&d.month) && (1..=31).contains(&d.day));
        }
        for (k, v) in r.fields() {
            prop_assert!(!v.is_empty(), "{k} rendered empty");
        }
        let _ = is_uninformative(&name);
    }

    /// Parsing is bounded: a long name parses like its first
    /// `MAX_INPUT_BYTES`.
    #[test]
    fn long_input_is_truncated(head in "[A-Za-z. ]{0,40}", fill in "[a-z.]{600,800}") {
        let name = format!("{head}{fill}");
        let r = parse(&name);
        prop_assert!(r.title.len() <= MAX_INPUT_BYTES);
    }
}

#[test]
fn speed() {
    // Not a gate (the bench crate owns those); a sanity check that a
    // parse costs microseconds, since it runs on every name and file.
    let names = [
        "Dune.Part.Two.2024.2160p.AMZN.WEB-DL.DDP5.1.Atmos.DV.HDR10.H.265-FLUX",
        "[SubsPlease] Frieren - 01 (1080p) [A1B2C3D4].mkv",
        "Pink Floyd - The Dark Side of the Moon (1973) [FLAC]",
        "Adobe Photoshop 2024 v25.0.0.37 (x64) Multilingual",
    ];
    let n = 20_000;
    let start = std::time::Instant::now();
    for i in 0..n {
        std::hint::black_box(parse(names[i % names.len()]));
    }
    let per = start.elapsed() / n as u32;
    println!("{per:?} per parse");
    assert!(per.as_micros() < 200, "{per:?} per parse");
}
