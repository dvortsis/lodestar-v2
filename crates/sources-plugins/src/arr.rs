//! Sonarr and Radarr webhooks as a source (architecture §18.2 rank 17,
//! §18.7; plan `2026-11-01-v06-sources.md` §5 Q9).
//!
//! Sonarr and Radarr call a webhook when they grab a release and when
//! they import it. The API's `POST /webhooks/{integration}` route
//! (`crates/api/src/webhooks.rs`) checks the integration's token, caps the
//! body and its rate, and hands the body to an [`Inbox`]. The inbox
//! parses it ([`parse`]), maps a Grab or Download event to one key with
//! its claims ([`to_found`]), and puts it on the running plugin's
//! [`Emitter`] without waiting. From there it takes the pipeline's
//! filter-before-storage path (SRC-2), as every source's finds do.
//!
//! **Everything it finds is `private_source`** (§18.7): the release came
//! through the user's own indexers, which may be private trackers, and a
//! public lookup would leak its hash (BEP 27). So the key never reaches
//! DHT `get_peers`, PEX or a public tracker (the pool refuses such keys),
//! and is never served over Torznab or Stremio, in feeds or in exports.
//! The claims are the release title, its size and the series' or movie's
//! catalogue ids. The indexer's name, the download client, file paths and
//! URLs are never kept.
//!
//! **Everything the body holds is untrusted.** It's read with the bounded
//! JSON reader ([`lodestar_sources_feeds::json`]), never a whole-tree
//! parse: at most [`LIMITS`]`.max_bytes`, nesting, values and arrays
//! capped while reading, and a string copied only when it's a field kept
//! here, after its raw length is checked. A body that isn't one JSON
//! object, or breaks a cap, is [`Malformed`] and dropped; the inbox
//! counts it ([`Inbox::malformed`]).
//!
//! The plugin itself sends nothing: no endpoints, no HTTP. While it runs,
//! its emitter is the inbox's; when it stops or is turned off, the inbox
//! answers [`Delivery::Off`] and keeps nothing.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lodestar_metainfo::InfoHash;
use lodestar_sources::claims::{Claims, Confidence, ExternalId, Provenance};
use lodestar_sources::{
    BoxFuture, Consent, Emitter, Found, Kind, Manifest, Network, PluginError, Policy, Priority,
    Risk, SourceCtx, SourcePlugin,
};
use lodestar_sources_feeds::json::{self, R, Reader, Walk};

/// The plugin's id in the store. Never rename.
pub const ID: &str = "arr_webhooks";
/// The strategy code of a Grab event's key.
pub const STRATEGY_GRAB: u8 = 1;
/// The strategy code of a Download (import) event's key.
pub const STRATEGY_DOWNLOAD: u8 = 2;
/// The largest size claim believed: 1 PiB.
pub const MAX_SIZE: u64 = 1 << 50;
/// Bytes in a kept title, after cleaning: the claims' name limit.
pub const MAX_TITLE: usize = lodestar_sources::claims::MAX_NAME;
/// Bytes in a kept catalogue id (`tt0111161`, `81189`).
pub const MAX_ID: usize = 32;

/// The caps on one body, and why:
/// - `max_bytes`: a real Grab or Download body is 2–40 KB (a season pack
///   lists every episode, each with its overview). 256 KiB is six times
///   the largest, and the API refuses a bigger body before reading it.
/// - `max_depth`: the shapes nest four deep (`episodes[].images[]`); 16
///   leaves room for fields added later without reading deep trees.
/// - `max_values`: about 60 values an episode; 20,000 covers a pack of
///   300 episodes.
/// - `max_array`: no array in the shapes comes near 1,000 elements.
/// - `max_text`: the raw bytes of one kept string. A release title is
///   under 300 bytes; an overview we skip may be longer, and skipping
///   doesn't copy.
pub const LIMITS: Limits = Limits {
    max_bytes: 256 * 1024,
    max_depth: 16,
    max_values: 20_000,
    max_array: 1_000,
    max_text: 4096,
};

