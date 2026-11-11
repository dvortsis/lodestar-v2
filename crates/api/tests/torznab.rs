//! `/torznab/api` through the router (plan §4.3 TZ-0): the caps document
//! byte for byte, each search mode, paging, every exclusion (private
//! source, BEP 27 private, hidden, unverified, Adult with
//! `torznab_adult` off and on), hostile names written as well-formed XML
//! (parsed back strictly below), parameter refusals, and every auth
//! refusal as a Torznab error document that never carries the key.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use lodestar_api::keys::{self, scope};
use lodestar_api::torznab::TorznabAdult;
use lodestar_api::{AdultVisibility, ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::Correction;
use lodestar_metainfo::Info;
use lodestar_search::SearchIndex;
use lodestar_store::claims::Claims;
use lodestar_store::unverified::{Network, NewUnverified};
use lodestar_store::{KeyKind, NewApiKey, NewTorrent, SourceId, Store, StoreConfig, flags};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

static NEXT_IP: AtomicU32 = AtomicU32::new(1);

/// A fresh address per call, so tests don't share failure buckets.
fn fresh_ip() -> SocketAddr {
    let n = NEXT_IP.fetch_add(1, Ordering::Relaxed);
    SocketAddr::from(([10, 70, (n >> 8) as u8, n as u8], 40000))
}

// ── A strict reader for what the endpoint writes ──────────────────────

/// An element: its name, attributes, text and children.
#[derive(Debug, Clone, PartialEq)]
struct El {
    name: String,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<El>,
}

impl El {
    fn attr(&self, k: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }

    fn child(&self, name: &str) -> Option<&El> {
        self.children.iter().find(|c| c.name == name)
    }

    fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a El> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// A `torznab:attr`'s value.
    fn tz(&self, name: &str) -> Vec<&str> {
        self.all("torznab:attr")
            .filter(|a| a.attr("name") == Some(name))
            .filter_map(|a| a.attr("value"))
            .collect()
    }
}

/// Whether XML 1.0 allows `c` in a document.
fn xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
}

/// Parses `doc` as XML 1.0, strictly: the declaration, one root,
/// matched tags, quoted attributes without `<`, only the five
/// predefined entities (and character references to allowed
/// characters), only characters XML allows. Panics on anything else.
fn parse_xml(doc: &str) -> El {
    quick_xml_accepts(doc);
    for c in doc.chars() {
        assert!(xml_char(c), "character {c:?} isn't allowed in XML:\n{doc}");
    }
    let decl = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>";
    let rest = doc.strip_prefix(decl).expect("the declaration");
    let mut p = Parser {
        s: rest.as_bytes(),
        i: 0,
        doc,
    };
    p.space();
    let root = p.element();
    p.space();
    assert_eq!(p.i, p.s.len(), "content after the root:\n{doc}");
    root
}

/// A second, independent reader: quick-xml reads `doc` to its end
/// without an error (end names checked).
fn quick_xml_accepts(doc: &str) {
    use quick_xml::events::Event;
    let mut r = quick_xml::Reader::from_str(doc);
    r.config_mut().check_end_names = true;
    let mut depth = 0i64;
    loop {
        match r.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::Start(_)) => depth += 1,
            Ok(Event::End(_)) => depth -= 1,
            Ok(Event::CData(_)) => panic!("CDATA isn't written:\n{doc}"),
            Ok(_) => {}
            Err(e) => panic!("quick-xml: {e}:\n{doc}"),
        }
    }
    assert_eq!(depth, 0, "unclosed elements:\n{doc}");
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    doc: &'a str,
}

