//! An adult-enrichment match as a strong adult signal (§20.9, plan ENR-6
//! and Q13): what it pins, what still wins over it, and that it never
//! weakens anything filtering relies on.

use lodestar_classify::{
    AdultKind, Classification, ContentType, Correction, EnrichmentMatch, FileInfo,
    MATCH_CONFIDENCE, MatchKind, classify, classify_corrected, classify_enriched,
};
use proptest::prelude::*;

fn file(path: &str, size: u64) -> FileInfo {
    FileInfo::new(path.split('/').map(str::to_owned).collect(), size)
}

const GIB: u64 = 1 << 30;

fn matched(kind: MatchKind) -> Option<EnrichmentMatch> {
    Some(EnrichmentMatch::new(kind))
}

const NONE: Correction = Correction {
    adult: None,
    not_junk: false,
};
const IS_ADULT: Correction = Correction {
    adult: Some(true),
    not_junk: false,
};
const NOT_ADULT: Correction = Correction {
    adult: Some(false),
    not_junk: false,
};

/// Everything a match must leave alone.
fn same_apart_from_adult(a: &Classification, b: &Classification) {
    assert_eq!(a.composition, b.composition);
    assert_eq!(a.structure, b.structure);
    assert_eq!(a.title, b.title);
    assert_eq!(a.junk, b.junk);
    assert_eq!(a.largest, b.largest);
    assert_eq!(a.depth, b.depth);
    assert_eq!(a.distinct_extensions, b.distinct_extensions);
}

#[test]
fn scene_match() {
    // A site scene detection already calls adult: the match pins Scene.
    let name = "Brazzers.24.01.15.Some.Title.1080p";
    let files = [file("Brazzers.24.01.15.Some.Title.1080p.mp4", GIB)];
    let before = classify(name, &files);
    assert_eq!(before.content_type, ContentType::Adult);
    let c = classify_enriched(name, &files, NONE, matched(MatchKind::Scene));
    assert_eq!(c.content_type, ContentType::Adult);
    assert_eq!(c.adult_kind, Some(AdultKind::Scene));
    assert_eq!(c.confidence, MATCH_CONFIDENCE);
    assert!(c.confidence >= before.confidence);
    same_apart_from_adult(&c, &before);
}

#[test]
fn movie_match() {
    // A name detection doesn't call adult at all: the match makes it so.
    let name = "Lantern.Bay.2019.1080p.WEB-DL";
    let files = [file("Lantern.Bay.2019.1080p.WEB-DL.mkv", 4 * GIB)];
    let before = classify(name, &files);
    assert_eq!(before.content_type, ContentType::Movie);
    let c = classify_enriched(name, &files, NONE, matched(MatchKind::Movie));
    assert_eq!(c.content_type, ContentType::Adult);
    assert_eq!(c.adult_kind, Some(AdultKind::Movie));
    assert_eq!(c.confidence, MATCH_CONFIDENCE);
    same_apart_from_adult(&c, &before);
    // Detection's Scene (scene naming) becomes the reported Movie.
    let name = "Somesite.24.01.15.Porn.Title";
    let files = [file("Somesite.24.01.15.Porn.Title.mp4", GIB)];
    assert_eq!(classify(name, &files).adult_kind, Some(AdultKind::Scene));
    let c = classify_enriched(name, &files, NONE, matched(MatchKind::Movie));
    assert_eq!(c.adult_kind, Some(AdultKind::Movie));
}

#[test]
fn vr_match() {
    let name = "Some.Title.XXX.1080p";
    let files = [file("Some.Title.XXX.1080p.mp4", GIB)];
    assert_eq!(classify(name, &files).adult_kind, Some(AdultKind::Movie));
    let c = classify_enriched(name, &files, NONE, matched(MatchKind::Vr));
    assert_eq!(c.adult_kind, Some(AdultKind::Vr));
    assert_eq!(c.confidence, MATCH_CONFIDENCE);
    // Detection's VR is more specific than a Scene or Movie match.
    let name = "Some.Title.XXX.VR180.LR";
    let files = [file("Some.Title.XXX.VR180.LR.mp4", 6 * GIB)];
    assert_eq!(classify(name, &files).adult_kind, Some(AdultKind::Vr));
    for k in MatchKind::ALL {
        let c = classify_enriched(name, &files, NONE, matched(k));
        assert_eq!(c.adult_kind, Some(AdultKind::Vr), "{k:?}");
    }
}

