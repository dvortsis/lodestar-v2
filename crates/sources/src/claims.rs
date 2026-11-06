//! What a source asserts about a key (architecture §18.1, §18.5).
//!
//! Claims are hints: a name, a file list, trackers, external IDs, a
//! description and a publisher, exactly as the source stated them.
//! Verified metadata (the BEP 9 info dict) always wins over them. They
//! come from untrusted feeds, so every claim is bounded: [`Claims::clamp`]
//! cuts each field to the limits below, and the [`Emitter`] applies it to
//! every event, so nothing past the contract boundary ever sees more.
//! Clamping trims; it never rejects, because a long name is still a
//! useful hint.
//!
//! Clamping bounds what crosses the contract, after the fact. **A parser
//! caps as it reads** (SRC-6 and later): it stops collecting a field at
//! these limits rather than building a 40 MB value for the emitter to
//! cut, and the clamp releases what it cuts.
//!
//! The child-safety filter still runs on every claim before storage
//! (§18.7, §20.1). Bounding here doesn't replace it.
//!
//! **The adult-enrichment section** ([`AdultEnrichment`], ENR-0) is the
//! store's own type, re-exported, so the two crates can't disagree on what
//! it may hold: names only, no performer id or other personal field
//! (owner §2.37, plan Q4 and Q5). Clamping it degrades rather than
//! spoils: a cut name may name someone else, so an over-long studio or
//! performer name is dropped, not cut (a dropped performer sets
//! `performers_truncated`), and a section whose scene id isn't valid is
//! dropped whole, since a cut id points at another scene.
//!
//! **The details section** ([`Details`], ENR-9) is shared the same way:
//! alternative titles, a canonical title, genres, a year and a match
//! confidence from an enrichment plugin, and a feed's category, posting
//! time and newsgroups. It describes a work, never a person. Clamping it
//! cuts free text (titles, the category) and drops what can't be cut
//! without changing its meaning (a language tag, a newsgroup, an id), and
//! claims with a details section keep only the external ids
//! [`id_ok`] accepts.
//!
//! [`Emitter`]: crate::Emitter

use lodestar_store::claims::scene_id_ok;
pub use lodestar_store::claims::{
    AdultEnrichment, AdultKind, AltTitle, Details, ENRICHMENT_SCHEMES, MAX_ALT_TITLE,
    MAX_ALT_TITLES, MAX_CANONICAL_TITLE, MAX_CATEGORY, MAX_GENRE, MAX_GENRES, MAX_LANGUAGE,
    MAX_MATCH_CONFIDENCE, MAX_NEWSGROUP, MAX_NEWSGROUPS, MAX_PERFORMER, MAX_PERFORMERS,
    MAX_POSTED_AT, MAX_SCENE_ID, MAX_SCENE_TITLE, MAX_STUDIO, ReleaseDate, genre_ok, id_ok,
    language_ok, newsgroup_ok,
};

/// Bytes in a claimed name.
pub const MAX_NAME: usize = 1024;
/// Claimed files per key.
pub const MAX_FILES: usize = 10_000;
/// Bytes in one claimed file path.
pub const MAX_PATH: usize = 4096;
/// Claimed trackers per key.
pub const MAX_TRACKERS: usize = 64;
/// Bytes in one tracker URL.
pub const MAX_TRACKER: usize = 2048;
/// External IDs per key.
pub const MAX_IDS: usize = 32;
/// Bytes in an ID's scheme (`imdb`) or value (`tt0111161`).
pub const MAX_ID_PART: usize = 128;
/// Bytes in a claimed description.
pub const MAX_DESCRIPTION: usize = 64 * 1024;
/// Bytes in a publisher identity.
pub const MAX_PUBLISHER: usize = 256;
/// Bytes in a provenance external reference (a GUID or item URL).
pub const MAX_EXTERNAL_REF: usize = 2048;
/// Bytes of text in one event's claims overall. Every other field fits
/// in about 220 KiB at its own limit (the adult section's ~17 KiB
/// included), so this bounds the file list: files
/// past the budget are dropped. Without it one event could hold 40 MB.
pub const MAX_TOTAL: usize = 1024 * 1024;