impl Parser<'_> {
    fn fail(&self, what: &str) -> ! {
        panic!("{what} at byte {}:\n{}", self.i, self.doc)
    }

    fn space(&mut self) {
        while self.i < self.s.len() && b" \t\r\n".contains(&self.s[self.i]) {
            self.i += 1;
        }
    }

    fn eat(&mut self, b: u8) {
        if self.s.get(self.i) != Some(&b) {
            self.fail(&format!("expected {:?}", char::from(b)));
        }
        self.i += 1;
    }

    fn name(&mut self) -> String {
        let start = self.i;
        while self
            .s
            .get(self.i)
            .is_some_and(|b| b.is_ascii_alphanumeric() || b"-_:.".contains(b))
        {
            self.i += 1;
        }
        if start == self.i || !self.s[start].is_ascii_alphabetic() {
            self.fail("expected a name");
        }
        String::from_utf8(self.s[start..self.i].to_vec()).unwrap()
    }

    /// Text up to `end` (exclusive), entities decoded; `<` refused.
    fn chars_until(&mut self, end: &[u8]) -> String {
        let mut out = Vec::new();
        while !end.contains(self.s.get(self.i).unwrap_or_else(|| self.fail("unexpected end"))) {
            match self.s[self.i] {
                b'<' => self.fail("a raw '<'"),
                b'&' => {
                    let semi = self.s[self.i..]
                        .iter()
                        .position(|b| *b == b';')
                        .unwrap_or_else(|| self.fail("an unterminated entity"));
                    let ent = std::str::from_utf8(&self.s[self.i + 1..self.i + semi]).unwrap();
                    let c = match ent {
                        "amp" => '&',
                        "lt" => '<',
                        "gt" => '>',
                        "quot" => '"',
                        "apos" => '\'',
                        _ => {
                            let n = ent
                                .strip_prefix("#x")
                                .and_then(|h| u32::from_str_radix(h, 16).ok())
                                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                                .and_then(char::from_u32)
                                .unwrap_or_else(|| self.fail("an unknown entity"));
                            if !xml_char(n) {
                                self.fail("a reference to a forbidden character");
                            }
                            n
                        }
                    };
                    let mut b = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                    self.i += semi + 1;
                }
                b => {
                    out.push(b);
                    self.i += 1;
                }
            }
        }
        String::from_utf8(out).unwrap()
    }

    fn element(&mut self) -> El {
        self.eat(b'<');
        let name = self.name();
        let mut attrs = Vec::new();
        loop {
            let before = self.i;
            self.space();
            match self.s.get(self.i) {
                Some(b'/') => {
                    self.i += 1;
                    self.eat(b'>');
                    return El {
                        name,
                        attrs,
                        text: String::new(),
                        children: Vec::new(),
                    };
                }
                Some(b'>') => {
                    self.i += 1;
                    break;
                }
                _ => {
                    if before == self.i {
                        self.fail("attributes need a space between them");
                    }
                    let k = self.name();
                    self.eat(b'=');
                    self.eat(b'"');
                    let v = self.chars_until(b"\"");
                    self.eat(b'"');
                    if attrs.iter().any(|(n, _)| *n == k) {
                        self.fail("a repeated attribute");
                    }
                    attrs.push((k, v));
                }
            }
        }
        let mut text = String::new();
        let mut children = Vec::new();
        loop {
            text.push_str(&self.chars_until(b"<"));
            if self.s.get(self.i + 1) == Some(&b'/') {
                self.i += 2;
                let end = self.name();
                if end != name {
                    self.fail(&format!("</{end}> closes <{name}>"));
                }
                self.space();
                self.eat(b'>');
                return El {
                    name,
                    attrs,
                    text,
                    children,
                };
            }
            if matches!(self.s.get(self.i + 1), Some(b'!' | b'?')) {
                self.fail("CDATA, comments and processing instructions aren't written");
            }
            children.push(self.element());
        }
    }
}

#[test]
fn the_reader_is_strict() {
    let ok = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<a x=\"1&amp;2\"><b/>t&lt;</a>\n";
    let a = parse_xml(ok);
    assert_eq!((a.attr("x"), a.text.as_str()), (Some("1&2"), "t<"));
    for bad in [
        "<a></b>",
        "<a>&nbsp;</a>",
        "<a>&amp</a>",
        "<a x='1'/>",
        "<a x=\"<\"/>",
        "<a><![CDATA[x]]></a>",
        "<a>\u{1}</a>",
        "<a/><b/>",
        "<a x=\"1\"x=\"2\"/>",
        "<a>&#1;</a>",
    ] {
        let doc = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>{bad}");
        assert!(
            std::panic::catch_unwind(|| parse_xml(&doc)).is_err(),
            "{bad}"
        );
    }
}

// ── The fixture ───────────────────────────────────────────────────────

/// One fixture torrent: raw name bytes, whether it's corrected to Adult,
/// and its extra flags. Single-file, 1 MiB.
#[derive(Clone)]
struct T {
    name: Vec<u8>,
    adult: bool,
    flags: i64,
}

fn t(name: &str) -> T {
    T {
        name: name.as_bytes().to_vec(),
        adult: false,
        flags: 0,
    }
}

fn metainfo(name: &[u8]) -> Vec<u8> {
    let entries = vec![
        ("name", OwnedValue::Bytes(name.to_vec())),
        ("piece length", OwnedValue::Int(1 << 16)),
        ("pieces", OwnedValue::Bytes(vec![0; 20])),
        ("length", OwnedValue::Int(1 << 20)),
    ];
    to_vec(&OwnedValue::Dict(
        entries
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
    ))
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    users: Arc<Mutex<Store>>,
    /// `torznab_adult`, live.
    torznab_adult: Arc<AtomicBool>,
    /// Info hashes, in insertion order.
    hashes: Vec<String>,
    _dir: tempfile::TempDir,
}

