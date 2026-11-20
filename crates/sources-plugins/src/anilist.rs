//! AniList, an anime enrichment plugin (ENR-anilist; architecture §18.2,
//! §18.7; plan `docs/plan/2026-11-05-v07-m7.md` Q5, Q12;
//! `docs/decisions/2026-11-15-ENR-anilist.md`). Built on Wikidata's
//! pattern (ENR-14, `docs/decisions/2026-11-12-ENR-14.md`).
//!
//! **Off by default, External class.** Nothing is sent until you enable
//! the source and acknowledge its endpoint (§18.7). ENR-10's runtime
//! (`lodestar_pipeline::identify`) decides whether an item may be looked
//! up at all (never a `PRIVACY`, held, hidden or Adult item), paces the
//! lookups, caches the answers and sends them through the writer's judge
//! before anything is stored. This plugin only answers one [`Query`] of a
//! film or a series, with one request:
//!
//! 1. a GraphQL `POST` of the fixed document [`SEARCH_ANIME`], with the
//!    query's title as the one text variable (`search`), serialized by
//!    `serde_json`, never written into the document; `isAdult` is always
//!    `false` and `perPage` is [`SEARCH_LIMIT`];
//! 2. picks the one media that isn't adult, whose format fits the query's
//!    kind, whose title (any of them) agrees with the query's and whose
//!    year agrees ([`choose`]). None, or more than one, is no match: it
//!    never guesses.
//!
//! Errors read the HTTP status and the parser's report only: the
//! service's text is never kept or put in an error.

use std::sync::Mutex;
use std::time::Duration;

use lodestar_sources::claims::{AltTitle, ExternalId};
use lodestar_sources::http::{Method, parse_retry_after};
use lodestar_sources::identify::{
    Attribution, Identification, Identifier, Lookup, MediaKind, Query, Retention, Terms,
};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Consent, Endpoint, Endpoints, HostRate, HttpError, Kind, Kit,
    Manifest, Network, PluginError, Policy, Priority, Request, Response, Risk,
};
pub use lodestar_sources_feeds::anilist::SEARCH_ANIME;
use lodestar_sources_feeds::anilist::{self, Format, Media, Page, SEARCH_LIMITS};
use serde::Serialize;
use tokio::time::Instant;

/// The plugin's id in the store. Never rename.
pub const ID: &str = "anilist";
/// The only host the plugin may reach.
pub const HOST: &str = "graphql.anilist.co";
/// The GraphQL endpoint: the host's root.
pub const ENDPOINT: &str = "https://graphql.anilist.co";
/// Media asked for in a search. The match must be among the top few.
pub const SEARCH_LIMIT: u8 = 10;

/// What a lookup sends, for its Sources row (UI review 02 §5.5).
pub const SENDS: &str = "The parsed title of a film or a TV series, to graphql.anilist.co, \
     asking for anime that aren't marked adult. Never the year, the info hash, the \
     torrent's name or its files.";

/// How sure a match is: the title and the year agree.
pub const CONFIDENCE_WITH_YEAR: u8 = 90;
/// The title agrees and the query has no year.
pub const CONFIDENCE_WITHOUT_YEAR: u8 = 70;

pub(crate) const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "AniList",
    version: "1",
    kinds: &[Kind::Enrichment],
    networks: &[Network::Bt],
    endpoints: &[Endpoint {
        protocol: "https",
        host: HOST,
    }],
    risk: Risk::Low,
    consent: Consent::External,
    default_priority: Priority::Low,
    enabled_by_default: false,
    always_adult: false,
    finds_private: false,
};

/// AniList documents 90 requests a minute, 30 while degraded, with a
/// burst limiter on top. The plan's cap is 30 a minute (§2.26): one
/// request every two seconds, with no burst. Back-off after a 429 or 5xx:
/// a minute up to 6 hours.
const POLICY: Policy = Policy {
    caps: Caps {
        per_host: HostRate::every(MIN_INTERVAL),
        backoff: Backoff {
            base: Duration::from_secs(60),
            max: Duration::from_secs(6 * 3600),
        },
        ..Caps::STRICT
    },
    ..Policy::STRICT
};

/// The least time between two lookups: one request at 30 a minute.
pub const MIN_INTERVAL: Duration = Duration::from_secs(2);

/// The service asked for slower requests (429).
pub const RATE_LIMITED: &str = "AniList is limiting requests; backing off";
/// The host is held after a rate limit, a server error or an API error.
pub const HELD: &str = "AniList requests are on hold; backing off";
/// The answer was a GraphQL error.
pub const API_ERROR: &str = "AniList answered with an error; trying later";
/// The wait after an API error that names none (`Retry-After`).
pub const API_ERROR_WAIT: Duration = Duration::from_secs(60);
/// The longest wait an API error's `Retry-After` sets.
pub const MAX_API_ERROR_WAIT: Duration = Duration::from_secs(3600);
const BAD_ANSWER: &str = "AniList's answer wasn't understood";
const FAILED: &str = "AniList's request failed";
const BAD_REQUEST: &str = "AniList's request didn't build";

