//! Adult as a media category (§20.9), scored for precision: a torrent is
//! only called adult on a strong signal or several supporting ones, and
//! counter-signals win.
//!
//! - Strong: the scene `XXX` tag (never as the first word, which is the
//!   film *xXx*), a known adult site or studio, `hentai` (not
//!   `hentai-free`).
//! - Supporting: adult terms in the name, the `Site.YY.MM.DD.…` scene
//!   naming.
//! - Counter: documentary, educational and medical words cancel the
//!   supporting signals and weaken the strong ones; season and episode
//!   numbers (mainstream TV like *Sex Education*) cancel the supporting
//!   ones.
//!
//! - Enrichment: a match from an adult-enrichment service (ThePornDB,
//!   StashDB) is a strong signal that pins the kind the service reports
//!   where the torrent's own kind is video (a non-video kind is kept),
//!   at [`MATCH_CONFIDENCE`] ([`detect_matched`], plan Q13). It only ever
//!   adds: it makes a torrent adult, or more surely or more specifically
//!   so, and never less. A user correction or classification rule still
//!   wins over it (§20.7, V8/V9), since those are applied by the caller.
//!
//! The source's own category arrives with the filter engine and the
//! plugins; it adjusts this score rather than replace it.

use lodestar_release_parser::Release;

use crate::FileInfo;
use crate::content::ContentType;

/// What an adult torrent would otherwise be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AdultKind {
    /// Short video: clips, site scenes, episodic adult video.
    Scene,
    Movie,
    Photos,
    /// Comics, manga and hentai artwork.
    Comics,
    Game,
    Vr,
    Audio,
}

impl AdultKind {
    pub const ALL: [AdultKind; 7] = [
        AdultKind::Scene,
        AdultKind::Movie,
        AdultKind::Photos,
        AdultKind::Comics,
        AdultKind::Game,
        AdultKind::Vr,
        AdultKind::Audio,
    ];

    /// The name used by `adult:` queries and the stored record.
    pub fn as_str(self) -> &'static str {
        match self {
            AdultKind::Scene => "scene",
            AdultKind::Movie => "movie",
            AdultKind::Photos => "photos",
            AdultKind::Comics => "comics",
            AdultKind::Game => "game",
            AdultKind::Vr => "vr",
            AdultKind::Audio => "audio",
        }
    }

    pub fn from_name(name: &str) -> Option<AdultKind> {
        AdultKind::ALL.into_iter().find(|k| k.as_str() == name)
    }
}

/// The score a torrent needs to be called adult.
pub const ADULT_THRESHOLD: u8 = 60;

const SCENE_TAG: u8 = 90;
const SITE: u8 = 85;
const HENTAI: u8 = 60;
const TERM: u8 = 35;
const SCENE_DATE: u8 = 30;
const MAX_SUPPORT: u8 = 70;
/// A counter-signal takes this much off a strong signal.
const COUNTER: u8 = 40;
/// Strong and supporting signals together.
const BOTH: u8 = 10;

/// Adult sites and studios whose names are unambiguous, written as one
/// word. Two-word names also match across a separator.
const SITES: &[&str] = &[
    "brazzers",
    "realitykings",
    "bangbros",
    "naughtyamerica",
    "digitalplayground",
    "mofos",
    "evilangel",
    "julesjordan",
    "pornhub",
    "xvideos",
    "xhamster",
    "youporn",
    "redtube",
    "onlyfans",
    "manyvids",
    "fansly",
    "chaturbate",
    "metart",
    "hegre",
    "sukebei",
    "javhd",
];

const TERMS: &[&str] = &[
    "porn",
    "porno",
    "pornstar",
    "nsfw",
    "jav",
    "uncensored",
    "milf",
    "blowjob",
    "creampie",
    "gangbang",
    "bukkake",
    "cumshot",
];

const COUNTERS: &[&str] = &[
    "documentary",
    "documentaries",
    "docuseries",
    "docu",
    "education",
    "educational",
    "course",
    "courses",
    "tutorial",
    "lecture",
    "lectures",
    "medical",
    "medicine",
    "anatomy",
    "health",
    "sexology",
    "therapy",
    "addiction",
];