#[derive(Default)]
struct Opts {
    /// "Show adult content" (search's own): on, to show it doesn't apply.
    show_adult: bool,
    no_search: bool,
    frozen: bool,
    /// No `TorznabAdult` extension at all.
    no_adult_extension: bool,
    unverified: Option<&'static str>,
}

fn fixture(ts: &[T], o: Opts) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let raws: Vec<Vec<u8>> = ts.iter().map(|t| metainfo(&t.name)).collect();
    let infos: Vec<Info> = raws.iter().map(|r| Info::parse(r).unwrap()).collect();
    let batch: Vec<NewTorrent<'_>> = infos
        .iter()
        .zip(&raws)
        .zip(ts)
        .enumerate()
        .map(|(i, ((info, raw), t))| NewTorrent {
            info,
            raw_info: Some(raw),
            discovered_at: 1_700_000_000 + i as i64,
            metadata_at: 1_700_000_001 + i as i64,
            extra_flags: t.flags,
            source: SourceId::new(0).unwrap(),
        })
        .collect();
    let ids: Vec<i64> = store
        .insert(&batch)
        .unwrap()
        .into_iter()
        .map(|i| i.id())
        .collect();
    let mut enriched = Vec::new();
    for (id, t) in ids.iter().zip(ts) {
        let row = store.torrent(*id).unwrap().unwrap();
        let tree = store.files(*id).unwrap().unwrap();
        let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
        let correction = Correction {
            adult: t.adult.then_some(true),
            not_junk: false,
        };
        let (e, _) = lodestar_filter::reclassify::enrich_corrected(&row.name, &files, correction);
        enriched.push((*id, e));
    }
    store.set_enrichments(&enriched).unwrap();
    if let Some(name) = o.unverified {
        let feed = store.register_source("nyaa", "1", 1_700_000_000).unwrap();
        let claims = Claims {
            name: Some(name.into()),
            ..Claims::default()
        };
        store
            .put_unverified(&[NewUnverified {
                network: Network::Bt,
                key: &[7; 20],
                source: feed,
                private_source: false,
                claims: Some(&claims),
                at: 1_700_000_100,
            }])
            .unwrap();
    }
    let index = SearchIndex::in_memory().unwrap();
    {
        let mut w = index.writer(15 << 20).unwrap();
        w.catch_up(&mut store, 100_000).unwrap();
        w.commit().unwrap();
    }
    let hashes = infos
        .iter()
        .map(|i| {
            i.primary_hash()
                .0
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        })
        .collect();
    drop(store);
    let mut auth = Auth::load(&dir.path().join("auth.json"), false).unwrap();
    if o.frozen {
        auth = auth.without_refill();
    }
    let auth = Arc::new(auth);
    let users = Arc::new(Mutex::new(
        Store::open(&db, StoreConfig::default()).unwrap(),
    ));
    let show = o.show_adult;
    let torznab_adult = Arc::new(AtomicBool::new(false));
    let shown = torznab_adult.clone();
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: (!o.no_search).then(|| index.into()),
            reader: Arc::new(Mutex::new(
                Store::open_reader(&db, StoreConfig::default()).unwrap(),
            )),
            filters: None,
            settings: None,
            ops: None,
            adult: AdultVisibility::follow(move || show),
            user_data: Some(users.clone()),
            insights: None,
            hot: None,
            events: None,
        }),
    );
    let app = if o.no_adult_extension {
        app
    } else {
        app.layer(Extension(TorznabAdult(AdultVisibility::follow(move || {
            shown.load(Ordering::SeqCst)
        }))))
    };
    Fixture {
        app,
        auth,
        users,
        torznab_adult,
        hashes,
        _dir: dir,
    }
}

impl Fixture {
    /// A stored key of `kind` with `scopes`: its id and secret.
    fn key(&self, kind: KeyKind, scopes: u32) -> (i64, String) {
        let m = keys::mint(kind).unwrap();
        let k = self
            .users
            .lock()
            .unwrap()
            .create_api_key(
                &NewApiKey {
                    kind,
                    name: "arr",
                    prefix: &m.prefix,
                    hash: m.hash,
                    scopes,
                    saved_search_id: None,
                    expires_at: None,
                },
                0,
            )
            .unwrap();
        (k.id, m.secret.expose().to_string())
    }

    fn torznab_key(&self) -> String {
        self.key(KeyKind::ApiKey, scope::TORZNAB).1
    }

    async fn send(&self, req: Request<Body>) -> Reply {
        let mut req = req;
        req.extensions_mut().insert(ConnectInfo(fresh_ip()));
        send_from(&self.app, req).await
    }

    /// `GET /api/v1/torznab/api?<query>&apikey=<key>`.
    async fn api(&self, key: &str, query: &str) -> Reply {
        let uri = format!("/api/v1/torznab/api?{query}&apikey={key}");
        self.send(get(&uri)).await
    }