#[test]
fn not_adult_correction_or_rule_wins_over_a_match() {
    // A Not adult correction and a Not adult classification rule both
    // reach the classifier as `Correction { adult: Some(false) }`.
    let name = "Some.Title.XXX.1080p";
    let files = [file("Some.Title.XXX.1080p.mp4", GIB)];
    for k in MatchKind::ALL {
        let c = classify_enriched(name, &files, NOT_ADULT, matched(k));
        assert_ne!(c.content_type, ContentType::Adult);
        assert_eq!(c.adult_kind, None);
        assert_eq!(c, classify_corrected(name, &files, NOT_ADULT));
        // Filtering judges detection's own result, so the item is still
        // adult to the Adult-only core checks (§20.7).
        let judged = classify_enriched(name, &files, NONE, matched(k));
        assert_eq!(judged.content_type, ContentType::Adult);
    }
}

#[test]
fn is_adult_correction_or_rule_keeps_the_matched_kind() {
    // Is adult decides the outcome, not the kind: with a match, the kind
    // is the reported one (as it's detection's without one), at the
    // correction's full confidence.
    let name = "The.Matrix.1999.1080p.BluRay.x264-GRP";
    let files = [file("The.Matrix.1999.1080p.BluRay.x264-GRP.mkv", 8 * GIB)];
    let forced = classify_corrected(name, &files, IS_ADULT);
    assert_eq!(forced.adult_kind, Some(AdultKind::Movie));
    assert_eq!(forced.confidence, 100);
    for k in MatchKind::ALL {
        let c = classify_enriched(name, &files, IS_ADULT, matched(k));
        assert_eq!(c.content_type, ContentType::Adult);
        assert_eq!(c.adult_kind, Some(k.adult_kind()));
        assert_eq!(c.confidence, 100);
    }
    // The same where detection had already called it adult.
    let name = "Some.Title.XXX.1080p";
    let files = [file("Some.Title.XXX.1080p.mp4", GIB)];
    assert_eq!(classify_corrected(name, &files, IS_ADULT).confidence, 90);
    let c = classify_enriched(name, &files, IS_ADULT, matched(MatchKind::Movie));
    assert_eq!((c.adult_kind, c.confidence), (Some(AdultKind::Movie), 100));
}

#[test]
fn not_junk_still_applies_with_a_match() {
    let name = "Movie.2024.1080p";
    let files = [
        file("Movie.2024.1080p.mkv", 2 * GIB),
        file("password.txt", 1024),
        file("setup.exe", 3 << 20),
    ];
    let fix = Correction {
        adult: None,
        not_junk: true,
    };
    assert!(
        classify_enriched(name, &files, NONE, matched(MatchKind::Scene))
            .junk
            .score
            > 0
    );
    let c = classify_enriched(name, &files, fix, matched(MatchKind::Scene));
    assert_eq!(c.junk.score, 0);
    assert_eq!(c.content_type, ContentType::Adult);
}

#[test]
fn a_match_ignores_counter_signals() {
    // An item looked up because it was corrected Is adult, with a
    // documentary word, judged as filtering judges it: still adult.
    let name = "XXX.Industry.Documentary.2019";
    let files = [file("XXX.Industry.Documentary.2019.mkv", 2 * GIB)];
    assert_ne!(classify(name, &files).content_type, ContentType::Adult);
    let c = classify_enriched(name, &files, NONE, matched(MatchKind::Movie));
    assert_eq!(c.content_type, ContentType::Adult);
}

#[test]
fn no_files_or_padding_only() {
    for files in [
        vec![],
        vec![FileInfo {
            padding: true,
            ..file("pad", 10)
        }],
    ] {
        let c = classify_enriched("x", &files, NONE, matched(MatchKind::Scene));
        assert_eq!(c.content_type, ContentType::Adult);
        assert_eq!(c.adult_kind, Some(AdultKind::Scene));
        assert_eq!(c.confidence, MATCH_CONFIDENCE);
        same_apart_from_adult(&c, &classify("x", &files));
    }
}