/// One claimed file. Sizes are what the source says, not what's true.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedFile {
    pub path: String,
    pub size: Option<u64>,
}

/// An external ID, such as `imdb:tt0111161`, `tmdb:278` or `anidb:1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalId {
    /// Lower-case scheme, without the colon.
    pub scheme: String,
    pub value: String,
}

/// Everything a source asserted about one key. Every field is optional.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Claims {
    pub name: Option<String>,
    /// The total size the source states, which may disagree with `files`.
    pub size: Option<u64>,
    pub files: Vec<ClaimedFile>,
    pub trackers: Vec<String>,
    pub ids: Vec<ExternalId>,
    pub description: Option<String>,
    /// The uploader or channel as the source names it. A public handle
    /// from the feed, never a peer address (§18.7: no profiles of people).
    pub publisher: Option<String>,
    /// An adult-enrichment match (ENR-0, architecture §20.9), from an
    /// enrichment source only.
    pub adult: Option<AdultEnrichment>,
    /// The details section (ENR-9): what an enrichment plugin resolved
    /// the item to, or what a feed says beyond a name and files.
    pub details: Option<Box<Details>>,
}

impl Claims {
    /// True when the source asserted nothing.
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.size.is_none()
            && self.files.is_empty()
            && self.trackers.is_empty()
            && self.ids.is_empty()
            && self.description.is_none()
            && self.publisher.is_none()
            && self.adult.is_none()
            && self.details.is_none()
    }

    /// Cuts every field to its limit and the whole to [`MAX_TOTAL`].
    /// Strings are cut on a character boundary; lists keep their first
    /// non-empty entries. Empty strings become `None`, and IDs with an
    /// empty part are dropped.
    pub fn clamp(&mut self) {
        clamp_opt(&mut self.name, MAX_NAME);
        self.trackers.retain(|t| !t.is_empty());
        self.trackers.truncate(MAX_TRACKERS);
        for t in &mut self.trackers {
            truncate(t, MAX_TRACKER);
        }
        for id in &mut self.ids {
            truncate(&mut id.scheme, MAX_ID_PART);
            id.scheme.make_ascii_lowercase();
            truncate(&mut id.value, MAX_ID_PART);
        }
        self.ids
            .retain(|id| !id.scheme.is_empty() && !id.value.is_empty());
        clamp_opt(&mut self.description, MAX_DESCRIPTION);
        clamp_opt(&mut self.publisher, MAX_PUBLISHER);
        if let Some(a) = &mut self.adult {
            if clamp_adult(a) {
                // Nothing rides along beside a section (the store refuses
                // it): no description (plan Q3), publisher or ids (Q4).
                self.description = None;
                self.publisher = None;
                self.ids = Vec::new();
            } else {
                self.adult = None;
            }
            // One section a row: claims that came with an adult section
            // carry no details, whether or not the section itself is kept.
            self.details = None;
        }
        if let Some(d) = &mut self.details {
            clamp_details(d);
            if d.is_empty() {
                self.details = None;
            } else {
                // No overview or poster beside the section (the store
                // refuses them): either could name a person.
                self.description = None;
                self.publisher = None;
                // The ids enrichment writes, each once (store: `id_ok`),
                // filtered before the cut so bad ones can't crowd them out.
                let mut kept: Vec<ExternalId> = Vec::new();
                for mut id in std::mem::take(&mut self.ids) {
                    if kept.len() == MAX_IDS {
                        break;
                    }
                    if id.scheme == "mbid" {
                        id.value.make_ascii_lowercase();
                    }
                    if id_ok(&id.scheme, &id.value) && !kept.contains(&id) {
                        kept.push(id);
                    }
                }
                self.ids = kept;
            }
        }
        self.ids.truncate(MAX_IDS);
        let mut total = self.fixed_len();
        self.files.truncate(MAX_FILES);
        let mut keep = 0;
        for f in &mut self.files {
            truncate(&mut f.path, MAX_PATH);
            total += f.path.len();
            if total > MAX_TOTAL {
                break;
            }
            keep += 1;
        }
        self.files.truncate(keep);
        self.files.shrink_to_fit();
        self.trackers.shrink_to_fit();
        self.ids.shrink_to_fit();
    }

    /// Bytes of text held, the measure [`MAX_TOTAL`] bounds.
    pub fn text_len(&self) -> usize {
        self.fixed_len() + self.files.iter().map(|f| f.path.len()).sum::<usize>()
    }

    /// Bytes of text outside the file list.
    fn fixed_len(&self) -> usize {
        let len = |s: &Option<String>| s.as_ref().map_or(0, String::len);
        len(&self.name)
            + len(&self.description)
            + len(&self.publisher)
            + self.trackers.iter().map(String::len).sum::<usize>()
            + self
                .ids
                .iter()
                .map(|i| i.scheme.len() + i.value.len())
                .sum::<usize>()
            + self.adult.as_ref().map_or(0, AdultEnrichment::text_len)
            + self.details.as_ref().map_or(0, |d| d.charged_len())
    }
}

