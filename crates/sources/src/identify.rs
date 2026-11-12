//! General enrichment plugins (architecture §6, §18.1, §18.2; plan
//! `docs/plan/2026-11-05-v07-m7.md` ENR-10, Q5 and Q12): the contract
//! between the pipeline's identification runtime
//! (`lodestar_pipeline::identify`) and a service such as Wikidata, TMDB or
//! MusicBrainz. It's named `identify` so it's never confused with ENR-1's
//! adult [`enrich`](crate::enrich) contract.
//!
//! **What may leave the install is decided by the runtime, not here.** It
//! hands a plugin a [`Query`] only for an item its eligibility gate passed
//! (Q5: verified, never held, hidden, `private_source`, unverified or
//! Adult). A [`Query`] is the only thing a plugin is ever given about an
//! item, and it holds only the parsed title, year and type (Q5). There's no
//! field for an info hash, the raw name, a file list, trackers or anything
//! about the install, so a plugin can't be handed one. Its `Debug` prints
//! lengths only.
//!
//! A plugin answers with a [`Lookup`]: an [`Identification`], or no match.
//! The runtime turns an identification into claims under the plugin's own
//! source id ([`Identification::into_claims`]): the ids beside a details
//! section (ENR-9) that always holds at least the match confidence, so the
//! closed id schemes apply. Those claims go through the writer's
//! filter-before-storage path like every other claim. Each plugin's answer
//! is its own claims row, so two plugins that disagree are both kept, each
//! with its confidence.

use std::fmt;
use std::time::Duration;

use crate::claims::{AltTitle, Claims, Details, ExternalId, MAX_MATCH_CONFIDENCE, truncate};
use crate::manifest::{Manifest, Policy};
use crate::plugin::{BoxFuture, Kit, PluginError};

/// Lookups one plugin may make a UTC day, user interest and background
/// together (plan Q12, the runner's number under §2.26; one cap for both is
/// stricter than the plan's "after user interest"). Each plugin's own rate
/// cap applies on top.
pub const PER_DAY: u32 = 100_000;
/// How long a match is cached before the item may be looked up again.
pub const HIT_TTL_SECS: i64 = 30 * 86_400;
/// How long a miss is cached.
pub const MISS_TTL_SECS: i64 = 7 * 86_400;
/// The least time between two lookups by one plugin, whatever it asks for
/// ([`Identifier::min_interval`]).
pub const MIN_INTERVAL_FLOOR: Duration = Duration::from_millis(100);
/// The prefix of every `source_state` key the runtime keeps for a plugin.
/// A plugin can neither read nor write keys under it.
pub const RUNTIME_PREFIX: &str = "enr10/";
/// Bytes in a query's title. A parsed title is short; a longer one is cut.
pub const MAX_QUERY_TITLE: usize = 256;

/// The kind of work an item is, as classification calls it. Only these
/// are ever looked up: Adult never (Q5), and video, images and other
/// files name no work a database could hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Movie,
    Tv,
    Music,
    Audiobook,
    Ebook,
    Comic,
    Software,
    Game,
}

impl MediaKind {
    pub const ALL: [MediaKind; 8] = [
        MediaKind::Movie,
        MediaKind::Tv,
        MediaKind::Music,
        MediaKind::Audiobook,
        MediaKind::Ebook,
        MediaKind::Comic,
        MediaKind::Software,
        MediaKind::Game,
    ];
}

/// What a plugin is told about one item; nothing else ever leaves the
/// install for a lookup (plan Q5). Build it with [`Query::new`], which
/// bounds every field.
#[derive(Clone, PartialEq, Eq)]
pub struct Query {
    title: String,
    year: Option<u16>,
    kind: MediaKind,
}

/// Lengths only: a title can name what a rule would hold.
impl fmt::Debug for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Query")
            .field("title_len", &self.title.len())
            .field("year", &self.year.is_some())
            .field("kind", &self.kind)
            .finish()
    }
}