    /// A search that must succeed: the channel.
    async fn feed(&self, key: &str, query: &str) -> El {
        let r = self.api(key, query).await;
        assert_eq!(r.status, StatusCode::OK, "{query}: {}", r.body);
        assert_eq!(r.content_type, "application/rss+xml; charset=utf-8");
        let rss = parse_xml(&r.body);
        assert_eq!(rss.name, "rss");
        rss.child("channel").unwrap().clone()
    }

    /// The titles a search finds, in order.
    async fn titles(&self, key: &str, query: &str) -> Vec<String> {
        self.feed(key, query)
            .await
            .all("item")
            .map(|i| i.child("title").unwrap().text.clone())
            .collect()
    }
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

#[derive(Debug)]
struct Reply {
    status: StatusCode,
    content_type: String,
    retry_after: Option<String>,
    cache_control: Option<String>,
    body: String,
}

async fn send_from(app: &Router, req: Request<Body>) -> Reply {
    let resp = app.clone().oneshot(req).await.unwrap();
    let h = |n: header::HeaderName| {
        resp.headers()
            .get(n)
            .map(|v| v.to_str().unwrap().to_string())
    };
    let (content_type, retry_after, cache_control) = (
        h(header::CONTENT_TYPE).unwrap_or_default(),
        h(header::RETRY_AFTER),
        h(header::CACHE_CONTROL),
    );
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 24)
        .await
        .unwrap();
    Reply {
        status,
        content_type,
        retry_after,
        cache_control,
        body: String::from_utf8(bytes.to_vec()).expect("UTF-8"),
    }
}

/// An error document's code.
fn error_code(r: &Reply) -> String {
    assert_eq!(r.content_type, "application/xml; charset=utf-8", "{}", r.body);
    let e = parse_xml(&r.body);
    assert_eq!(e.name, "error", "{}", r.body);
    assert!(e.attr("description").is_some_and(|d| !d.is_empty()));
    e.attr("code").unwrap().to_string()
}

const TV1: &str = "The.Show.S01E02.1080p.WEB.x264-GRP.mkv";
const TV2: &str = "The.Show.S02E05.720p.HDTV.x264-GRP.mkv";
const MOVIE: &str = "Great.Movie.2019.2160p.BluRay.x265-GRP.mkv";
const BOOK: &str = "Some Author - A Novel (2020).epub";
const MUSIC: &str = "Some Artist - Song Title (2021).flac";
const DAILY: &str = "Daily.Show.2024.01.15.1080p.WEB.x264-GRP.mkv";

fn library() -> Vec<T> {
    [TV1, TV2, MOVIE, BOOK, MUSIC, DAILY]
        .iter()
        .map(|n| t(n))
        .collect()
}

// ── Caps ──────────────────────────────────────────────────────────────

/// The caps document, byte for byte. A change here is a change to what
/// every connected *arr app sees.
const CAPS: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
    "<caps><server title=\"Lodestar\"/><limits default=\"100\" max=\"100\"/>",
    "<searching>",
    "<search available=\"yes\" supportedParams=\"q\"/>",
    "<tv-search available=\"yes\" supportedParams=\"q,season,ep\"/>",
    "<movie-search available=\"yes\" supportedParams=\"q\"/>",
    "<music-search available=\"yes\" supportedParams=\"q\"/>",
    "<audio-search available=\"yes\" supportedParams=\"q\"/>",
    "<book-search available=\"yes\" supportedParams=\"q\"/>",
    "</searching>",
    "<categories>",
    "<category id=\"2000\" name=\"Movies\"><subcat id=\"2030\" name=\"Movies/SD\"/>",
    "<subcat id=\"2040\" name=\"Movies/HD\"/><subcat id=\"2045\" name=\"Movies/UHD\"/></category>",
    "<category id=\"3000\" name=\"Audio\"><subcat id=\"3030\" name=\"Audio/Audiobook\"/></category>",
    "<category id=\"4000\" name=\"PC\"><subcat id=\"4050\" name=\"PC/Games\"/></category>",
    "<category id=\"5000\" name=\"TV\"><subcat id=\"5030\" name=\"TV/SD\"/>",
    "<subcat id=\"5040\" name=\"TV/HD\"/><subcat id=\"5045\" name=\"TV/UHD\"/></category>",
    "<category id=\"6000\" name=\"XXX\"><subcat id=\"6060\" name=\"XXX/ImageSet\"/></category>",
    "<category id=\"7000\" name=\"Books\"><subcat id=\"7020\" name=\"Books/EBook\"/>",
    "<subcat id=\"7030\" name=\"Books/Comics\"/></category>",
    "<category id=\"8000\" name=\"Other\"><subcat id=\"8010\" name=\"Other/Misc\"/></category>",
    "</categories></caps>\n",
);