/// Caps on one parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The body's bytes; a longer body is [`Malformed`].
    pub max_bytes: usize,
    pub max_depth: usize,
    pub max_values: usize,
    pub max_array: usize,
    pub max_text: usize,
}

/// Which app sent it: the route's integration, never the body's say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum App {
    Sonarr,
    Radarr,
}

impl App {
    /// The route name: `sonarr` or `radarr`.
    pub const fn name(self) -> &'static str {
        match self {
            App::Sonarr => "sonarr",
            App::Radarr => "radarr",
        }
    }

    pub fn from_name(name: &str) -> Option<App> {
        match name {
            "sonarr" => Some(App::Sonarr),
            "radarr" => Some(App::Radarr),
            _ => None,
        }
    }

    /// The object holding the series or movie: its catalogue ids.
    fn media_key(self) -> &'static str {
        match self {
            App::Sonarr => "series",
            App::Radarr => "movie",
        }
    }

    /// The object holding the imported file: its scene name.
    fn file_key(self) -> &'static str {
        match self {
            App::Sonarr => "episodeFile",
            App::Radarr => "movieFile",
        }
    }
}

/// What the event says happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    /// A release was sent to the download client.
    Grab,
    /// A download was imported (Sonarr and Radarr call it `Download`).
    Download,
    /// The app's "Test" button.
    Test,
    /// Any other event (Rename, Health, ApplicationUpdate, …), or none.
    Other,
}

/// One event, as the app stated it. Every field is a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub event_type: EventType,
    /// `downloadId` when it's a 40-digit hex info hash (a torrent client's
    /// id). A Usenet client's id isn't one, and names no key.
    pub key: Option<InfoHash>,
    /// `release.releaseTitle`, else the imported file's `sceneName`.
    /// Hidden characters removed, at most [`MAX_TITLE`] bytes.
    pub title: Option<String>,
    /// `release.size`, within 1 B to [`MAX_SIZE`]. An imported file's size
    /// isn't the release's (a season pack imports many files), so it isn't
    /// used.
    pub size: Option<u64>,
    /// The series' or movie's catalogue ids: `tvdb`, `tmdb`, `imdb`.
    pub ids: Vec<(&'static str, String)>,
}

/// The body isn't one JSON object within [`LIMITS`]. Carries nothing from
/// the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Malformed;

impl std::fmt::Display for Malformed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the webhook body isn't a Sonarr or Radarr event")
    }
}

/// Parses `body` with [`LIMITS`].
pub fn parse(app: App, body: &[u8]) -> Result<Event, Malformed> {
    parse_with(app, body, &LIMITS)
}

/// Parses `body` with `limits`.
pub fn parse_with(app: App, body: &[u8], limits: &Limits) -> Result<Event, Malformed> {
    if body.len() > limits.max_bytes {
        return Err(Malformed);
    }
    let text = std::str::from_utf8(body).map_err(|_| Malformed)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut p = Parser {
        j: Reader::new(
            text,
            json::Limits {
                max_depth: limits.max_depth,
                max_values: limits.max_values,
                max_array: limits.max_array,
                max_text: limits.max_text,
            },
        ),
        app,
        d: Draft::default(),
    };
    p.document().map_err(|_| Malformed)?;
    Ok(p.d.finish())
}

/// What's been read so far.
#[derive(Debug, Default)]
struct Draft {
    event_type: Option<String>,
    download_id: Option<String>,
    release_title: Option<String>,
    scene_name: Option<String>,
    size: Option<u64>,
    ids: Vec<(&'static str, String)>,
}

impl Draft {
    fn finish(self) -> Event {
        let event_type = match self.event_type.as_deref() {
            Some("Grab") => EventType::Grab,
            Some("Download") => EventType::Download,
            Some("Test") => EventType::Test,
            _ => EventType::Other,
        };
        let key = self
            .download_id
            .filter(|h| h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|h| h.to_ascii_lowercase().parse().ok());
        let title = self
            .release_title
            .and_then(|t| clean_title(&t))
            .or_else(|| self.scene_name.and_then(|t| clean_title(&t)));
        Event {
            event_type,
            key,
            title,
            size: self.size.filter(|s| (1..=MAX_SIZE).contains(s)),
            ids: self.ids,
        }
    }
}

struct Parser<'a> {
    j: Reader<'a>,
    app: App,
    d: Draft,
}

