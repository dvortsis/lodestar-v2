//! IGDB (the Internet Game Database, run by Twitch), a general enrichment
//! plugin for games (ENR-igdb; architecture §18.2, §18.7;
//! `docs/decisions/2026-11-16-ENR-igdb.md`). TMDB's shape (ENR-tmdb).
//!
//! **Off by default, Account class.** It needs your own Twitch
//! application's Client ID and Client secret, stored in the credential
//! store (SRC-4) and read on each lookup. They are exchanged at
//! `id.twitch.tv` for an app access token (OAuth client credentials),
//! which lives only in this plugin's memory, as a [`Secret`], and is
//! never logged, stored or put in a URL or an error. ENR-10's runtime
//! decides whether an item may be looked up at all, paces the lookups,
//! caches the answers and sends them through the writer's judge. This
//! plugin only answers one [`Query`]:
//!
//! 1. when it holds no token for the stored Client ID, or the one it holds
//!    is about to expire, `POST https://id.twitch.tv/oauth2/token` with the
//!    two credentials in a form body (never the URL);
//! 2. `POST https://api.igdb.com/v4/games` with a fixed Apicalypse body,
//!    the title its one string literal, with any `"` or `\` dropped;
//! 3. picks the one game whose title (and year, when the query has one)
//!    agrees ([`choose`]). None, or more than one, is no match.
//!
//! A 401 or 403 from IGDB drops the token, and the limiter holds IGDB's
//! host for the policy's longest back-off, 6 hours; the first lookup after
//! that asks Twitch for a new one. So a revoked token (Twitch revokes app
//! tokens when the secret is regenerated, and sometimes on its own) stops
//! IGDB lookups for 6 hours. That is intended: the lookup is never retried
//! with a new token. A 400, 401 or 403 from Twitch holds its host for the
//! same 6 hours. One lookup sends at most one token request and one
//! search, and only one lookup at a time asks for a token, so a refused
//! token never loops.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_integrations::secret::Secret;
use lodestar_sources::claims::ExternalId;
use lodestar_sources::consent::{CredentialKind, CredentialSpec, Need};
use lodestar_sources::http::Method;
use lodestar_sources::identify::{
    Attribution, Identification, Identifier, Lookup, MediaKind, Query, Retention, Terms,
};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Consent, Endpoint, Endpoints, HostRate, HttpError, Kind, Kit,
    Manifest, Network, PluginError, Policy, Priority, Request, Risk,
};
use lodestar_sources_feeds::igdb::{self, Hit, LIMITS, TOKEN_LIMITS};
use tokio::time::Instant;

use crate::Credentials;

/// The plugin's id in the store. Never rename.
pub const ID: &str = "igdb";
/// The credential that holds the Twitch application's Client ID.
pub const CLIENT_ID: &str = "client_id";
/// The credential that holds the Twitch application's Client secret.
pub const CLIENT_SECRET: &str = "client_secret";
/// IGDB's API host.
pub const HOST: &str = "api.igdb.com";
/// Twitch's OAuth host, for the access token.
pub const TOKEN_HOST: &str = "id.twitch.tv";
/// Bytes in a Client ID or secret. Twitch's are 30.
pub const MAX_CREDENTIAL: usize = 128;
/// Bytes of the title put in the query; longer is cut on a character
/// boundary.
pub const MAX_QUERY_TITLE: usize = 255;
/// A token is renewed this long before Twitch says it expires.
pub const RENEW_EARLY: Duration = Duration::from_secs(300);

/// What a lookup sends, for its Sources row (UI review 02 §5.5).
pub const SENDS: &str = "The parsed title of a game, with your Twitch Client ID and an \
     access token, to api.igdb.com; your Client ID and secret to id.twitch.tv for that token. \
     Never the info hash, the torrent's name or its files.";
/// The credit IGDB's terms ask for.
pub const ATTRIBUTION: &str = "Game data from IGDB.com.";
/// Where the attribution links.
pub const ATTRIBUTION_URL: &str = "https://www.igdb.com";