#[tokio::test]
async fn caps_is_the_golden_document() {
    let f = fixture(&[], Opts::default());
    let key = f.torznab_key();
    let r = f.api(&key, "t=caps").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.content_type, "application/xml; charset=utf-8");
    assert_eq!(r.cache_control.as_deref(), Some("no-store"));
    assert_eq!(r.body, CAPS);
    let caps = parse_xml(&r.body);
    // ID search isn't advertised until ENR-11.
    assert!(!r.body.contains("imdbid") && !r.body.contains("tvdbid"));
    assert_eq!(caps.child("limits").unwrap().attr("max"), Some("100"));
    // Caps doesn't need search.
    let off = fixture(&[], Opts { no_search: true, ..Opts::default() });
    let key = off.torznab_key();
    assert_eq!(off.api(&key, "t=caps").await.body, CAPS);
}

// ── Searching ─────────────────────────────────────────────────────────

#[tokio::test]
async fn each_mode_finds_its_kind() {
    let f = fixture(&library(), Opts::default());
    let key = f.torznab_key();
    // t=search: words, everything.
    assert_eq!(f.titles(&key, "t=search&q=show").await.len(), 3);
    assert_eq!(f.titles(&key, "t=search&q=great+movie").await, [MOVIE]);
    // tvsearch: TV only, season and episode.
    let tv = f.titles(&key, "t=tvsearch&q=the+show").await;
    assert_eq!(tv.len(), 2, "{tv:?}");
    assert_eq!(f.titles(&key, "t=tvsearch&q=the+show&season=1").await, [TV1]);
    assert_eq!(
        f.titles(&key, "t=tvsearch&q=the+show&season=2&ep=5").await,
        [TV2]
    );
    assert!(f.titles(&key, "t=tvsearch&q=the+show&season=1&ep=5").await.is_empty());
    assert!(f.titles(&key, "t=tvsearch&q=great+movie").await.is_empty());
    // A daily show: the date.
    assert_eq!(
        f.titles(&key, "t=tvsearch&q=daily+show&season=2024&ep=01/15").await,
        [DAILY]
    );
    assert!(f.titles(&key, "t=tvsearch&q=daily+show&season=2024&ep=01/16").await.is_empty());
    // movie, music, book: their own categories.
    assert_eq!(f.titles(&key, "t=movie").await, [MOVIE]);
    assert!(f.titles(&key, "t=movie&q=show").await.is_empty());
    assert_eq!(f.titles(&key, "t=book").await, [BOOK]);
    assert_eq!(f.titles(&key, "t=music").await, [MUSIC]);
    // An explicit cat replaces the mode's.
    assert_eq!(f.titles(&key, "t=tvsearch&cat=2000").await, [MOVIE]);
    // Subcategories by resolution.
    assert_eq!(f.titles(&key, "t=search&cat=5040&q=the+show").await, [TV1, TV2]);
    assert_eq!(f.titles(&key, "t=search&cat=2045").await, [MOVIE]);
    assert!(f.titles(&key, "t=search&cat=2030").await.is_empty());
    // Categories this server doesn't list find nothing.
    let none = f.feed(&key, "t=search&cat=1234").await;
    assert_eq!(none.all("item").count(), 0);
    assert_eq!(none.child("torznab:response").unwrap().attr("total"), Some("0"));
}

#[tokio::test]
async fn items_carry_magnets_categories_and_attributes() {
    let f = fixture(&library(), Opts::default());
    let key = f.torznab_key();
    let channel = f.feed(&key, "t=search&q=great+movie").await;
    let item = channel.child("item").unwrap();
    let hash = &f.hashes[2];
    assert_eq!(item.child("guid").unwrap().text, *hash);
    assert_eq!(item.child("guid").unwrap().attr("isPermaLink"), Some("false"));
    assert_eq!(item.child("size").unwrap().text, "1048576");
    assert_eq!(item.child("pubDate").unwrap().text, "Tue, 14 Nov 2023 22:13:22 +0000");
    let link = &item.child("link").unwrap().text;
    assert_eq!(
        *link,
        format!("magnet:?xt=urn:btih:{hash}&dn=Great.Movie.2019.2160p.BluRay.x265-GRP.mkv")
    );
    let enc = item.child("enclosure").unwrap();
    assert_eq!(enc.attr("url"), Some(link.as_str()));
    assert_eq!(enc.attr("length"), Some("1048576"));
    assert_eq!(enc.attr("type"), Some("application/x-bittorrent"));
    let cats: Vec<&str> = item.all("category").map(|c| c.text.as_str()).collect();
    assert_eq!(cats, ["2000", "2045"]);
    assert_eq!(item.tz("category"), ["2000", "2045"]);
    assert_eq!(item.tz("infohash"), [hash.as_str()]);
    assert_eq!(item.tz("magneturl"), [link.as_str()]);
    assert_eq!(item.tz("files"), ["1"]);
    // Never scraped: no swarm counts claimed.
    assert!(item.tz("seeders").is_empty());
    assert_eq!(item.tz("downloadvolumefactor"), ["0"]);
    // Only magnets: no link back to this server, so nothing that could
    // carry the key.
    assert!(!channel_text(&f.feed(&key, "t=search").await).contains("http"));
    // TV and books list under their subcategories.
    let tv = f.feed(&key, "t=search&q=the+show&season=1").await;
    assert_eq!(tv.child("item").unwrap().tz("category"), ["5000", "5040"]);
    let book = f.feed(&key, "t=book").await;
    assert_eq!(book.child("item").unwrap().tz("category"), ["7000", "7020"]);
}