impl<'a> Walk<'a> for Parser<'a> {
    fn json(&mut self) -> &mut Reader<'a> {
        &mut self.j
    }
}

impl<'a> Parser<'a> {
    fn document(&mut self) -> R<()> {
        if self.j.peek() != Some(b'{') {
            return Err(json::Stop::Malformed);
        }
        let (media, file) = (self.app.media_key(), self.app.file_key());
        self.object(|p, key| match key {
            "eventType" => p.text_into(|d| &mut d.event_type),
            "downloadId" => p.text_into(|d| &mut d.download_id),
            "release" => p.if_object(|p| {
                p.object(|p, key| match key {
                    "releaseTitle" => p.text_into(|d| &mut d.release_title),
                    "size" => {
                        let size = p.number()?;
                        p.d.size = size;
                        Ok(())
                    }
                    _ => p.skip(),
                })
            }),
            k if k == media => p.if_object(|p| {
                p.object(|p, key| match key {
                    "tvdbId" => p.id("tvdb"),
                    "tmdbId" => p.id("tmdb"),
                    "imdbId" => p.id("imdb"),
                    _ => p.skip(),
                })
            }),
            k if k == file => p.if_object(|p| {
                p.object(|p, key| match key {
                    "sceneName" => p.text_into(|d| &mut d.scene_name),
                    _ => p.skip(),
                })
            }),
            _ => p.skip(),
        })?;
        self.j.end()
    }

    /// Skips the next value. An over-long array in it is fine: nothing in
    /// it is kept, and the value cap still bounds the work.
    fn skip(&mut self) -> R<()> {
        self.j.skip().map(|_| ())
    }

    /// Reads an object with `f`, or skips a value of another type.
    fn if_object(&mut self, f: impl FnOnce(&mut Self) -> R<()>) -> R<()> {
        if self.j.peek() == Some(b'{') {
            f(self)
        } else {
            self.skip()
        }
    }

    /// The first usable string for a field. An over-long one, a lone
    /// surrogate or another type leaves it unset, so a later repeat may
    /// fill it; once set, repeats are read past.
    fn text_into(&mut self, field: impl FnOnce(&mut Draft) -> &mut Option<String>) -> R<()> {
        if self.j.peek() != Some(b'"') {
            return self.skip();
        }
        let text = self.j.text()?;
        let slot = field(&mut self.d);
        if slot.is_none() {
            *slot = text.ok();
        }
        Ok(())
    }

    /// A non-negative whole number, or `None` for anything else (which is
    /// read past).
    fn number(&mut self) -> R<Option<u64>> {
        match self.j.peek() {
            Some(b'-' | b'0'..=b'9') => Ok(self.j.num()?.parse().ok()),
            _ => self.skip().map(|()| None),
        }
    }

    /// A catalogue id: a number (`tvdbId`, `tmdbId`) or a string
    /// (`imdbId`). Zero and empty mean unknown and are left out.
    fn id(&mut self, scheme: &'static str) -> R<()> {
        let value = match self.j.peek() {
            Some(b'"') => self.j.text()?.ok(),
            Some(b'-' | b'0'..=b'9') => self.number()?.map(|n| n.to_string()),
            _ => return self.skip(),
        };
        if let Some(v) = value {
            push_id(&mut self.d.ids, scheme, &v);
        }
        Ok(())
    }
}