const VR: &[&str] = &[
    "vr", "oculus", "gearvr", "vr180", "vr360", "180x180", "3dh", "lr180",
];

/// The kind an adult-enrichment service reports for a match (§20.9).
/// Only video is matched, so these are the only kinds a match can pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MatchKind {
    Scene,
    Movie,
    Vr,
}

impl MatchKind {
    pub const ALL: [MatchKind; 3] = [MatchKind::Scene, MatchKind::Movie, MatchKind::Vr];

    /// The adult kind the match pins.
    pub fn adult_kind(self) -> AdultKind {
        match self {
            MatchKind::Scene => AdultKind::Scene,
            MatchKind::Movie => AdultKind::Movie,
            MatchKind::Vr => AdultKind::Vr,
        }
    }
}

/// A match from an adult-enrichment service, as classification takes it
/// (§20.9, plan Q13). Only the kind is the caller's: the confidence is
/// always [`MATCH_CONFIDENCE`], so no service score can lower detection's.
/// Deciding whether a service result is a match at all is the caller's
/// job (ENR-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnrichmentMatch {
    pub kind: MatchKind,
}

impl EnrichmentMatch {
    pub fn new(kind: MatchKind) -> EnrichmentMatch {
        EnrichmentMatch { kind }
    }

    /// The confidence a match gives (plan Q13).
    pub fn confidence(self) -> u8 {
        MATCH_CONFIDENCE
    }
}

/// The confidence of an enrichment match (plan Q13): the heuristic cap,
/// so a match never lowers a confidence detection gave, and still below
/// a user correction or classification rule (100).
pub const MATCH_CONFIDENCE: u8 = 99;

/// The highest score detection gives.
const SCORE_CAP: u8 = 99;
// A match never lowers detection's confidence.
const _: () = assert!(SCORE_CAP <= MATCH_CONFIDENCE);

#[derive(Debug, Default)]
struct Signals {
    strong: u8,
    support: u8,
    terms: Vec<&'static str>,
    counter: bool,
    vr: bool,
    scene_date: bool,
}

impl Signals {
    fn scan(&mut self, text: &str) {
        // `hentai-free` says the opposite.
        let text = text.to_lowercase().replace("hentai-free", " ");
        let words: Vec<String> = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_owned)
            .collect();
        for (i, w) in words.iter().enumerate() {
            let w = w.as_str();
            if w == "xxx" && i > 0 {
                self.strong = self.strong.max(SCENE_TAG);
            }
            let pair = words.get(i + 1).map(|next| format!("{w}{next}"));
            if SITES.contains(&w) || pair.is_some_and(|p| SITES.contains(&p.as_str())) {
                self.strong = self.strong.max(SITE);
            }
            if w == "hentai" {
                self.strong = self.strong.max(HENTAI);
            }
            if let Some(&t) = TERMS.iter().find(|&&t| t == w)
                && !self.terms.contains(&t)
            {
                self.terms.push(t);
                self.support = self.support.saturating_add(TERM);
            }
            self.counter |= COUNTERS.contains(&w);
            self.vr |= VR.contains(&w);
        }
        if !self.scene_date && scene_date(&words) {
            self.scene_date = true;
            self.support = self.support.saturating_add(SCENE_DATE);
        }
    }

    fn score(&self, r: &Release) -> u8 {
        let mut strong = self.strong;
        let mut support = self.support.min(MAX_SUPPORT);
        if r.season.is_some() || r.episode.is_some() {
            support = 0;
        }
        if self.counter {
            strong = strong.saturating_sub(COUNTER);
            support = 0;
        }
        let both = if strong > 0 && support > 0 { BOTH } else { 0 };
        strong.max(support).saturating_add(both).min(SCORE_CAP)
    }
}

/// `Site.24.01.15.…`: a site name, then a two-digit year, month and day.
fn scene_date(words: &[String]) -> bool {
    let two = |i: usize| {
        words
            .get(i)
            .filter(|w| w.len() == 2 && w.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|w| w.parse::<u8>().ok())
    };
    matches!(
        (two(1), two(2), two(3)),
        (Some(_), Some(1..=12), Some(1..=31))
    ) && !words[0].bytes().all(|b| b.is_ascii_digit())
}