/// Every attribute value and text of a document, joined.
fn channel_text(e: &El) -> String {
    let mut out = e.text.clone();
    for (_, v) in &e.attrs {
        out.push_str(v);
    }
    for c in &e.children {
        out.push_str(&channel_text(c));
    }
    out
}

#[tokio::test]
async fn paging_follows_offset_and_caps_limit() {
    let ts: Vec<T> = (0..105).map(|i| t(&format!("Paged Item {i:03}.txt"))).collect();
    let f = fixture(&ts, Opts::default());
    let key = f.torznab_key();
    // No words: newest first.
    let all = f.feed(&key, "t=search&limit=1000").await;
    let titles: Vec<&str> = all
        .all("item")
        .map(|i| i.child("title").unwrap().text.as_str())
        .collect();
    assert_eq!(titles.len(), 100, "limit is capped at 100");
    assert_eq!(titles[0], "Paged Item 104.txt");
    let response = all.child("torznab:response").unwrap();
    assert_eq!(response.attr("offset"), Some("0"));
    assert_eq!(response.attr("total"), Some("105"));
    let page = f.feed(&key, "t=search&limit=10&offset=100").await;
    let rest: Vec<&str> = page
        .all("item")
        .map(|i| i.child("title").unwrap().text.as_str())
        .collect();
    assert_eq!(
        rest,
        (0..5).rev().map(|i| format!("Paged Item {i:03}.txt")).collect::<Vec<_>>()
    );
    assert_eq!(page.child("torznab:response").unwrap().attr("offset"), Some("100"));
    let two = f.titles(&key, "t=search&limit=2&offset=2").await;
    assert_eq!(two, ["Paged Item 102.txt", "Paged Item 101.txt"]);
    // Past the search's window: an empty page, not an error.
    for offset in ["10000", "999999999"] {
        let deep = f.feed(&key, &format!("t=search&offset={offset}")).await;
        assert_eq!(deep.all("item").count(), 0);
        assert_eq!(deep.child("torznab:response").unwrap().attr("total"), Some("105"));
    }
}

// ── What's never served ───────────────────────────────────────────────

#[tokio::test]
async fn private_hidden_and_unverified_are_never_served() {
    let mut ts = vec![t("Visible Thing.txt")];
    for (name, flag) in [
        ("Secret Private Source Thing.txt", flags::PRIVATE_SOURCE),
        ("Secret Bep27 Private Thing.txt", flags::PRIVATE),
        ("Secret Remote Unverified Thing.txt", flags::REMOTE_UNVERIFIED),
        ("Secret Hidden Thing.txt", flags::HIDDEN),
    ] {
        let mut x = t(name);
        x.flags = flag;
        ts.push(x);
    }
    let f = fixture(
        &ts,
        Opts {
            unverified: Some("Secret Unverified Claim Thing"),
            ..Opts::default()
        },
    );
    let key = f.torznab_key();
    for q in [
        "t=search",
        "t=search&q=thing",
        "t=search&q=secret",
        // Query-language text is only words here.
        "t=search&q=verified:no",
        "t=search&q=flag:hidden",
        "t=search&q=flag:private_source",
        "t=search&q=hidden:yes+secret",
        "t=search&cat=8000",
    ] {
        let channel = f.feed(&key, q).await;
        let text = channel_text(&channel);
        assert!(!text.contains("Secret"), "{q}: {text}");
        for h in &f.hashes[1..] {
            assert!(!text.contains(h.as_str()), "{q}");
        }
    }
    assert_eq!(f.titles(&key, "t=search&q=thing").await, ["Visible Thing.txt"]);
    // Counts leave them out too.
    let all = f.feed(&key, "t=search").await;
    assert_eq!(all.child("torznab:response").unwrap().attr("total"), Some("1"));
}