/// Bounds `d` in place, in the store's form: free text cut, a language
/// tag lower-cased, genres normalised, and every entry that still isn't
/// valid dropped, as are duplicates. May leave it empty.
fn clamp_details(d: &mut Details) {
    clamp_opt(&mut d.source_category, MAX_CATEGORY);
    clamp_opt(&mut d.canonical_title, MAX_CANONICAL_TITLE);
    if d.year.is_some_and(|y| !(1..=9999).contains(&y)) {
        d.year = None;
    }
    if d.posted_at
        .is_some_and(|t| !(0..=MAX_POSTED_AT).contains(&t))
    {
        d.posted_at = None;
    }
    if d.match_confidence.is_some_and(|c| c > MAX_MATCH_CONFIDENCE) {
        d.match_confidence = None;
    }
    let mut groups: Vec<String> = Vec::new();
    for g in std::mem::take(&mut d.newsgroups) {
        if groups.len() < MAX_NEWSGROUPS && newsgroup_ok(&g) && !groups.contains(&g) {
            groups.push(g);
        }
    }
    d.newsgroups = groups;
    let mut titles: Vec<AltTitle> = Vec::new();
    for mut a in std::mem::take(&mut d.alt_titles) {
        if titles.len() >= MAX_ALT_TITLES {
            break;
        }
        a.language.make_ascii_lowercase();
        truncate(&mut a.title, MAX_ALT_TITLE);
        if language_ok(&a.language) && !a.title.is_empty() && !titles.contains(&a) {
            titles.push(a);
        }
    }
    d.alt_titles = titles;
    let mut genres: Vec<String> = Vec::new();
    for g in std::mem::take(&mut d.genres) {
        if genres.len() >= MAX_GENRES {
            break;
        }
        if let Some(g) = normalise_genre(&g).filter(|g| !genres.contains(g)) {
            genres.push(g);
        }
    }
    d.genres = genres;
}

/// A genre in its stored form ([`genre_ok`]): lower case, control
/// characters gone, words joined by one space, cut to [`MAX_GENRE`].
fn normalise_genre(g: &str) -> Option<String> {
    let lower: String = g
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .to_lowercase();
    let mut out = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&mut out, MAX_GENRE);
    let out = out.trim_end().to_owned();
    genre_ok(&out).then_some(out)
}