/// An id of ASCII letters and digits, not all zeros; the first per scheme.
/// A bare IMDb number gets its `tt`.
fn push_id(ids: &mut Vec<(&'static str, String)>, scheme: &'static str, v: &str) {
    let ok = !v.is_empty()
        && v.len() <= MAX_ID
        && v.bytes().all(|b| b.is_ascii_alphanumeric())
        && v.bytes().any(|b| b != b'0');
    if !ok || ids.iter().any(|(s, _)| *s == scheme) {
        return;
    }
    let v = if scheme == "imdb" && v.bytes().all(|b| b.is_ascii_digit()) {
        format!("tt{v:0>7}")
    } else {
        v.to_ascii_lowercase()
    };
    ids.push((scheme, v));
}

/// Control and format characters removed, whitespace runs made one
/// space, trimmed, and cut to [`MAX_TITLE`] bytes on a character
/// boundary. `None` if nothing is left.
fn clean_title(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len().min(MAX_TITLE));
    let mut space = false;
    for c in raw.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if c.is_control() || is_format(c) {
            continue;
        }
        let need = c.len_utf8() + usize::from(space);
        if out.len() + need > MAX_TITLE {
            break;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    (!out.is_empty()).then_some(out)
}

/// Invisible formatting characters a title has no use for: bidi controls
/// (which can disguise a name) and zero-width ones.
fn is_format(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
}

/// A Grab or Download event as a discovery: always `private_source`, with
/// no peers, no external reference, no publish date and no trackers.
/// `None` for any other event, or one with no torrent hash.
pub fn to_found(event: &Event) -> Option<Found> {
    let strategy = match event.event_type {
        EventType::Grab => STRATEGY_GRAB,
        EventType::Download => STRATEGY_DOWNLOAD,
        EventType::Test | EventType::Other => return None,
    };
    let claims = Claims {
        name: event.title.clone(),
        size: event.size,
        ids: event
            .ids
            .iter()
            .map(|(scheme, value)| ExternalId {
                scheme: (*scheme).to_owned(),
                value: value.clone(),
            })
            .collect(),
        ..Claims::default()
    };
    let mut found = Found::key(event.key?, strategy)
        .with_claims(claims)
        .with_provenance(Provenance {
            external_ref: None,
            published_at: None,
            confidence: Confidence::Medium,
        });
    found.private_source = true;
    found.clamp();
    Some(found)
}

const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "Sonarr and Radarr webhooks",
    version: "1",
    kinds: &[Kind::Discovery],
    networks: &[Network::Bt],
    // It contacts nothing: the apps call in.
    endpoints: &[],
    risk: Risk::Low,
    // A toggle: the integration's token is the setup, and nothing leaves
    // the machine.
    consent: Consent::None,
    default_priority: Priority::Normal,
    enabled_by_default: false,
};

/// No settings of its own: each integration's token is made under
/// `/webhooks` and stored hashed.
pub const CONFIG_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{},"additionalProperties":false}"#;

/// It sends no requests, so the caps are the strict defaults. It runs on
/// the coordinator, where the API receives the calls (§19.5).
const POLICY: Policy = Policy {
    config_schema: CONFIG_SCHEMA,
    ..Policy::STRICT
};

/// What became of a delivered body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// A key was handed to the pipeline.
    Found,
    /// Valid, but nothing to find: a Test, another event type, or a
    /// Usenet download with no torrent hash.
    Ignored,
    /// Not an event within [`LIMITS`]; dropped and counted.
    Malformed,
    /// The pipeline is full; dropped (the emitter counts it).
    Busy,
    /// The plugin isn't running (turned off, or not started yet); kept
    /// nothing. `test`: it was the app's Test call, which the API answers
    /// with an error so the Test button shows the source is off.
    Off { test: bool },
}

/// Where the API delivers webhook bodies: shared with the plugin, which
/// lends it its emitter while it runs.
#[derive(Clone, Default)]
pub struct Inbox {
    inner: Arc<InboxInner>,
}

#[derive(Default)]
struct InboxInner {
    /// The running plugin's emitter, with the run it belongs to.
    emitter: Mutex<Option<(u64, Emitter)>>,
    runs: AtomicU64,
    malformed: AtomicU64,
}

impl std::fmt::Debug for Inbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inbox")
            .field("running", &self.is_running())
            .field("malformed", &self.malformed())
            .finish()
    }
}