impl Query {
    /// The parsed title, trimmed, control characters gone, cut to
    /// [`MAX_QUERY_TITLE`] bytes; a year outside 1–9999 is left out.
    /// `None` when no title is left.
    pub fn new(title: &str, year: Option<u16>, kind: MediaKind) -> Option<Query> {
        let clean: String = title.chars().filter(|c| !c.is_control()).collect();
        let mut t = clean.trim().to_owned();
        truncate(&mut t, MAX_QUERY_TITLE);
        let t = t.trim_end().to_owned();
        (!t.is_empty()).then(|| Query {
            title: t,
            year: year.filter(|y| (1..=9999).contains(y)),
            kind,
        })
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn year(&self) -> Option<u16> {
        self.year
    }

    pub fn kind(&self) -> MediaKind {
        self.kind
    }

    /// Every byte of text the query carries, for tests and audits.
    pub fn text(&self) -> Vec<&str> {
        vec![&self.title]
    }
}

/// What a service resolved an item to. Everything in it is untrusted:
/// [`into_claims`](Identification::into_claims) bounds it.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Identification {
    /// The work's ids, such as `imdb:tt0111161` or `wikidata:Q172241`.
    /// Only the schemes enrichment writes survive, each in its one
    /// spelling (`claims::id_ok`).
    pub ids: Vec<ExternalId>,
    /// The work's own title in the service's database.
    pub canonical_title: Option<String>,
    /// Other titles it's known by, each with its BCP 47 language.
    pub alt_titles: Vec<AltTitle>,
    pub genres: Vec<String>,
    pub year: Option<u16>,
    /// How sure the plugin is of the match, 0–100 percent; more is read as
    /// 100.
    pub confidence: u8,
}

/// Counts only: titles and genres can name what a rule holds.
impl fmt::Debug for Identification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identification")
            .field("ids", &self.ids.len())
            .field("canonical_title", &self.canonical_title.is_some())
            .field("alt_titles", &self.alt_titles.len())
            .field("genres", &self.genres.len())
            .field("year", &self.year)
            .field("confidence", &self.confidence)
            .finish()
    }
}

impl Identification {
    /// The claims the runtime emits for this answer, clamped: the ids the
    /// details rules accept, and a details section that always holds the
    /// match confidence, so it's never empty (ENR-9: a row with ids alone
    /// would store as version 1, with any scheme). No name, description,
    /// publisher, file or tracker: an overview or a poster could name a
    /// person, and the answer is about a stored torrent, not a find.
    pub fn into_claims(self) -> Claims {
        let details = Details {
            canonical_title: self.canonical_title,
            alt_titles: self.alt_titles,
            genres: self.genres,
            year: self.year,
            match_confidence: Some(self.confidence.min(MAX_MATCH_CONFIDENCE)),
            ..Details::default()
        };
        let mut claims = Claims {
            ids: self.ids,
            details: Some(Box::new(details)),
            ..Claims::default()
        };
        claims.clamp();
        debug_assert!(
            claims.details.is_some(),
            "a match confidence always survives the clamp"
        );
        claims
    }
}

/// A plugin's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Match(Identification),
    NoMatch,
}

/// A general enrichment service (ENR-14 and the plugins after it).
/// Compiled in, like every plugin; the runtime runs it as a source of its
/// own, so it's registered, enabled, consented to and purged like any
/// other, and nothing is sent until you enable it (§18.7).
pub trait Identifier: Send + Sync {
    /// Its kinds include [`Kind::Enrichment`](crate::Kind::Enrichment), and
    /// it's never always adult: the runtime refuses one that is.
    fn manifest(&self) -> &Manifest;

    /// Politeness caps for its client, per request.
    fn policy(&self) -> &Policy {
        &Policy::STRICT
    }

    /// The kinds of work it can identify. An item of any other kind is
    /// never sent to it.
    fn kinds(&self) -> &[MediaKind];

    /// The least time between two of its lookups (its documented rate
    /// cap); never below [`MIN_INTERVAL_FLOOR`].
    fn min_interval(&self) -> Duration {
        Duration::from_secs(1)
    }