/// How sure a match is: the title and the year agree.
pub const CONFIDENCE_WITH_YEAR: u8 = 90;
/// The title agrees and the query has no year.
pub const CONFIDENCE_WITHOUT_YEAR: u8 = 70;

pub(crate) const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "IGDB",
    version: "1",
    kinds: &[Kind::Enrichment],
    networks: &[Network::Bt],
    endpoints: &[
        Endpoint {
            protocol: "https",
            host: HOST,
        },
        Endpoint {
            protocol: "https",
            host: TOKEN_HOST,
        },
    ],
    risk: Risk::Low,
    consent: Consent::Account,
    default_priority: Priority::Low,
    enabled_by_default: false,
    always_adult: false,
    finds_private: false,
};

/// No settings of its own: the credentials are declared, never settings.
pub const CONFIG_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{},"additionalProperties":false}"#;

/// IGDB allows 4 requests a second with 8 open; this asks at most one a
/// second a host, with no burst. Back-off after a 429 or 5xx: a minute up
/// to 6 hours. A refusal (a 401 or 403, or a 400 from Twitch) holds the
/// host for the 6 hours at once.
const POLICY: Policy = Policy {
    caps: Caps {
        per_host: HostRate::per_second(1),
        backoff: Backoff {
            base: Duration::from_secs(60),
            max: Duration::from_secs(6 * 3600),
        },
        ..Caps::STRICT
    },
    config_schema: CONFIG_SCHEMA,
    credentials: &[
        CredentialSpec {
            name: CLIENT_ID,
            label: "Twitch Client ID",
            kind: CredentialKind::Secret,
            required: true,
        },
        CredentialSpec {
            name: CLIENT_SECRET,
            label: "Twitch Client secret",
            kind: CredentialKind::Secret,
            required: true,
        },
    ],
    ..Policy::STRICT
};

/// The least time between two lookups.
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// IGDB refused the access token (401 or 403). Names no token.
pub const REFUSED: &str = "IGDB refused the access token";
/// Twitch refused the Client ID or secret.
pub const CREDENTIALS_REFUSED: &str = "Twitch refused the Client ID or secret";
/// The service asked for slower requests (429).
pub const RATE_LIMITED: &str = "IGDB is limiting requests; backing off";
/// The host is held after a refusal, a rate limit or a server error.
pub const HELD: &str = "IGDB requests are on hold; backing off";
/// A credential isn't stored.
pub const NO_CREDENTIALS: &str = "needs setup: no Twitch Client ID and secret are stored";
const BAD_CREDENTIALS: &str = "the stored Twitch Client ID or secret isn't valid";
const UNREADABLE: &str = "the stored Twitch Client ID or secret couldn't be read";
const BAD_ANSWER: &str = "IGDB's answer wasn't understood";
const BAD_TOKEN_ANSWER: &str = "Twitch's token answer wasn't understood";
const FAILED: &str = "IGDB's request failed";
const TOKEN_FAILED: &str = "Twitch's token request failed";

/// A token Twitch gave for one Client ID.
struct Held {
    client_id: Secret,
    token: Secret,
    renew_at: Instant,
}

/// The IGDB plugin. It holds no credential: it reads them on each lookup.
/// It holds the access token in memory only.
pub struct Igdb {
    credentials: Arc<dyn Credentials>,
    token: Mutex<Option<Held>>,
    /// Held across a token request, so only one lookup asks at a time.
    fetch: tokio::sync::Mutex<()>,
}

impl fmt::Debug for Igdb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Igdb").finish_non_exhaustive()
    }
}

impl Igdb {
    pub fn new(credentials: Arc<dyn Credentials>) -> Igdb {
        Igdb {
            credentials,
            token: Mutex::new(None),
            fetch: tokio::sync::Mutex::new(()),
        }
    }

    /// Where the plugin's egress may send requests (SRC-5): the manifest's
    /// two hosts only, over https, public addresses only.
    pub fn endpoints(&self) -> Endpoints {
        Endpoints::manifest(MANIFEST.endpoints)
    }