/// The AniList plugin. It holds no credential; its one state is the wait
/// after an API error.
#[derive(Debug, Default)]
pub struct AniList {
    not_before: Mutex<Option<Instant>>,
}

impl AniList {
    pub fn new() -> AniList {
        AniList::default()
    }

    /// Where the plugin's egress may send requests (SRC-5): the manifest's
    /// host only, over https, public addresses only.
    pub fn endpoints(&self) -> Endpoints {
        Endpoints::manifest(MANIFEST.endpoints)
    }
}

/// AniList's terms (ENR-terms; `docs/decisions/2026-11-15-ENR-anilist.md`).
/// Its terms of use forbid hoarding, mass collection and using the API as
/// a data store, and grant no licence to pass its data on, so: no
/// whole-index walk, only the items you open, and its answers stay in
/// Lodestar's own UI. Claims are refreshed and dropped within 180 days,
/// so nothing is kept as a store. It asks for no credit; the line is a
/// courtesy.
pub const TERMS: Terms = Terms {
    background_walk: false,
    redistribute: false,
    retention: Retention::DEFAULT,
    attribution: Some(Attribution {
        text: "AniList",
        link: "https://anilist.co",
        logo: None,
    }),
    // AniList marks every media `isAdult`: an answer without the marker
    // is refused (the plugin never chooses one anyway).
    adult_marker_required: true,
};

impl Identifier for AniList {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn terms(&self) -> &Terms {
        &TERMS
    }

    fn policy(&self) -> &Policy {
        &POLICY
    }

    fn kinds(&self) -> &[MediaKind] {
        &[MediaKind::Movie, MediaKind::Tv]
    }

    fn min_interval(&self) -> Duration {
        MIN_INTERVAL
    }

    fn lookup<'a>(
        &'a self,
        kit: &'a Kit,
        query: &'a Query,
    ) -> BoxFuture<'a, Result<Lookup, PluginError>> {
        Box::pin(async move {
            // A title of nothing but punctuation, or a kind AniList has no
            // format for, can name no anime: nothing is sent.
            if fold(query.title()).is_empty() || !self.kinds().contains(&query.kind()) {
                return Ok(Lookup::NoMatch);
            }
            self.not_held()?;
            let res = self.answer(kit.http.send(&search_request(query.title())?).await)?;
            let found = anilist::parse_search(&res.body, &SEARCH_LIMITS);
            self.usable(&found.page, &res)?;
            let media: Vec<&Media> = found.media.iter().collect();
            Ok(choose(query, &media))
        })
    }
}

impl AniList {
    /// Fails at once, sending nothing, while an API error's wait lasts.
    fn not_held(&self) -> Result<(), PluginError> {
        let until = *self.not_before.lock().unwrap_or_else(|e| e.into_inner());
        match until {
            Some(t) if Instant::now() < t => Err(fail(HELD)),
            _ => Ok(()),
        }
    }

    /// A 200, or the error its status means.
    fn answer(&self, res: Result<Response, HttpError>) -> Result<Response, PluginError> {
        let res = match res {
            Ok(r) => r,
            // The limiter holds the host after a 429 or 5xx; the retry
            // waits.
            Err(HttpError::Blocked { .. }) => return Err(fail(HELD)),
            Err(_) => return Err(fail(FAILED)),
        };
        match res.status {
            200 => Ok(res),
            429 => Err(fail(RATE_LIMITED)),
            _ => Err(fail(FAILED)),
        }
    }

    /// Only a whole answer is used, as a match or a miss
    /// ([`Page::whole`]), so a broken one is never cached as a miss. A
    /// GraphQL error on a 200, which the host limiter doesn't see, holds
    /// the plugin for its `Retry-After`, or [`API_ERROR_WAIT`], at most
    /// [`MAX_API_ERROR_WAIT`].
    fn usable(&self, page: &Page, res: &Response) -> Result<(), PluginError> {
        if page.error {
            let wait = res
                .header("retry-after")
                .and_then(parse_retry_after)
                .unwrap_or(API_ERROR_WAIT)
                .clamp(MIN_INTERVAL, MAX_API_ERROR_WAIT);
            *self.not_before.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Instant::now() + wait);
            return Err(fail(API_ERROR));
        }
        if !page.whole() {
            return Err(fail(BAD_ANSWER));
        }
        Ok(())
    }
}

fn fail(msg: &str) -> PluginError {
    PluginError(msg.to_owned())
}

/// The GraphQL request body: the fixed document and its variables.
#[derive(Serialize)]
struct Body<'a> {
    query: &'static str,
    variables: Variables<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Variables<'a> {
    search: &'a str,
    per_page: u8,
    is_adult: bool,
}

