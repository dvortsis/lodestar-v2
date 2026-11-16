//! Open Library, a book enrichment plugin (ENR-ol; architecture §18.2,
//! §18.7; plan `docs/plan/2026-11-05-v07-m7.md` Q5, Q12;
//! `docs/decisions/2026-11-15-ENR-ol.md`). Built on Wikidata's pattern
//! (ENR-14, `docs/decisions/2026-11-12-ENR-14.md`).
//!
//! **Off by default, External class.** Nothing is sent until you enable
//! the source and acknowledge its endpoint (§18.7). ENR-10's runtime
//! (`lodestar_pipeline::identify`) decides whether an item may be looked
//! up at all (never a `PRIVACY`, held, hidden or Adult item), paces the
//! lookups, caches the answers and sends them through the writer's judge
//! before anything is stored. This plugin only answers one [`Query`] of
//! an ebook or audiobook, with one request:
//!
//! 1. `search.json` for the query's title, and nothing else from the item:
//!    the book's own title ([`book_title`], never an `Author - ` before
//!    it), sent [`fold`]ed (lower case, letters, digits and single
//!    spaces), as one URL parameter, so no character of it can reach the
//!    service's query syntax; at most [`SEARCH_LIMIT`] works, with the
//!    fixed [`FIELDS`] only;
//! 2. picks the one work whose title (or an alternative title) and year
//!    agree with the query ([`choose`]). No work, or more than one, is no
//!    match: it never guesses.
//!
//! The request carries a [`USER_AGENT`] naming the project, never the
//! user or the install. Errors read the HTTP status and the parser's
//! report only: the service's text is never kept or put in an error.

use std::sync::Mutex;
use std::time::Duration;

use lodestar_sources::claims::{AltTitle, ExternalId};
use lodestar_sources::http::parse_retry_after;
use lodestar_sources::identify::{Identification, Identifier, Lookup, MediaKind, Query};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Consent, Endpoint, Endpoints, HostRate, HttpError, Kind, Kit,
    Manifest, Network, PluginError, Policy, Priority, Request, Response, Risk,
};
use lodestar_sources_feeds::openlibrary::{self, Page, SEARCH_LIMITS, Work};
use tokio::time::Instant;

/// The plugin's id in the store. Never rename.
pub const ID: &str = "openlibrary";
/// The only host the plugin may reach.
pub const HOST: &str = "openlibrary.org";
/// The search API's path on it.
pub const API: &str = "/search.json";
/// Works asked for in a search. The match must be among the top few.
pub const SEARCH_LIMIT: usize = 10;
/// The only fields asked for: the work's id, titles and years. Never an
/// author, a subject or an edition.
pub const FIELDS: &str = "key,title,alternative_title,first_publish_year,publish_year";
/// Open Library asks clients to name themselves: the client, its version
/// and the project's own URL, the same for every install. Nothing about
/// the user, their host or their account.
pub const USER_AGENT: &str = concat!(
    "Lodestar/",
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CARGO_PKG_REPOSITORY"),
    ") lodestar-sources-plugins"
);

/// How sure a match is: the title and the year agree.
pub const CONFIDENCE_WITH_YEAR: u8 = 80;
/// The title agrees and the query has no year. Lower than Wikidata's:
/// with no author, many books share a title.
pub const CONFIDENCE_WITHOUT_YEAR: u8 = 60;
/// The language of an alternative title, which Open Library doesn't give.
pub const UNKNOWN_LANGUAGE: &str = "und";

pub(crate) const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "Open Library",
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

/// Open Library's limit for clients: one request a second (three for one
/// that names an email, which this never sends). The plan's cap is one a
/// second (§2.26), with no burst. Back-off after a 429 or 5xx: a minute
/// up to 6 hours.
const POLICY: Policy = Policy {
    caps: Caps {
        per_host: HostRate::per_second(1),
        backoff: Backoff {
            base: Duration::from_secs(60),
            max: Duration::from_secs(6 * 3600),
        },
        ..Caps::STRICT
    },
    ..Policy::STRICT
};

/// The least time between two lookups: one request at one a second.
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// The service asked for slower requests (429).
pub const RATE_LIMITED: &str = "Open Library is limiting requests; backing off";
/// The host is held after a rate limit, a server error or an API error.
pub const HELD: &str = "Open Library requests are on hold; backing off";
/// The answer was an error object.
pub const API_ERROR: &str = "Open Library answered with an error; trying later";
/// The wait after an API error that names none (`Retry-After`).
pub const API_ERROR_WAIT: Duration = Duration::from_secs(60);
/// The longest wait an API error's `Retry-After` sets.
pub const MAX_API_ERROR_WAIT: Duration = Duration::from_secs(3600);
const BAD_ANSWER: &str = "Open Library's answer wasn't understood";
const FAILED: &str = "Open Library's request failed";
const BAD_URL: &str = "Open Library's address didn't build";