#[tokio::test]
async fn adult_follows_torznab_adult_not_search() {
    let mut x = t("Hot.Scene.Clip.XXX.1080p.mp4");
    x.adult = true;
    let ts = vec![t("Plain Clip Thing.mp4"), x];
    // "Show adult content" is on: it doesn't apply to Torznab.
    let f = fixture(&ts, Opts { show_adult: true, ..Opts::default() });
    let key = f.torznab_key();
    for q in ["t=search", "t=search&q=clip", "t=search&cat=6000", "t=search&q=type:adult"] {
        let text = channel_text(&f.feed(&key, q).await);
        assert!(!text.contains("Hot.Scene"), "{q}: {text}");
    }
    // On: served, as XXX, followed live with no restart.
    f.torznab_adult.store(true, Ordering::SeqCst);
    assert_eq!(f.titles(&key, "t=search&cat=6000").await, ["Hot.Scene.Clip.XXX.1080p.mp4"]);
    let item = f.feed(&key, "t=search&cat=6000").await;
    assert_eq!(item.child("item").unwrap().tz("category")[0], "6000");
    assert_eq!(f.titles(&key, "t=search&q=clip").await.len(), 2);
    // Off again: gone again.
    f.torznab_adult.store(false, Ordering::SeqCst);
    assert_eq!(f.titles(&key, "t=search&q=clip").await, ["Plain Clip Thing.mp4"]);
    // Without the setting wired at all, never.
    let bare = fixture(
        &ts,
        Opts {
            show_adult: true,
            no_adult_extension: true,
            ..Opts::default()
        },
    );
    let key = bare.torznab_key();
    assert_eq!(bare.titles(&key, "t=search&q=clip").await, ["Plain Clip Thing.mp4"]);
}

// ── Hostile names ─────────────────────────────────────────────────────

#[tokio::test]
async fn hostile_names_are_well_formed_xml() {
    let names: Vec<Vec<u8>> = vec![
        b"Hostile ]]> <script>alert(1)</script> & \"q\" 'a' x.txt".to_vec(),
        b"Hostile ctrl \x01\x02\x07\x08\x0b\x0c\x1b[31m\x7f bell.txt".to_vec(),
        b"Hostile invalid \xff\xfe\xc3\x28 utf8.txt".to_vec(),
        "Hostile c1 \u{85}\u{9f} nel \u{feff} bom \u{202e} rlo.txt".as_bytes().to_vec(),
        b"Hostile tab\tnew\nline\rcr.txt".to_vec(),
        b"Hostile entity &amp; &#0; &#x1; &lt;!-- --> <![CDATA[ x ]]> <?pi?>.txt".to_vec(),
        "Hostile nonchar \u{fffe}\u{ffff} end.txt".as_bytes().to_vec(),
    ];
    let ts: Vec<T> = names
        .iter()
        .map(|n| T {
            name: n.clone(),
            adult: false,
            flags: 0,
        })
        .collect();
    let f = fixture(&ts, Opts::default());
    let key = f.torznab_key();
    let r = f.api(&key, "t=search&q=hostile").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    // Strictly well formed, every character allowed.
    let rss = parse_xml(&r.body);
    let items: Vec<&El> = rss.child("channel").unwrap().all("item").collect();
    assert_eq!(items.len(), names.len(), "{}", r.body);
    for item in items {
        let title = &item.child("title").unwrap().text;
        assert!(title.starts_with("Hostile"), "{title:?}");
        assert!(!title.chars().any(char::is_control), "{title:?}");
        // The magnet is one URL: its name percent-encoded.
        let link = &item.child("link").unwrap().text;
        assert!(link.bytes().all(|b| b.is_ascii_graphic()), "{link}");
    }
    // The markup in a name stays text.
    let text = &r.body;
    assert!(text.contains("&lt;script&gt;") && !text.contains("<script>"));
    assert!(!text.contains("<![CDATA[") && !text.contains("<?pi"));
}

// ── Parameters ────────────────────────────────────────────────────────

#[tokio::test]
async fn bad_parameters_are_error_documents() {
    let f = fixture(&library(), Opts::default());
    let key = f.torznab_key();
    for (q, code) in [
        ("q=x", "200"),
        ("t=nope", "202"),
        ("t=search&limit=0", "201"),
        ("t=search&limit=abc", "201"),
        ("t=search&offset=-5", "201"),
        ("t=search&cat=movies", "201"),
        ("t=tvsearch&season=x", "201"),
        ("t=tvsearch&season=1&ep=1/2", "201"),
        ("t=search&q=a&q=b", "201"),
        ("t=movie&imdbid=tt0111161", "203"),
    ] {
        let r = f.api(&key, q).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{q}");
        assert_eq!(error_code(&r), code, "{q}");
        assert!(!r.body.contains(&key), "{q}");
    }
    let long = format!("t=search&q={}", "a".repeat(300));
    assert_eq!(error_code(&f.api(&key, &long).await), "201");
    // An error never echoes the value it refused.
    let r = f.api(&key, "t=search&limit=%3Cscript%3E").await;
    assert!(!r.body.contains("script"), "{}", r.body);
    // Search off: 503, "API disabled".
    let off = fixture(&[], Opts { no_search: true, ..Opts::default() });
    let key = off.torznab_key();
    let r = off.api(&key, "t=search").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error_code(&r), "910");
}

