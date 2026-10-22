//! Adult as a media category (§20.9), scored for precision: a torrent is
//! only called adult on a strong signal or several supporting ones, and
//! counter-signals win.
//!
//! - Strong: the scene `XXX` tag (never as the first word, which is the
//!   film *xXx*), a known adult site or studio, `hentai`.
//! - Supporting: adult terms in the name, the `Site.YY.MM.DD.…` scene
//!   naming.
//! - Counter: documentary, educational and medical words cancel the
//!   supporting signals and weaken the strong ones; season and episode
//!   numbers (mainstream TV like *Sex Education*) cancel the supporting
//!   ones.
//!
//! Signals that need more than the name and files (the source's own
//! category, enrichment matches, user corrections) arrive with the filter
//! engine and the plugins; they adjust this score rather than replace it.

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
        let words: Vec<String> = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
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
            if let Some(&t) = TERMS.iter().find(|&&t| t == w) {
                if !self.terms.contains(&t) {
                    self.terms.push(t);
                    self.support = self.support.saturating_add(TERM);
                }
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
        strong.max(support).saturating_add(both).min(99)
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
pub(crate) fn detect(
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
        // No adult kind fits: the torrent keeps its type.
        ContentType::Ebook | ContentType::Other | ContentType::Adult => return None,
        _ => forced_kind(base),
    };
    let kind = match kind {
        AdultKind::Scene | AdultKind::Movie if s.vr => AdultKind::Vr,
        k => k,
    };
    Some((kind, score))
}

/// The adult kind for a torrent of type `base`. When you say a torrent is
/// adult, the types detection leaves alone get the nearest kind too:
/// Ebook as Comics, Other as Scene.
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
    fn scene_dates() {
        let w = |s: &str| -> Vec<String> { s.split('.').map(str::to_lowercase).collect() };
        assert!(scene_date(&w("Site.24.01.15.Name")));
        assert!(!scene_date(&w("Site.24.13.15.Name")));
        assert!(!scene_date(&w("Site.2024.01.15.Name")));
        assert!(!scene_date(&w("10.24.01.15")));
        assert!(!scene_date(&w("Site.24")));
    }
}