    /// Looks one item up through `kit`'s client, the only way it may send
    /// anything. It must not keep the query beyond the call.
    fn lookup<'a>(
        &'a self,
        kit: &'a Kit,
        query: &'a Query,
    ) -> BoxFuture<'a, Result<Lookup, PluginError>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claims::{MAX_ALT_TITLES, MAX_IDS, id_ok, language_ok};
    use proptest::prelude::*;

    #[test]
    fn queries_are_bounded_and_never_empty() {
        assert_eq!(Query::new(" \t\u{7} ", Some(2001), MediaKind::Movie), None);
        let q = Query::new(&"é".repeat(MAX_QUERY_TITLE), Some(0), MediaKind::Tv).unwrap();
        assert!(q.title().len() <= MAX_QUERY_TITLE && q.title().chars().all(|c| c == 'é'));
        assert_eq!(q.year(), None, "year 0 is no year");
        let q = Query::new(" A\u{0}B ", Some(1999), MediaKind::Game).unwrap();
        assert_eq!(
            (q.title(), q.year(), q.kind()),
            ("AB", Some(1999), MediaKind::Game)
        );
        assert_eq!(q.text(), vec!["AB"]);
    }

    #[test]
    fn nothing_prints_text() {
        let q = Query::new("Secret Title", Some(2001), MediaKind::Movie).unwrap();
        let i = Identification {
            canonical_title: Some("Secret Canon".into()),
            genres: vec!["secret genre".into()],
            ..Identification::default()
        };
        for s in [format!("{q:?}"), format!("{i:?}")] {
            assert!(!s.contains("Secret") && !s.contains("secret"), "{s}");
        }
    }

    #[test]
    fn an_empty_answer_still_holds_its_confidence() {
        let c = Identification {
            confidence: 250,
            ..Identification::default()
        }
        .into_claims();
        let d = c.details.as_deref().unwrap();
        assert_eq!(d.match_confidence, Some(MAX_MATCH_CONFIDENCE));
        assert!(c.name.is_none() && c.files.is_empty() && c.description.is_none());
    }

    #[test]
    fn only_the_closed_schemes_survive() {
        let id = |s: &str, v: &str| ExternalId {
            scheme: s.into(),
            value: v.into(),
        };
        let c = Identification {
            ids: vec![
                id("imdb", "tt0111161"),
                id("IMDB", "tt0111161"),
                id("btih", "0123456789abcdef0123456789abcdef01234567"),
                id("wikidata", "Q172241"),
                id("tmdb", "not a number"),
            ],
            confidence: 90,
            ..Identification::default()
        }
        .into_claims();
        assert_eq!(
            c.ids,
            vec![id("imdb", "tt0111161"), id("wikidata", "Q172241")]
        );
    }

    fn text() -> impl Strategy<Value = String> {
        prop_oneof![
            ".{0,40}",
            "[a-z ]{0,12}",
            proptest::collection::vec(any::<char>(), 0..300).prop_map(|v| v.into_iter().collect()),
        ]
    }

    fn answer() -> impl Strategy<Value = Identification> {
        (
            proptest::collection::vec(("[a-zA-Z]{0,10}", "[a-zA-Z0-9:-]{0,20}"), 0..40),
            proptest::option::of(text()),
            proptest::collection::vec(("[a-zA-Z-]{0,12}", text()), 0..40),
            proptest::collection::vec(text(), 0..24),
            proptest::option::of(any::<u16>()),
            any::<u8>(),
        )
            .prop_map(|(ids, canonical_title, alt, genres, year, confidence)| {
                Identification {
                    ids: ids
                        .into_iter()
                        .map(|(scheme, value)| ExternalId { scheme, value })
                        .collect(),
                    canonical_title,
                    alt_titles: alt
                        .into_iter()
                        .map(|(language, title)| AltTitle { language, title })
                        .collect(),
                    genres,
                    year,
                    confidence,
                }
            })
    }

    proptest! {
        /// Whatever a plugin answers, its claims are bounded, carry only
        /// the closed schemes and a non-empty section, and never a name,
        /// file, tracker, description or publisher.
        #[test]
        fn any_answer_becomes_bounded_claims(a in answer()) {
            let c = a.clone().into_claims();
            let d = c.details.as_deref().unwrap();
            prop_assert!(!d.is_empty());
            prop_assert_eq!(d.match_confidence, Some(a.confidence.min(MAX_MATCH_CONFIDENCE)));
            prop_assert!(c.ids.len() <= MAX_IDS);
            prop_assert!(c.ids.iter().all(|i| id_ok(&i.scheme, &i.value)));
            prop_assert!(d.alt_titles.len() <= MAX_ALT_TITLES);
            prop_assert!(d.alt_titles.iter().all(|t| language_ok(&t.language) && !t.title.is_empty()));
            prop_assert!(d.year.is_none_or(|y| (1..=9999).contains(&y)));
            prop_assert!(c.name.is_none() && c.size.is_none() && c.files.is_empty());
            prop_assert!(c.trackers.is_empty() && c.description.is_none());
            prop_assert!(c.publisher.is_none() && c.adult.is_none());
            // Clamping again changes nothing: it's in stored form.
            let mut again = c.clone();
            again.clamp();
            prop_assert_eq!(again, c);
        }

        #[test]
        fn any_query_is_bounded(t in text(), y in proptest::option::of(any::<u16>())) {
            if let Some(q) = Query::new(&t, y, MediaKind::Music) {
                prop_assert!(!q.title().is_empty() && q.title().len() <= MAX_QUERY_TITLE);
                prop_assert!(!q.title().chars().any(char::is_control));
                prop_assert!(q.year().is_none_or(|y| (1..=9999).contains(&y)));
            }
        }
    }
}