/// Bounds `a` in place; false when it can't be kept (a bad scene id).
fn clamp_adult(a: &mut AdultEnrichment) -> bool {
    if !scene_id_ok(&a.scene_id) {
        return false;
    }
    clamp_opt(&mut a.title, MAX_SCENE_TITLE);
    if a.studio
        .as_ref()
        .is_some_and(|s| s.is_empty() || s.len() > MAX_STUDIO)
    {
        a.studio = None;
    }
    let before = a.performers.len();
    a.performers
        .retain(|p| !p.is_empty() && p.len() <= MAX_PERFORMER);
    a.performers.truncate(MAX_PERFORMERS);
    if a.performers.len() < before {
        a.performers_truncated = true;
    }
    true
}

/// How much the source's word is worth, as the plugin judges it. Ranking
/// treats every claim as weaker than verified metadata whatever this says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Confidence {
    /// Scraped or user-submitted text with no checks.
    #[default]
    Low,
    /// A curated feed or an indexer's own record.
    Medium,
    /// The publisher's own official list.
    High,
}

/// Where a discovery came from, beyond the source id the [`Emitter`]
/// stamps on it.
///
/// [`Emitter`]: crate::Emitter
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Provenance {
    /// The source's own reference: a feed item GUID or its page URL.
    pub external_ref: Option<String>,
    /// When the source says the item was published, in Unix seconds. The
    /// time Lodestar saw it is [`Discovery::at`](crate::Discovery::at).
    pub published_at: Option<i64>,
    pub confidence: Confidence,
}

impl Provenance {
    pub fn clamp(&mut self) {
        clamp_opt(&mut self.external_ref, MAX_EXTERNAL_REF);
    }
}

/// Cuts `s` to at most `max` bytes on a character boundary.
pub(crate) fn truncate(s: &mut String, max: usize) {
    if s.len() > max {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        // Give back what was cut: clamping bounds memory, not just length.
        s.shrink_to_fit();
    }
}