    /// What enabling the plugin still needs, given the names of the
    /// credentials stored for it (Account class).
    pub fn need(stored: &[String]) -> Option<Need> {
        lodestar_sources::consent::need_with(&MANIFEST, POLICY.credentials, None, stored)
    }

    fn held(&self) -> std::sync::MutexGuard<'_, Option<Held>> {
        self.token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The held token for `client_id`, unless it's due for renewal.
    fn cached(&self, client_id: &Secret) -> Option<Secret> {
        let held = self.held();
        held.as_ref()
            .filter(|h| h.client_id.expose() == client_id.expose() && Instant::now() < h.renew_at)
            .map(|h| Secret::new(h.token.expose()))
    }

    /// A token for `client_id`: the held one, or a new one from Twitch.
    async fn token(
        &self,
        kit: &Kit,
        client_id: &Secret,
        secret: &Secret,
    ) -> Result<Secret, PluginError> {
        if let Some(t) = self.cached(client_id) {
            return Ok(t);
        }
        // One token request at a time. A lookup that waited here takes the
        // token the one before it got, or stops if Twitch refused it.
        let _fetching = self.fetch.lock().await;
        if let Some(t) = self.cached(client_id) {
            return Ok(t);
        }
        if blocked(kit, TOKEN_HOST) {
            return Err(fail(HELD));
        }
        let res = kit.http.send(&token_request(client_id, secret)).await;
        let res = match res {
            Ok(r) => r,
            Err(HttpError::Blocked { .. }) => return Err(fail(HELD)),
            Err(_) => return Err(fail(TOKEN_FAILED)),
        };
        match res.status {
            200 => {}
            400 | 401 | 403 => {
                // Twitch answers a wrong Client ID with a 400, which the
                // limiter counts as an answer, not a refusal. Hold the
                // token host for the policy's longest back-off for all
                // three, as the limiter's own 401 rule does. A block only
                // extends: a 401 or 403 the limiter already held isn't
                // counted again.
                let now = Instant::now();
                kit.http
                    .limiter()
                    .block(TOKEN_HOST, now + POLICY.caps.backoff.max, now);
                return Err(fail(CREDENTIALS_REFUSED));
            }
            429 => return Err(fail(RATE_LIMITED)),
            _ => return Err(fail(TOKEN_FAILED)),
        }
        let t =
            igdb::parse_token(&res.body, &TOKEN_LIMITS).ok_or_else(|| fail(BAD_TOKEN_ANSWER))?;
        // Renew early, but never sooner than half the token's life, so a
        // short-lived token is still reused.
        let full = Duration::from_secs(t.expires_in);
        let life = full.saturating_sub(RENEW_EARLY).max(full / 2);
        let token = Secret::new(t.access_token);
        *self.held() = Some(Held {
            client_id: Secret::new(client_id.expose()),
            token: Secret::new(token.expose()),
            renew_at: Instant::now() + life,
        });
        Ok(token)
    }

    fn drop_token(&self) {
        *self.held() = None;
    }
}

/// IGDB's terms (ENR-terms; `docs/decisions/2026-11-16-ENR-igdb.md`): the
/// Twitch Developer Services Agreement IGDB's API is under. No
/// whole-index walk, answers kept in Lodestar's own UI, the default
/// retention, and IGDB credited with a link. IGDB marks adult games only
/// by its Erotic theme and has no "not adult" marker, so the marker isn't
/// required; a game with that theme is marked adult and refused.
pub const TERMS: Terms = Terms {
    background_walk: false,
    redistribute: false,
    retention: Retention::DEFAULT,
    attribution: Some(Attribution {
        text: ATTRIBUTION,
        link: ATTRIBUTION_URL,
        logo: None,
    }),
    adult_marker_required: false,
};