impl Inbox {
    /// Parses `body` from `app`, and hands a Grab or Download event's key
    /// to the running plugin without waiting. Never logs the body.
    pub fn deliver(&self, app: App, body: &[u8]) -> Delivery {
        let event = match parse(app, body) {
            Ok(e) => e,
            Err(Malformed) => {
                self.inner.malformed.fetch_add(1, Ordering::Relaxed);
                return Delivery::Malformed;
            }
        };
        let emitter = match self.inner.emitter.lock() {
            Ok(e) => e.as_ref().map(|(_, e)| e.clone()),
            Err(_) => None,
        };
        let Some(found) = to_found(&event) else {
            if event.event_type == EventType::Test && emitter.is_none() {
                return Delivery::Off { test: true };
            }
            return Delivery::Ignored;
        };
        match emitter {
            None => Delivery::Off { test: false },
            Some(e) if e.try_emit(found) => Delivery::Found,
            Some(_) => Delivery::Busy,
        }
    }

    /// Bodies refused as malformed since start.
    pub fn malformed(&self) -> u64 {
        self.inner.malformed.load(Ordering::Relaxed)
    }

    /// Whether the plugin is running and takes deliveries.
    pub fn is_running(&self) -> bool {
        self.inner.emitter.lock().is_ok_and(|e| e.is_some())
    }

    /// Lends a new run's emitter; the guard takes it back.
    fn lend(&self, emitter: Emitter) -> Lent<'_> {
        let run = self.inner.runs.fetch_add(1, Ordering::Relaxed) + 1;
        if let Ok(mut slot) = self.inner.emitter.lock() {
            *slot = Some((run, emitter));
        }
        Lent(self, run)
    }
}

/// Takes the emitter back from the inbox when the run ends, however it
/// ends, unless a newer run has lent its own since (a supervisor that
/// starts the next run before the old future is dropped).
struct Lent<'a>(&'a Inbox, u64);

impl Drop for Lent<'_> {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.0.inner.emitter.lock()
            && slot.as_ref().is_some_and(|(run, _)| *run == self.1)
        {
            *slot = None;
        }
    }
}

/// The webhooks plugin.
#[derive(Debug, Default)]
pub struct ArrWebhooks {
    inbox: Inbox,
}

impl ArrWebhooks {
    pub fn new() -> ArrWebhooks {
        ArrWebhooks::default()
    }

    /// The inbox the API delivers to.
    pub fn inbox(&self) -> Inbox {
        self.inbox.clone()
    }
}

impl SourcePlugin for ArrWebhooks {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn policy(&self) -> &Policy {
        &POLICY
    }

    fn run(&self, ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(async move {
            let SourceCtx {
                emitter,
                mut shutdown,
                ..
            } = ctx;
            let _lent = self.inbox.lend(emitter);
            shutdown.requested().await;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_lose_hidden_characters_and_keep_to_the_cap() {
        assert_eq!(
            clean_title("  Big\u{202e} Buck\t\tBunny\u{0}  ").as_deref(),
            Some("Big Buck Bunny")
        );
        assert_eq!(clean_title("\u{200b} \u{7}"), None);
        let long = "é".repeat(MAX_TITLE);
        let t = clean_title(&long).unwrap();
        assert!(t.len() <= MAX_TITLE && t.len() >= MAX_TITLE - 1);
    }

    #[test]
    fn ids_are_checked_and_imdb_numbers_get_their_prefix() {
        let mut ids = Vec::new();
        push_id(&mut ids, "imdb", "1254207");
        push_id(&mut ids, "imdb", "tt9");
        push_id(&mut ids, "tvdb", "0");
        push_id(&mut ids, "tvdb", "a b");
        push_id(&mut ids, "tmdb", &"1".repeat(MAX_ID + 1));
        push_id(&mut ids, "tmdb", "10378");
        assert_eq!(
            ids,
            vec![
                ("imdb", "tt1254207".to_owned()),
                ("tmdb", "10378".to_owned())
            ]
        );
    }
}