fn segment() -> impl Strategy<Value = String> {
    prop_oneof![
        "[A-Za-z0-9 ._\\-\\[\\]()]{0,24}",
        "\\PC{0,12}",
        Just("Some.Title.XXX.1080p.mp4".to_owned()),
        Just("Brazzers.24.01.15.Title.mp4".to_owned()),
        Just("Scene.VR180.LR.mp4".to_owned()),
        Just("Documentary.2019.mkv".to_owned()),
        Just("Show.S01E02.1080p.mkv".to_owned()),
        Just("cover.jpg".to_owned()),
        Just("book.epub".to_owned()),
        Just("setup.exe".to_owned()),
        Just("BDMV".to_owned()),
    ]
}

fn name() -> impl Strategy<Value = String> {
    prop_oneof![
        "\\PC{0,40}",
        Just("Some.Title.XXX.1080p".to_owned()),
        Just("xXx.2002.1080p.BluRay".to_owned()),
        Just("Porn.Milf.Documentary.2019".to_owned()),
        Just("Anime [Hentai]".to_owned()),
        Just("Sex.Education.S01.1080p".to_owned()),
        Just("Lantern.Bay.Parody.XXX.VR180.LR".to_owned()),
    ]
}

fn files() -> impl Strategy<Value = Vec<FileInfo>> {
    proptest::collection::vec(
        (
            proptest::collection::vec(segment(), 1..4),
            any::<u64>(),
            any::<bool>(),
        )
            .prop_map(|(path, size, padding)| FileInfo {
                path,
                size,
                padding,
            }),
        0..12,
    )
}

fn correction() -> impl Strategy<Value = Correction> {
    (
        prop_oneof![Just(None), Just(Some(true)), Just(Some(false))],
        any::<bool>(),
    )
        .prop_map(|(adult, not_junk)| Correction { adult, not_junk })
}

fn kind() -> impl Strategy<Value = MatchKind> {
    prop_oneof![
        Just(MatchKind::Scene),
        Just(MatchKind::Movie),
        Just(MatchKind::Vr)
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// No match: the new entry point is the old one, for every correction.
    /// This pins the API delegation; that unmatched detection itself is
    /// unchanged is held by the golden corpus (`fixtures.rs`) and by
    /// `adult.rs`'s frozen-oracle property test.
    #[test]
    fn no_match_is_unchanged(name in name(), files in files(), fix in correction()) {
        prop_assert_eq!(classify_enriched(&name, &files, fix, None), classify_corrected(&name, &files, fix));
        prop_assert_eq!(classify_enriched(&name, &files, NONE, None), classify(&name, &files));
    }

    /// A match only adds: adult, no less surely, as the reported kind (or
    /// VR), with every other field unchanged; and Not adult still wins.
    #[test]
    fn a_match_never_weakens(name in name(), files in files(), fix in correction(), k in kind()) {
        let before = classify_corrected(&name, &files, fix);
        let c = classify_enriched(&name, &files, fix, matched(k));
        let other = Classification {
            content_type: before.content_type,
            confidence: before.confidence,
            adult_kind: before.adult_kind,
            ..c.clone()
        };
        prop_assert_eq!(&other, &before);
        if fix.adult == Some(false) {
            prop_assert_eq!(&c, &before);
        } else {
            prop_assert_eq!(c.content_type, ContentType::Adult);
            prop_assert!(c.adult_kind == Some(k.adult_kind()) || c.adult_kind == Some(AdultKind::Vr));
            let want = if fix.adult == Some(true) { 100 } else { MATCH_CONFIDENCE };
            prop_assert_eq!(c.confidence, want);
            prop_assert!(c.confidence >= before.confidence);
            if before.adult_kind == Some(AdultKind::Vr) && k != MatchKind::Vr {
                prop_assert_eq!(c.adult_kind, Some(AdultKind::Vr));
            }
        }
        // What filtering judges (no correction) is adult whatever the fix.
        prop_assert_eq!(
            classify_enriched(&name, &files, NONE, matched(k)).content_type,
            ContentType::Adult
        );
    }
}