impl Identifier for Igdb {
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
        &[MediaKind::Game]
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
            if query.kind() != MediaKind::Game || fold(query.title()).is_empty() {
                return Ok(Lookup::NoMatch);
            }
            // Both read before either is checked, so a lookup reads
            // exactly what the policy declares.
            let id = self.credentials.get(ID, CLIENT_ID).await;
            let secret = self.credentials.get(ID, CLIENT_SECRET).await;
            let (id, secret) = match (id, secret) {
                (Ok(Some(i)), Ok(Some(s))) => (i, s),
                (Err(_), _) | (_, Err(_)) => return Err(fail(UNREADABLE)),
                _ => return Err(fail(NO_CREDENTIALS)),
            };
            if !valid_credential(id.expose()) || !valid_credential(secret.expose()) {
                return Err(fail(BAD_CREDENTIALS));
            }
            // While IGDB's host is held (after a 401, 403 or 429), no new
            // token is asked for either: the search would be refused. While
            // Twitch's is held (refused credentials), nothing is sent.
            if [HOST, TOKEN_HOST].into_iter().any(|h| blocked(kit, h)) {
                return Err(fail(HELD));
            }
            let token = self.token(kit, &id, &secret).await?;
            drop(secret);
            let request = search_request(query, &id, &token);
            drop(token);
            let res = match kit.http.send(&request).await {
                Ok(r) => r,
                Err(HttpError::Blocked { .. }) => return Err(fail(HELD)),
                Err(_) => return Err(fail(FAILED)),
            };
            match res.status {
                200 => {}
                401 | 403 => {
                    // The next lookup asks for a new token, once.
                    self.drop_token();
                    return Err(fail(REFUSED));
                }
                429 => return Err(fail(RATE_LIMITED)),
                _ => return Err(fail(FAILED)),
            }
            let found = igdb::parse_games(&res.body, &LIMITS);
            if !found.page.whole() {
                return Err(fail(BAD_ANSWER));
            }
            Ok(choose(query, &found.hits))
        })
    }
}

/// Whether the limiter holds `host` after a refusal.
fn blocked(kit: &Kit, host: &str) -> bool {
    matches!(
        kit.http.limiter().state(host, Instant::now()),
        lodestar_sources::HostState::Blocked { .. }
    )
}

fn fail(msg: &str) -> PluginError {
    PluginError(msg.to_owned())
}

/// A Client ID or secret goes into a header or a form: ASCII letters and
/// digits only.
fn valid_credential(c: &str) -> bool {
    (1..=MAX_CREDENTIAL).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Twitch's client-credentials request: the two credentials in a form
/// body, never the URL.
pub fn token_request(client_id: &Secret, secret: &Secret) -> Request {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", client_id.expose())
        .append_pair("client_secret", secret.expose())
        .append_pair("grant_type", "client_credentials")
        .finish();
    Request {
        method: Method::Post,
        url: format!("https://{TOKEN_HOST}/oauth2/token"),
        headers: vec![
            ("Accept".to_owned(), "application/json".to_owned()),
            (
                "Content-Type".to_owned(),
                "application/x-www-form-urlencoded".to_owned(),
            ),
        ],
        body: body.into_bytes(),
    }
}

/// The games search for `query`: a fixed Apicalypse body whose only item
/// data is the title, as one string literal ([`literal`]).
pub fn search_request(query: &Query, client_id: &Secret, token: &Secret) -> Request {
    let body = format!(
        "search \"{}\"; fields name,first_release_date,themes,genres; limit 20;",
        literal(query.title())
    );
    Request {
        method: Method::Post,
        url: format!("https://{HOST}/v4/games"),
        headers: vec![
            ("Accept".to_owned(), "application/json".to_owned()),
            ("Content-Type".to_owned(), "text/plain".to_owned()),
            ("Client-ID".to_owned(), client_id.expose().to_owned()),
            (
                "Authorization".to_owned(),
                format!("Bearer {}", token.expose()),
            ),
        ],
        body: body.into_bytes(),
    }
}

/// `title` as the inside of an Apicalypse string: control characters, `"`
/// and `\` dropped, at most [`MAX_QUERY_TITLE`] bytes, cut on a character
/// boundary. Dropping rather than escaping means the title can't leave the
/// literal whatever Apicalypse does with an escape; [`fold`] ignores
/// punctuation, so matching is unchanged.
pub fn literal(title: &str) -> String {
    let mut out = String::with_capacity(title.len().min(MAX_QUERY_TITLE) + 8);
    let mut used = 0;
    for c in title
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '"' | '\\'))
    {
        used += c.len_utf8();
        if used > MAX_QUERY_TITLE {
            break;
        }
        out.push(c);
    }
    out
}