/// Whether the torrent is adult, as what kind and how surely. `base` is
/// the content type it would otherwise have.
///
/// `m` is an enrichment match, a strong signal (plan Q13). Without one,
/// this is name-and-files detection alone. With one, the torrent is
/// adult at [`MATCH_CONFIDENCE`] (which no detection score exceeds). The
/// kind never gets less specific: the reported kind is pinned only where
/// the torrent's own kind is video (Scene, Movie or VR), and VR words
/// keep a Scene or Movie match as VR, the way they turn detection's
/// Scene or Movie into VR. A non-video kind from the files (Photos,
/// Comics, Game, Audio) is kept. The name's counter-signals
/// don't weaken a match: only items already classified adult are looked
/// up (plan Q2), and a match can only add (§20.7 "stricter only"). That
/// covers the counters scanned here only; how a future mainstream match
/// (TMDB, TVDB, MusicBrainz, §20.9) weighs against an adult one is for
/// the slice that adds it.
pub(crate) fn detect_matched(
    name: &str,
    release: &Release,
    largest: Option<&FileInfo>,
    base: ContentType,
    m: Option<EnrichmentMatch>,
) -> Option<(AdultKind, u8)> {
    let s = signals(name, largest);
    let detected = detect_from(&s, release, base);
    let Some(m) = m else {
        return detected;
    };
    let own = detected.map_or(forced_kind(base), |(k, _)| k);
    let kind = match own {
        AdultKind::Scene | AdultKind::Movie | AdultKind::Vr => match m.kind.adult_kind() {
            // Detection's VR comes from the same VR words, so `s.vr`
            // covers it.
            AdultKind::Scene | AdultKind::Movie if s.vr => AdultKind::Vr,
            k => k,
        },
        own => own,
    };
    Some((kind, MATCH_CONFIDENCE))
}

fn signals(name: &str, largest: Option<&FileInfo>) -> Signals {
    let mut s = Signals::default();
    s.scan(name);
    if let Some(f) = largest {
        s.scan(f.name());
    }
    s
}

fn detect_from(s: &Signals, release: &Release, base: ContentType) -> Option<(AdultKind, u8)> {
    let score = s.score(release);
    if score < ADULT_THRESHOLD {
        return None;
    }
    // Every type gets its nearest kind, as an Is adult correction gives:
    // a name with no known extension (Other) is a Scene, an Ebook is
    // Comics. They need no more than the threshold, since the same
    // signals and counters decide every type.
    let kind = match base {
        ContentType::Movie if s.scene_date => AdultKind::Scene,
        _ => forced_kind(base),
    };
    let kind = match kind {
        AdultKind::Scene | AdultKind::Movie if s.vr => AdultKind::Vr,
        k => k,
    };
    Some((kind, score))
}