// ── Auth ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn only_a_torznab_key_or_a_session_gets_in() {
    let f = fixture(&library(), Opts::default());
    let good = f.torznab_key();
    let (_, search_only) = f.key(KeyKind::ApiKey, scope::SEARCH);
    let (_, feeds) = f.key(KeyKind::FeedToken, 0);
    let (revoked_id, revoked) = f.key(KeyKind::ApiKey, scope::TORZNAB);
    f.users
        .lock()
        .unwrap()
        .revoke_api_key(revoked_id, 1)
        .unwrap();
    let unknown = keys::mint(KeyKind::ApiKey)
        .unwrap()
        .secret
        .expose()
        .to_string();
    let mut wrong = good.clone();
    let last = wrong.pop().unwrap();
    wrong.push(if last == 'A' { 'B' } else { 'A' });

    // No key: a Torznab error, not the JSON envelope.
    let r = f.send(get("/api/v1/torznab/api?t=caps")).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&r), "100");
    for (bad, status, code) in [
        (&wrong, StatusCode::UNAUTHORIZED, "100"),
        (&unknown, StatusCode::UNAUTHORIZED, "100"),
        (&revoked, StatusCode::UNAUTHORIZED, "100"),
        (&feeds, StatusCode::UNAUTHORIZED, "100"),
        (&"lsk_short".to_string(), StatusCode::UNAUTHORIZED, "100"),
        (&search_only, StatusCode::FORBIDDEN, "102"),
    ] {
        let r = f.api(bad, "t=caps").await;
        assert_eq!(r.status, status, "{bad}");
        assert_eq!(error_code(&r), code, "{bad}");
        assert!(!r.body.contains(bad.as_str()), "the key is never echoed");
        assert!(!r.body.contains("\"error\""), "not JSON");
    }
    // A Torznab key works as ?apikey=, X-Api-Key and Bearer, read-only.
    assert_eq!(f.api(&good, "t=caps").await.body, CAPS);
    let r = f
        .send(
            Request::get("/api/v1/torznab/api?t=caps")
                .header("x-api-key", &good)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(r.body, CAPS);
    let r = f
        .send(
            Request::get("/api/v1/torznab/api?t=caps")
                .header(header::AUTHORIZATION, format!("Bearer {good}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(r.body, CAPS);
    let r = f
        .send(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/api/v1/torznab/api?t=caps&apikey={good}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert!(r.status.is_client_error(), "{:?}", r.status);
    assert_eq!(r.content_type, "application/xml; charset=utf-8");
    parse_xml(&r.body);
    // Two different keys in one request: refused.
    let r = f
        .send(
            Request::get(format!("/api/v1/torznab/api?t=caps&apikey={good}"))
                .header("x-api-key", &search_only)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(error_code(&r), "100");
    // A key with every scope reaches it too.
    let (_, all) = f.key(KeyKind::ApiKey, scope::ALL);
    assert_eq!(f.api(&all, "t=caps").await.body, CAPS);
    // A signed-in browser reaches it without a key.
    let token = f.auth.setup_token().unwrap();
    let session = f
        .auth
        .setup("127.0.0.1".parse().unwrap(), &token, PASSWORD)
        .unwrap();
    let r = f
        .send(
            Request::get("/api/v1/torznab/api?t=caps")
                .header(
                    header::COOKIE,
                    format!("{}={session}", lodestar_api::auth::COOKIE),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body, CAPS);
}

#[tokio::test]
async fn failed_checks_are_rate_limited_as_a_torznab_error() {
    let f = fixture(&[], Opts { frozen: true, ..Opts::default() });
    let from = fresh_ip();
    let bad = keys::mint(KeyKind::ApiKey)
        .unwrap()
        .secret
        .expose()
        .to_string();
    let mut last = None;
    for _ in 0..=lodestar_api::auth::KEY_FAIL_BURST as usize {
        let mut req = get(&format!("/api/v1/torznab/api?t=caps&apikey={bad}"));
        req.extensions_mut().insert(ConnectInfo(from));
        last = Some(send_from(&f.app, req).await);
    }
    let r = last.unwrap();
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error_code(&r), "500");
    assert!(r.retry_after.is_some_and(|v| v.parse::<u64>().unwrap() > 0));
}