/// The one game that agrees with the query: its title equal to the
/// query's (compared by [`fold`]) and, when the query has a year, that
/// year. None, or more than one, is no match. A game with IGDB's Erotic
/// theme isn't passed over: it's chosen, marked adult, and the runtime
/// refuses it, so a same-titled game never stands in for it.
pub fn choose(query: &Query, hits: &[Hit]) -> Lookup {
    let want = fold(query.title());
    if want.is_empty() {
        return Lookup::NoMatch;
    }
    let mut agree = hits.iter().filter(|h| {
        h.title.as_deref().is_some_and(|t| fold(t) == want)
            && query.year().is_none_or(|y| h.year == Some(y))
    });
    match (agree.next(), agree.next()) {
        (Some(h), None) => Lookup::Match(identification(query, h)),
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

fn identification(query: &Query, h: &Hit) -> Identification {
    let mut genres: Vec<String> = Vec::new();
    for g in h.genres.iter().filter_map(|&g| genre(g)) {
        if !genres.iter().any(|x| x == g) {
            genres.push(g.to_owned());
        }
    }
    Identification {
        ids: vec![ExternalId {
            scheme: "igdb".into(),
            value: h.id.to_string(),
        }],
        canonical_title: h.title.clone(),
        alt_titles: Vec::new(),
        genres,
        year: query.year().or(h.year),
        confidence: if query.year().is_some() {
            CONFIDENCE_WITH_YEAR
        } else {
            CONFIDENCE_WITHOUT_YEAR
        },
        adult: h.adult(),
    }
}

/// IGDB's fixed genre ids (`/v4/genres`), by name. An id not listed is
/// dropped, so no text from the answer is used.
pub fn genre(id: u32) -> Option<&'static str> {
    Some(match id {
        2 => "Point-and-click",
        4 => "Fighting",
        5 => "Shooter",
        7 => "Music",
        8 => "Platform",
        9 => "Puzzle",
        10 => "Racing",
        11 => "Real Time Strategy (RTS)",
        12 => "Role-playing (RPG)",
        13 => "Simulator",
        14 => "Sport",
        15 => "Strategy",
        16 => "Turn-based strategy (TBS)",
        24 => "Tactical",
        25 => "Hack and slash/Beat 'em up",
        26 => "Quiz/Trivia",
        30 => "Pinball",
        31 => "Adventure",
        32 => "Indie",
        33 => "Arcade",
        34 => "Visual Novel",
        35 => "Card & Board Game",
        36 => "MOBA",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_alphanumeric() {
        assert!(valid_credential("testclientid0"));
        for bad in ["", "a b", "a\r\nX: y", "a&b=c", "é"] {
            assert!(!valid_credential(bad), "{bad:?}");
        }
        assert!(!valid_credential(&"a".repeat(MAX_CREDENTIAL + 1)));
    }

    #[test]
    fn quotes_and_backslashes_are_dropped_and_capped() {
        assert_eq!(literal(r#"a"b\c\"#), "abc");
        assert_eq!(literal(&"\"".repeat(400)), "");
        assert_eq!(literal("a\n\u{0}b; fields *;"), "ab; fields *;");
        assert_eq!(literal(&"é".repeat(200)).len(), 254);
    }

    #[test]
    fn terms() {
        let t = &TERMS;
        assert!(!t.background_walk && !t.redistribute && !t.adult_marker_required);
        assert_eq!(t.retention, Retention::DEFAULT);
        let a = t.attribution.unwrap();
        assert_eq!((a.text, a.link), (ATTRIBUTION, ATTRIBUTION_URL));
        assert!(t.accepts(None) && t.accepts(Some(false)) && !t.accepts(Some(true)));
    }
}