/// The adult kind for a torrent of type `base`, the nearest one where no
/// kind fits exactly: Ebook as Comics, Other as Scene.
pub(crate) fn forced_kind(base: ContentType) -> AdultKind {
    match base {
        ContentType::Movie => AdultKind::Movie,
        ContentType::Tv | ContentType::Video | ContentType::Other | ContentType::Adult => {
            AdultKind::Scene
        }
        ContentType::Images => AdultKind::Photos,
        ContentType::Comic | ContentType::Ebook => AdultKind::Comics,
        ContentType::Game | ContentType::Software => AdultKind::Game,
        ContentType::Music | ContentType::Audiobook => AdultKind::Audio,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(
        name: &str,
        release: &Release,
        largest: Option<&FileInfo>,
        base: ContentType,
    ) -> Option<(AdultKind, u8)> {
        detect_matched(name, release, largest, base, None)
    }

    fn score(name: &str) -> u8 {
        let mut s = Signals::default();
        s.scan(name);
        s.score(&lodestar_release_parser::parse(name))
    }

    #[test]
    fn kinds_round_trip() {
        for k in AdultKind::ALL {
            assert_eq!(AdultKind::from_name(k.as_str()), Some(k));
        }
        assert_eq!(AdultKind::from_name("nope"), None);
    }

    #[test]
    fn strong_signals() {
        assert!(score("Some.Title.XXX.1080p.MP4-GRP") >= ADULT_THRESHOLD);
        assert!(score("Brazzers.24.01.15.Some.Title.1080p") >= ADULT_THRESHOLD);
        assert!(score("Reality Kings - Some Title") >= ADULT_THRESHOLD);
    }

    #[test]
    fn supporting_signals_need_company() {
        assert!(score("Porn.Something.2019") < ADULT_THRESHOLD);
        assert!(score("Porn.Something.Milf.2019") >= ADULT_THRESHOLD);
        assert!(score("Somesite.24.01.15.Porn.Title") >= ADULT_THRESHOLD);
        assert!(score("Somesite.24.01.15.Jane.Doe.1080p") < ADULT_THRESHOLD);
    }

    #[test]
    fn counter_signals_win() {
        assert_eq!(score("Anime.Collection [hentai-free]"), 0);
        assert_eq!(score("Anime Collection (Hentai-Free)"), 0);
        assert!(score("Some Title [Hentai] Free Download") >= ADULT_THRESHOLD);
        assert!(score("Some Title [Hentai] [Hentai-Free Edition]") >= ADULT_THRESHOLD);
        // The film, not the tag.
        assert_eq!(score("xXx.2002.1080p.BluRay.x264"), 0);
        assert!(score("xXx.Return.of.Xander.Cage.2017.1080p") < ADULT_THRESHOLD);
        assert!(score("Porn.Milf.Documentary.2019") < ADULT_THRESHOLD);
        assert!(score("XXX.Industry.Documentary.2019") < ADULT_THRESHOLD);
        assert!(score("Show.Porn.Milf.S01E02.1080p") < ADULT_THRESHOLD);
        assert_eq!(score("Sex.Education.S01.1080p.NF.WEB-DL"), 0);
        assert_eq!(score("Sex.and.the.City.2008.1080p"), 0);
    }

    #[test]
    fn every_type_gets_a_kind() {
        let at = |name: &str, base| {
            detect(name, &lodestar_release_parser::parse(name), None, base).map(|(k, _)| k)
        };
        let name = "Lantern.Bay.Parody.XXX.1080p";
        for base in ContentType::ALL {
            assert_eq!(at(name, base), Some(forced_kind(base)), "{base:?}");
        }
        assert_eq!(at(name, ContentType::Other), Some(AdultKind::Scene));
        assert_eq!(at(name, ContentType::Ebook), Some(AdultKind::Comics));
        // VR words turn Other's Scene into VR, but not Ebook's Comics.
        let vr = "Lantern.Bay.Parody.XXX.VR180.LR";
        assert_eq!(at(vr, ContentType::Other), Some(AdultKind::Vr));
        assert_eq!(at(vr, ContentType::Ebook), Some(AdultKind::Comics));
        // Below the threshold nothing changes, whatever the type.
        for base in ContentType::ALL {
            assert_eq!(at("Irvine Welsh - Porno (2002)", base), None);
        }
    }

    /// `detect` exactly as it was before enrichment matches (ENR-6),
    /// kept as the oracle that unmatched detection hasn't changed.
    fn frozen_detect(
        name: &str,
        release: &Release,
        largest: Option<&FileInfo>,
        base: ContentType,
    ) -> Option<(AdultKind, u8)> {
        let mut s = Signals::default();
        s.scan(name);
        if let Some(f) = largest {
            s.scan(f.name());
        }
        let score = s.score(release);
        if score < ADULT_THRESHOLD {
            return None;
        }
        let kind = match base {
            ContentType::Movie if s.scene_date => AdultKind::Scene,
            _ => forced_kind(base),
        };
        let kind = match kind {
            AdultKind::Scene | AdultKind::Movie if s.vr => AdultKind::Vr,
            k => k,
        };
        Some((kind, score))
    }

    fn word() -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::prelude::*;
        prop_oneof![
            "[A-Za-z0-9]{0,8}",
            "\\PC{0,6}",
            proptest::sample::select(
                [SITES, TERMS, COUNTERS, VR]
                    .concat()
                    .into_iter()
                    .chain([
                        "xxx",
                        "XXX",
                        "hentai",
                        "hentai-free",
                        "S01E02",
                        "24",
                        "01",
                        "15"
                    ])
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            ),
        ]
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(4000))]

        /// No match: detection is exactly what it was before ENR-6.
        #[test]
        fn no_match_matches_frozen_detect(
            name in proptest::collection::vec(word(), 0..8),
            sep in proptest::sample::select(vec![".", " ", "-", "_"]),
            file in proptest::option::of(proptest::collection::vec(word(), 1..5)),
            base in proptest::sample::select(ContentType::ALL.to_vec()),
        ) {
            let name = name.join(sep);
            let file = file.map(|w| FileInfo::new(vec![w.join(".")], 1));
            let r = lodestar_release_parser::parse(&name);
            proptest::prop_assert_eq!(
                detect_matched(&name, &r, file.as_ref(), base, None),
                frozen_detect(&name, &r, file.as_ref(), base)
            );
        }
    }

    #[test]
    fn a_match_pins_its_kind() {
        let at = |name: &str, base, m: Option<MatchKind>| {
            let r = lodestar_release_parser::parse(name);
            detect_matched(name, &r, None, base, m.map(EnrichmentMatch::new))
        };
        // No match: detection as before.
        for name in ["Lantern.Bay.Parody.XXX.1080p", "Some.Movie.2019.1080p"] {
            for base in ContentType::ALL {
                let r = lodestar_release_parser::parse(name);
                assert_eq!(at(name, base, None), frozen_detect(name, &r, None, base));
            }
        }
        // A match on a name detection doesn't call adult.
        let plain = "Some.Title.2019.1080p";
        for k in MatchKind::ALL {
            assert_eq!(
                at(plain, ContentType::Movie, Some(k)),
                Some((k.adult_kind(), MATCH_CONFIDENCE))
            );
        }
        // Counter-signals don't weaken a match.
        let doc = "XXX.Industry.Documentary.2019";
        assert_eq!(
            at(doc, ContentType::Movie, Some(MatchKind::Movie)),
            Some((AdultKind::Movie, MATCH_CONFIDENCE))
        );
        // A non-video kind from the files is kept, detected or not: a
        // match never makes the kind less specific.
        for name in ["Brazzers.24.01.15.Title", "Some.Title.2019"] {
            for base in [
                ContentType::Images,
                ContentType::Comic,
                ContentType::Ebook,
                ContentType::Game,
                ContentType::Software,
                ContentType::Music,
                ContentType::Audiobook,
            ] {
                for k in MatchKind::ALL {
                    assert_eq!(
                        at(name, base, Some(k)),
                        Some((forced_kind(base), MATCH_CONFIDENCE)),
                        "{name} {base:?} {k:?}"
                    );
                }
            }
        }
        // A video kind takes the reported one.
        assert_eq!(
            at(
                "Brazzers.24.01.15.Title",
                ContentType::Movie,
                Some(MatchKind::Movie)
            ),
            Some((AdultKind::Movie, MATCH_CONFIDENCE))
        );
        // VR stays VR.
        let vr = "Lantern.Bay.Parody.XXX.VR180.LR";
        for k in MatchKind::ALL {
            assert_eq!(
                at(vr, ContentType::Video, Some(k)),
                Some((AdultKind::Vr, MATCH_CONFIDENCE))
            );
        }
        assert_eq!(
            at(
                "Some.Title.VR180",
                ContentType::Video,
                Some(MatchKind::Scene)
            ),
            Some((AdultKind::Vr, MATCH_CONFIDENCE))
        );
        assert_eq!(EnrichmentMatch::new(MatchKind::Scene).confidence(), 99);
    }

    #[test]
    fn scene_dates() {
        let w = |s: &str| -> Vec<String> { s.split('.').map(str::to_lowercase).collect() };
        assert!(scene_date(&w("Site.24.01.15.Name")));
        assert!(!scene_date(&w("Site.24.13.15.Name")));
        assert!(!scene_date(&w("Site.2024.01.15.Name")));
        assert!(!scene_date(&w("10.24.01.15")));
        assert!(!scene_date(&w("Site.24")));
    }
}