fn clamp_opt(s: &mut Option<String>, max: usize) {
    if let Some(v) = s {
        truncate(v, max);
        if v.is_empty() {
            *s = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_char_boundaries() {
        let mut s = "aé".repeat(10); // 3 bytes per pair
        truncate(&mut s, 4);
        assert_eq!(s, "aéa");
        let mut short = String::from("ok");
        truncate(&mut short, 4);
        assert_eq!(short, "ok");
    }

    #[test]
    fn clamp_bounds_every_field() {
        let long = "x".repeat(MAX_DESCRIPTION * 2);
        let mut c = Claims {
            name: Some(long.clone()),
            size: Some(1),
            files: vec![
                ClaimedFile {
                    path: long.clone(),
                    size: None
                };
                MAX_FILES + 5
            ],
            trackers: vec![long.clone(); MAX_TRACKERS + 5],
            ids: vec![
                ExternalId {
                    scheme: "IMDB".into(),
                    value: long.clone()
                };
                MAX_IDS + 5
            ],
            description: Some(long.clone()),
            publisher: Some(long),
            adult: None,
            details: None,
        };
        c.clamp();
        assert_eq!(c.name.as_ref().unwrap().len(), MAX_NAME);
        assert!(c.files.iter().all(|f| f.path.len() == MAX_PATH));
        assert!(c.text_len() <= MAX_TOTAL);
        assert!(!c.files.is_empty() && c.files.len() < MAX_FILES);
        assert_eq!(c.trackers.len(), MAX_TRACKERS);
        assert_eq!(c.ids.len(), MAX_IDS);
        assert_eq!(c.ids[0].scheme, "imdb");
        assert_eq!(c.ids[0].value.len(), MAX_ID_PART);
        assert_eq!(c.description.as_ref().unwrap().len(), MAX_DESCRIPTION);
        assert_eq!(c.publisher.as_ref().unwrap().len(), MAX_PUBLISHER);
        assert!(lodestar_store::claims::encode(&to_store(&c), 3).is_ok());
    }

    #[test]
    fn clamp_bounds_the_adult_section() {
        let long = "x".repeat(MAX_SCENE_TITLE * 2);
        let mut c = Claims {
            name: Some("n".into()),
            ids: vec![ExternalId {
                scheme: "stashdb-performer".into(),
                value: "0b9e4c1a".into(),
            }],
            description: Some("d".into()),
            publisher: Some("p".into()),
            adult: Some(AdultEnrichment {
                title: Some(long),
                studio: Some("s".repeat(MAX_STUDIO + 1)),
                performers: vec!["p".repeat(MAX_PERFORMER); MAX_PERFORMERS + 5],
                ..AdultEnrichment::new("a".repeat(MAX_SCENE_ID))
            }),
            ..Claims::default()
        };
        c.clamp();
        // Nothing rides along beside the section; the name stays.
        assert_eq!(c.name.as_deref(), Some("n"));
        assert!(c.ids.is_empty() && c.description.is_none() && c.publisher.is_none());
        let a = c.adult.as_ref().unwrap();
        assert_eq!(a.title.as_ref().unwrap().len(), MAX_SCENE_TITLE);
        assert_eq!(a.studio, None, "a cut name may name someone else");
        assert_eq!(a.performers.len(), MAX_PERFORMERS);
        assert!(a.performers_truncated);
        assert!(lodestar_store::claims::encode(&to_store(&c), 3).is_ok());
    }

    /// The store's claims for `c`, as SRC-2's mapping builds them, so the
    /// clamp's output is checked against what the store accepts.
    fn to_store(c: &Claims) -> lodestar_store::claims::Claims {
        use lodestar_store::claims as s;
        s::Claims {
            name: c.name.clone(),
            size: c.size,
            files: c
                .files
                .iter()
                .map(|f| s::ClaimedFile {
                    path: f.path.clone(),
                    size: f.size,
                })
                .collect(),
            trackers: c.trackers.clone(),
            ids: c
                .ids
                .iter()
                .map(|i| s::ExternalId {
                    scheme: i.scheme.clone(),
                    value: i.value.clone(),
                })
                .collect(),
            description: c.description.clone(),
            publisher: c.publisher.clone(),
            provenance: s::Provenance::default(),
            adult: c.adult.clone(),
            details: c.details.clone(),
        }
    }

    #[test]
    fn clamp_degrades_the_adult_section() {
        // Bad performer entries are dropped and flagged; the rest stays.
        let mut c = Claims {
            adult: Some(AdultEnrichment {
                studio: Some(String::new()),
                performers: vec![
                    "A".into(),
                    String::new(),
                    "p".repeat(MAX_PERFORMER + 1),
                    "B".into(),
                ],
                ..AdultEnrichment::new("123")
            }),
            ..Claims::default()
        };
        c.clamp();
        let a = c.adult.as_ref().unwrap();
        assert_eq!(a.performers, ["A", "B"]);
        assert!(a.performers_truncated);
        assert_eq!(a.studio, None);
        assert!(!c.is_empty());

        // A bad scene id drops the section whole: it can't be cut.
        for id in [
            String::new(),
            "a b".into(),
            "../x".into(),
            "é".into(),
            "x".repeat(MAX_SCENE_ID + 1),
        ] {
            let mut c = Claims {
                adult: Some(AdultEnrichment::new(id)),
                ..Claims::default()
            };
            c.clamp();
            assert!(c.is_empty());
        }
    }

    #[test]
    fn the_adult_section_counts_toward_the_total() {
        let mut c = Claims {
            files: vec![
                ClaimedFile {
                    path: "f".repeat(MAX_PATH),
                    size: None
                };
                MAX_TOTAL / MAX_PATH
            ],
            adult: Some(AdultEnrichment {
                performers: vec!["p".repeat(MAX_PERFORMER); MAX_PERFORMERS],
                ..AdultEnrichment::new("1")
            }),
            ..Claims::default()
        };
        c.clamp();
        assert!(c.text_len() <= MAX_TOTAL);
        assert!(c.files.len() < MAX_TOTAL / MAX_PATH, "files make room");
        assert_eq!(c.adult.as_ref().unwrap().performers.len(), MAX_PERFORMERS);
        assert!(lodestar_store::claims::encode(&to_store(&c), 3).is_ok());
    }

    #[test]
    fn clamp_bounds_the_details_section() {
        let mut c = Claims {
            name: Some("n".into()),
            ids: vec![
                ExternalId {
                    scheme: "IMDB".into(),
                    value: "tt0111161".into(),
                },
                ExternalId {
                    scheme: "imdb".into(),
                    value: "tt0111161".into(),
                },
                ExternalId {
                    scheme: "imdb".into(),
                    value: "nm0000151".into(),
                },
                ExternalId {
                    scheme: "rage".into(),
                    value: "1".into(),
                },
                ExternalId {
                    scheme: "tmdb".into(),
                    value: "278".into(),
                },
                ExternalId {
                    scheme: "mbid".into(),
                    value: "F27EC8DB-AF05-4F36-916E-3D57F91ECF5E".into(),
                },
            ],
            description: Some("An overview naming the cast".into()),
            publisher: Some("poster@example.com".into()),
            details: Some(Box::new(Details {
                source_category: Some("c".repeat(MAX_CATEGORY + 9)),
                posted_at: Some(MAX_POSTED_AT),
                newsgroups: (0..20)
                    .map(|i| format!("alt.g{}", i % 10))
                    .chain(["bad group".into()])
                    .collect(),
                alt_titles: [
                    ("EN-US", "Title"),
                    ("en-us", "Title"),
                    ("en_US", "Bad tag"),
                    ("ja", ""),
                    ("de", &"é".repeat(MAX_ALT_TITLE)),
                ]
                .into_iter()
                .map(|(l, t)| AltTitle {
                    language: l.into(),
                    title: t.into(),
                })
                .chain((0..40).map(|i| AltTitle {
                    language: "und".into(),
                    title: format!("t{i}"),
                }))
                .collect(),
                canonical_title: Some("n".repeat(MAX_CANONICAL_TITLE + 1)),
                genres: [
                    "  Science   Fiction ",
                    "science fiction",
                    "DRAMA\u{7}",
                    "\t",
                    "ÉPOPÉE",
                ]
                .into_iter()
                .map(str::to_owned)
                .chain((0..30).map(|i| format!("g{i}")))
                .chain(["x ".repeat(MAX_GENRE)])
                .collect(),
                year: Some(10_000),
                match_confidence: Some(101),
            })),
            ..Claims::default()
        };
        c.clamp();
        let d = c.details.as_deref().unwrap();
        assert_eq!(d.source_category.as_ref().unwrap().len(), MAX_CATEGORY);
        assert_eq!(d.posted_at, Some(MAX_POSTED_AT));
        assert_eq!(d.newsgroups.len(), MAX_NEWSGROUPS);
        assert_eq!(d.newsgroups[0], "alt.g0");
        assert_eq!(d.alt_titles.len(), MAX_ALT_TITLES);
        assert_eq!(d.alt_titles[0].language, "en-us");
        assert_eq!(d.alt_titles[0].title, "Title");
        assert_eq!(d.alt_titles[1].language, "de");
        assert!(d.alt_titles[1].title.len() <= MAX_ALT_TITLE);
        assert_eq!(d.alt_titles[2].title, "t0");
        assert_eq!(
            d.canonical_title.as_ref().unwrap().len(),
            MAX_CANONICAL_TITLE
        );
        assert_eq!(d.genres.len(), MAX_GENRES);
        assert_eq!(d.genres[..3], ["science fiction", "drama", "épopée"]);
        assert_eq!(d.year, None);
        assert_eq!(d.match_confidence, None);
        // Only enrichment ids, each once.
        let ids: Vec<_> = c.ids.iter().map(|i| (&*i.scheme, &*i.value)).collect();
        assert_eq!(
            ids,
            [
                ("imdb", "tt0111161"),
                ("tmdb", "278"),
                ("mbid", "f27ec8db-af05-4f36-916e-3d57f91ecf5e")
            ]
        );
        assert!(c.description.is_none() && c.publisher.is_none());
        assert_eq!(c.name.as_deref(), Some("n"));
        let blob = lodestar_store::claims::encode(&to_store(&c), 3).unwrap();
        assert_eq!(blob[0], lodestar_store::claims::VERSION_DETAILS);
    }

    #[test]
    fn a_genre_cut_at_the_cap_loses_its_trailing_space() {
        let mut c = Claims {
            details: Some(Box::new(Details {
                genres: vec![format!("{} b", "a".repeat(MAX_GENRE - 1))],
                ..Details::default()
            })),
            ..Claims::default()
        };
        c.clamp();
        assert_eq!(c.details.unwrap().genres, ["a".repeat(MAX_GENRE - 1)]);
    }

    #[test]
    fn clamp_drops_an_empty_or_shared_details_section() {
        let bad = Details {
            posted_at: Some(MAX_POSTED_AT + 1),
            year: Some(0),
            match_confidence: Some(200),
            newsgroups: vec!["..".into()],
            alt_titles: vec![AltTitle {
                language: "x".into(),
                title: "t".into(),
            }],
            genres: vec![" ".into()],
            ..Details::default()
        };
        let mut c = Claims {
            ids: vec![ExternalId {
                scheme: "rage".into(),
                value: "1".into(),
            }],
            details: Some(Box::new(bad)),
            ..Claims::default()
        };
        c.clamp();
        assert!(c.details.is_none());
        assert_eq!(c.ids.len(), 1, "no section: any id stays");

        // An adult match carries nothing beside it.
        let mut c = Claims {
            adult: Some(AdultEnrichment::new("1")),
            details: Some(Box::new(Details {
                year: Some(2000),
                ..Details::default()
            })),
            ..Claims::default()
        };
        c.clamp();
        assert!(c.adult.is_some() && c.details.is_none());
        assert!(lodestar_store::claims::encode(&to_store(&c), 3).is_ok());
    }

    #[test]
    fn the_details_section_counts_toward_the_total() {
        let mut c = Claims {
            files: vec![
                ClaimedFile {
                    path: "f".repeat(MAX_PATH),
                    size: None
                };
                MAX_TOTAL / MAX_PATH
            ],
            details: Some(Box::new(Details {
                alt_titles: (0..MAX_ALT_TITLES)
                    .map(|i| AltTitle {
                        language: "en".into(),
                        title: format!("{i:02}{}", "t".repeat(MAX_ALT_TITLE - 2)),
                    })
                    .collect(),
                ..Details::default()
            })),
            ..Claims::default()
        };
        c.clamp();
        assert!(c.text_len() <= MAX_TOTAL);
        assert!(c.files.len() < MAX_TOTAL / MAX_PATH, "files make room");
        assert_eq!(c.details.as_ref().unwrap().alt_titles.len(), MAX_ALT_TITLES);
        assert!(lodestar_store::claims::encode(&to_store(&c), 3).is_ok());
    }

    #[test]
    fn bad_ids_cannot_crowd_out_good_ones() {
        let mut ids = vec![
            ExternalId {
                scheme: "rage".into(),
                value: "1".into(),
            };
            MAX_IDS * 2
        ];
        ids.push(ExternalId {
            scheme: "tmdb".into(),
            value: "278".into(),
        });
        let mut c = Claims {
            ids,
            details: Some(Box::new(Details {
                year: Some(2000),
                ..Details::default()
            })),
            ..Claims::default()
        };
        c.clamp();
        assert_eq!(c.ids.len(), 1);
        assert_eq!(c.ids[0].value, "278");
    }

    mod props {
        use super::super::*;
        use super::to_store;
        use proptest::prelude::*;

        fn any_text() -> impl Strategy<Value = String> {
            prop_oneof![
                4 => proptest::string::string_regex("(.|\\s){0,12}").unwrap(),
                1 => "[A-Za-z .+_-]{0,12}",
                1 => (1usize..1200).prop_map(|n| "é".repeat(n)),
            ]
        }

        fn any_details() -> impl Strategy<Value = Details> {
            (
                (
                    proptest::option::of(any_text()),
                    proptest::option::of(any::<i64>()),
                    proptest::collection::vec(any_text(), 0..12),
                    proptest::collection::vec(
                        (any_text(), any_text())
                            .prop_map(|(language, title)| AltTitle { language, title }),
                        0..40,
                    ),
                ),
                (
                    proptest::option::of(any_text()),
                    proptest::collection::vec(any_text(), 0..20),
                    proptest::option::of(any::<u16>()),
                    proptest::option::of(any::<u8>()),
                ),
            )
                .prop_map(
                    |(
                        (source_category, posted_at, newsgroups, alt_titles),
                        (canonical_title, genres, year, m),
                    )| {
                        Details {
                            source_category,
                            posted_at,
                            newsgroups,
                            alt_titles,
                            canonical_title,
                            genres,
                            year,
                            match_confidence: m,
                        }
                    },
                )
        }

        fn any_ids() -> impl Strategy<Value = Vec<ExternalId>> {
            proptest::collection::vec(
                (
                    prop_oneof![
                        Just("imdb".to_owned()),
                        Just("TMDB".to_owned()),
                        Just("mbid".to_owned()),
                        "[a-z]{0,6}",
                    ],
                    prop_oneof![
                        Just("tt0111161".to_owned()),
                        Just("278".to_owned()),
                        "[0-9a-fA-F-]{0,36}",
                        any_text(),
                    ],
                )
                    .prop_map(|(scheme, value)| ExternalId { scheme, value }),
                0..40,
            )
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(512))]

            /// Whatever a source sends, the clamp leaves claims the store
            /// accepts, round-trips, and stores as version 3 exactly when a
            /// details section survives.
            #[test]
            fn clamped_details_always_encode(
                d in any_details(),
                ids in any_ids(),
                description in proptest::option::of(any_text()),
                publisher in proptest::option::of(any_text()),
                adult in any::<bool>(),
            ) {
                let mut c = Claims {
                    name: Some("n".into()),
                    ids,
                    description,
                    publisher,
                    adult: adult.then(|| AdultEnrichment::new("1")),
                    details: Some(Box::new(d)),
                    ..Claims::default()
                };
                c.clamp();
                let s = to_store(&c);
                let blob = lodestar_store::claims::encode(&s, 1);
                prop_assert!(blob.is_ok(), "{:?}: {:?}", blob, c);
                let blob = blob.unwrap();
                prop_assert_eq!(&lodestar_store::claims::decode(&blob).unwrap(), &s);
                prop_assert_eq!(
                    blob[0] == lodestar_store::claims::VERSION_DETAILS,
                    c.details.is_some()
                );
                prop_assert!(!(adult && c.details.is_some()));
                // Clamping twice changes nothing.
                let mut again = c.clone();
                again.clamp();
                prop_assert_eq!(again, c);
            }
        }
    }

    #[test]
    fn small_file_lists_are_kept_whole() {
        let mut c = Claims {
            files: vec![
                ClaimedFile {
                    path: "a/b.mkv".into(),
                    size: Some(1)
                };
                MAX_FILES + 1
            ],
            ..Claims::default()
        };
        c.clamp();
        assert_eq!(c.files.len(), MAX_FILES);
    }

    #[test]
    fn clamp_drops_empty_parts() {
        let mut c = Claims {
            name: Some(String::new()),
            trackers: vec![String::new()],
            ids: vec![ExternalId {
                scheme: "tmdb".into(),
                value: String::new(),
            }],
            ..Claims::default()
        };
        c.clamp();
        assert!(c.is_empty());
    }
}