/// The search for `title`: the fixed [`SEARCH_ANIME`] document, and the
/// title as the `search` variable, the only item data in the request.
/// `isAdult` is always `false`.
pub fn search_request(title: &str) -> Result<Request, PluginError> {
    let body = serde_json::to_vec(&Body {
        query: SEARCH_ANIME,
        variables: Variables {
            search: title,
            per_page: SEARCH_LIMIT,
            is_adult: false,
        },
    })
    .map_err(|_| fail(BAD_REQUEST))?;
    Ok(Request {
        method: Method::Post,
        url: ENDPOINT.to_owned(),
        headers: vec![
            ("Accept".to_owned(), "application/json".to_owned()),
            ("Content-Type".to_owned(), "application/json".to_owned()),
        ],
        body,
    })
}

/// Whether a media's format is the query's kind: a film is a `MOVIE`; a
/// series is `TV`, `TV_SHORT`, `OVA`, `ONA` or `SPECIAL`. Music videos,
/// print formats and formats this doesn't know never fit.
pub fn fits(kind: MediaKind, format: Option<Format>) -> bool {
    match kind {
        MediaKind::Movie => format == Some(Format::Movie),
        MediaKind::Tv => matches!(
            format,
            Some(Format::Tv | Format::TvShort | Format::Ova | Format::Ona | Format::Special)
        ),
        _ => false,
    }
}

/// The one media that agrees with the query: marked not adult (an adult
/// or unmarked one is never chosen, whatever was asked), its format
/// [`fits`] the kind, one of its titles equal to the query's (compared
/// by [`fold`]), and, when the query has a year, that year as its start
/// or season year. None, or more than one, is no match.
pub fn choose(query: &Query, media: &[&Media]) -> Lookup {
    let want = fold(query.title());
    if want.is_empty() {
        return Lookup::NoMatch;
    }
    let mut agree = media.iter().filter(|m| {
        m.adult == Some(false)
            && fits(query.kind(), m.format)
            && m.titles().any(|t| fold(t) == want)
            && query
                .year()
                .is_none_or(|y| m.start_year == Some(y) || m.season_year == Some(y))
    });
    match (agree.next(), agree.next()) {
        (Some(m), None) => Lookup::Match(identification(query, m)),
        _ => Lookup::NoMatch,
    }
}

/// A title for comparing: lower case, letters and digits only, each run
/// of anything else one space.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with(' ') && !out.is_empty() {
            out.push(' ');
        }
    }
    out.trim_end().to_owned()
}

/// The BCP 47 language of a native title, from the country of origin;
/// `und` for any other.
fn native_language(country: Option<&str>) -> &'static str {
    match country {
        Some("JP") => "ja",
        Some("KR") => "ko",
        Some("CN" | "TW") => "zh",
        _ => "und",
    }
}

/// The answer for `m`: its AniList id; its English title as the canonical
/// one, else its romaji, else its native; its other titles as
/// alternatives (romaji in Latin script of the native language, English
/// `en`, native by the country of origin, synonyms `und`); and the
/// query's year, else its start year.
fn identification(query: &Query, m: &Media) -> Identification {
    let canonical = m
        .english
        .clone()
        .or_else(|| m.romaji.clone())
        .or_else(|| m.native.clone());
    let native = native_language(m.country.as_deref());
    let romaji = if native == "und" {
        "und-latn".to_owned()
    } else {
        format!("{native}-latn")
    };
    let titled = [
        (romaji.as_str(), m.romaji.as_ref()),
        ("en", m.english.as_ref()),
        (native, m.native.as_ref()),
    ];
    let synonyms = m.synonyms.iter().map(|t| ("und", Some(t)));
    let mut alt_titles: Vec<AltTitle> = Vec::new();
    for (language, t) in titled.into_iter().chain(synonyms) {
        let Some(t) = t else { continue };
        if canonical.as_ref() != Some(t) && !alt_titles.iter().any(|a| a.title == *t) {
            alt_titles.push(AltTitle {
                language: language.to_owned(),
                title: t.clone(),
            });
        }
    }
    Identification {
        ids: vec![ExternalId {
            scheme: "anilist".into(),
            value: m.id.to_string(),
        }],
        canonical_title: canonical,
        alt_titles,
        genres: Vec::new(),
        year: query.year().or(m.start_year).or(m.season_year),
        confidence: if query.year().is_some() {
            CONFIDENCE_WITH_YEAR
        } else {
            CONFIDENCE_WITHOUT_YEAR
        },
        // AniList's own `isAdult`, passed on: always `Some(false)` here,
        // since [`choose`] takes nothing else.
        adult: m.adult,
    }
}

#[cfg(test)]
mod terms_tests {
    use super::*;

    /// No mass collection: opened items only, kept in Lodestar's own UI,
    /// refreshed within 180 days, credited as a courtesy, its marker
    /// required.
    #[test]
    fn anilist_terms() {
        let t = AniList::new();
        let t = Identifier::terms(&t);
        assert!(!t.background_walk && !t.redistribute && t.adult_marker_required);
        assert_eq!(t.retention, Retention::DEFAULT);
        assert_eq!(t.attribution.map(|a| a.link), Some("https://anilist.co"));
        assert!(t.accepts(Some(false)) && !t.accepts(None) && !t.accepts(Some(true)));
    }
}