/// The Open Library plugin. It holds no credential; its one state is the
/// wait after an API error.
#[derive(Debug, Default)]
pub struct OpenLibrary {
    not_before: Mutex<Option<Instant>>,
}

impl OpenLibrary {
    pub fn new() -> OpenLibrary {
        OpenLibrary::default()
    }

    /// Where the plugin's egress may send requests (SRC-5): the manifest's
    /// host only, over https, public addresses only.
    pub fn endpoints(&self) -> Endpoints {
        Endpoints::manifest(MANIFEST.endpoints)
    }
}

impl Identifier for OpenLibrary {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn policy(&self) -> &Policy {
        &POLICY
    }

    fn kinds(&self) -> &[MediaKind] {
        &[MediaKind::Ebook, MediaKind::Audiobook]
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
            // A title of nothing but punctuation can name no work: nothing
            // is sent.
            let want = fold(book_title(query.title()));
            if want.is_empty() {
                return Ok(Lookup::NoMatch);
            }
            self.not_held()?;
            let res = self.answer(kit.http.send(&search_request(&want)?).await)?;
            let found = openlibrary::parse_search(&res.body, &SEARCH_LIMITS);
            self.usable(&found.page, &res)?;
            let works: Vec<&Work> = found.works.iter().collect();
            Ok(choose(query, &works))
        })
    }
}

impl OpenLibrary {
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
    /// ([`Page::whole`]), so a broken one is never cached as a miss. An
    /// error object on a 200, which the host limiter doesn't see, holds
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

/// The search for `title`, which the caller has already [`fold`]ed: the
/// only item data in it, as one encoded parameter.
pub fn search_request(title: &str) -> Result<Request, PluginError> {
    let mut url = url::Url::parse(&format!("https://{HOST}{API}")).map_err(|_| fail(BAD_URL))?;
    url.query_pairs_mut()
        .append_pair("title", title)
        .append_pair("fields", FIELDS)
        .append_pair("limit", &SEARCH_LIMIT.to_string());
    Ok(Request::get(url.as_str())
        .header("Accept", "application/json")
        .header("User-Agent", USER_AGENT))
}

/// The one work that agrees with the query: its title or an alternative
/// title equal to the query's [`book_title`] (compared by [`fold`]), and, when the query
/// has a year, that year as its first publication or among its
/// publication years. None, or more than one, is no match.
pub fn choose(query: &Query, works: &[&Work]) -> Lookup {
    let want = fold(book_title(query.title()));
    if want.is_empty() {
        return Lookup::NoMatch;
    }
    let mut agree = works.iter().filter(|w| {
        w.title
            .iter()
            .chain(&w.alternatives)
            .any(|t| fold(t) == want)
            && query
                .year()
                .is_none_or(|y| w.first_year == Some(y) || w.years.contains(&y))
    });
    match (agree.next(), agree.next()) {
        (Some(w), None) => Lookup::Match(identification(query, w)),
        _ => Lookup::NoMatch,
    }
}

/// The book's own title in a parsed one. The release parser keeps a
/// book's `Author - Title` together as its title, so the part after the
/// first ` - ` is the book's, when there's one with a letter or digit in
/// it. The author's name, a person's, is then never sent.
pub fn book_title(title: &str) -> &str {
    match title.split_once(" - ") {
        Some((_, rest)) if !fold(rest).is_empty() => rest,
        _ => title,
    }
}

/// A title for comparing and sending: lower case, letters and digits
/// only, each run of anything else one space.
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

/// The answer for `w`: its Open Library work id, its title as the
/// canonical one and its other titles as alternatives (in no known
/// language), and the query's year, else its first publication year.
fn identification(query: &Query, w: &Work) -> Identification {
    let canonical = w.title.clone();
    let mut alt_titles: Vec<AltTitle> = Vec::new();
    for t in &w.alternatives {
        if canonical.as_ref() != Some(t) && !alt_titles.iter().any(|a| a.title == *t) {
            alt_titles.push(AltTitle {
                language: UNKNOWN_LANGUAGE.to_owned(),
                title: t.clone(),
            });
        }
    }
    Identification {
        ids: vec![ExternalId {
            scheme: "olid".into(),
            value: w.id.to_string(),
        }],
        canonical_title: canonical,
        alt_titles,
        genres: Vec::new(),
        year: query.year().or(w.first_year),
        confidence: if query.year().is_some() {
            CONFIDENCE_WITH_YEAR
        } else {
            CONFIDENCE_WITHOUT_YEAR
        },
    }
}
