//! The logging canary (architecture §12.1) against the real binary, as
//! `tests/serve.rs` runs it: every event on through `PATCH
//! /settings/logging`, the API exercised over fixture data, and every
//! output read back (stderr, the JSONL file).
//!
//! 1. **Private:** no fixture hash (hex or base32), name, path, tracker
//!    passkey or peer address in any output.
//! 2. **One kind at a time:** only that kind's fixture values appear.
//! 3. **Opt-ins:** filter change details appear only while the opt-in is
//!    on, only in the file, and not once its time has run out.
//! 4. **Full plus both opt-ins:** no secret (the admin passwords, the
//!    session cookie, the setup token after setup, a tracker passkey, an
//!    API key as `Authorization: Bearer`, `X-Api-Key` and `?apikey=`, a
//!    feed token as `?token=`, the trusted-proxy user) and no sealed name
//!    in any output. Keys and tokens can't read `/logs/catalog`.
//!
//! B-log2 adds the API's outputs to every run: the ring buffer and the
//! files through `GET /logs` (paged to the end), the live stream (opened
//! before exercising, read over a socket, with one reconnect by
//! `Last-Event-ID`), every file downloaded as JSONL and text, redacted and
//! not, and the archive.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lodestar_metainfo::Info;
use lodestar_store::{Filtered, Held, NewTorrent, SourceId, Store, StoreConfig, WriteBatch};

const NAME: &str = "Canary.Fixture.Alpha.2026";
const FILE: &str = "alpha-disc-one.iso";
const SEALED_NAME: &str = "Canary.Sealed.Quarantined.Bravo";
const SEALED_HASH: [u8; 20] = [0x5e; 20];
const PASSKEY: &str = "CanaryPasskey0123456789abcdef0001";
const PEER: &str = "203.0.113.77";
const QUERY: &str = "Canary Fixture Alpha";
const PASSWORD: &str = "canary-first-password-1";
const PASSWORD_2: &str = "canary-second-password-2";
const WRONG: &str = "canary-wrong-password-3";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Bencode by hand: the canary needs only a dict, a list, ints and bytes.
fn info_dict() -> Vec<u8> {
    let s = |x: &str| format!("{}:{x}", x.len());
    let mut v = Vec::new();
    v.extend(
        format!(
            "d5:filesld6:lengthi1048576e4:pathl{}eed6:lengthi2048e4:pathl{}{}eee4:name{}12:piece lengthi65536e6:pieces20:",
            s(FILE),
            s("Subs"),
            s("en.srt"),
            s(NAME)
        )
        .bytes(),
    );
    v.extend([0u8; 20]);
    v.push(b'e');
    v
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn base32(b: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let (mut out, mut buf, mut bits) = (String::new(), 0u32, 0);
    for &x in b {
        buf = (buf << 8) | u32::from(x);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(A[((buf >> bits) & 31) as usize] as char);
        }
    }
    out
}

/// A data folder with one stored torrent and one quarantined, its name
/// sealed under the folder's own key.
fn seed(dir: &Path) -> [u8; 20] {
    let key = [7u8; 32];
    std::fs::write(dir.join("seal.key"), key).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.join("seal.key"), std::fs::Permissions::from_mode(0o600))
            .unwrap();
    }
    let raw = info_dict();
    let info = Info::parse(&raw).unwrap();
    let hash = info.primary_hash().0;
    let mut store = Store::open(dir.join("lodestar.db"), StoreConfig::default()).unwrap();
    store
        .insert(&[NewTorrent {
            info: &info,
            raw_info: Some(&raw),
            discovered_at: now() - 100,
            metadata_at: now() - 99,
            extra_flags: 0,
            source: SourceId::new(0).unwrap(),
        }])
        .unwrap();
    let sealed = lodestar_filter::Sealer::new(&key)
        .seal(SEALED_NAME)
        .unwrap();
    store
        .write(&WriteBatch {
            filtered: &[Filtered {
                hashes: &[SEALED_HASH],
                rule: "CS-2a",
                action: 3,
                held: Some(Held::Quarantine),
                sealed: Some(&sealed),
                evidence: &[2, 0],
                at: now() - 50,
                expires_at: now() + 86_400,
                adult_hold: false,
            }],
            ..WriteBatch::default()
        })
        .unwrap();
    hash
}

struct Server {
    dir: tempfile::TempDir,
    port: u16,
    child: Child,
    stderr: Arc<Mutex<String>>,
    token: String,
    hash: [u8; 20],
    cookie: String,
}

impl Server {
    fn start() -> Server {
        let dir = tempfile::tempdir().unwrap();
        let hash = seed(dir.path());
        let port = free_port();
        let mut child = Command::new(env!("CARGO_BIN_EXE_lodestar"))
            .env_remove("LODESTAR_LOG")
            .env("LODESTAR_DATA_DIR", dir.path())
            .env("LODESTAR_HTTP_BIND", format!("127.0.0.1:{port}"))
            .env("LODESTAR_DHT_PORT", "0")
            .env("LODESTAR_DHT_ENABLED", "false")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = Arc::new(Mutex::new(String::new()));
        let mut pipe = child.stderr.take().unwrap();
        let sink = stderr.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        let start = Instant::now();
        let token = loop {
            let text = stderr.lock().unwrap().clone();
            if let Some(rest) = text.split("setup token ").nth(1) {
                if let Some(t) = rest.split_whitespace().next() {
                    if text.contains('\n') && rest.contains('\n') {
                        break t.to_string();
                    }
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "no setup token: {text}"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        let mut s = Server {
            dir,
            port,
            child,
            stderr,
            token,
            hash,
            cookie: String::new(),
        };
        while s.get("/healthz").is_none() {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "server didn't come up"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let setup = s.post(
            "/api/v1/setup",
            &format!(r#"{{"token":"{}","password":"{PASSWORD}"}}"#, s.token),
        );
        assert!(setup.starts_with("HTTP/1.0 200"), "{setup}");
        s.cookie = cookie(&setup);
        s
    }

    fn request(&self, head: &str, body: &str) -> Option<String> {
        let mut c = TcpStream::connect(("127.0.0.1", self.port)).ok()?;
        c.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
        write!(
            c,
            "{head}\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .ok()?;
        let mut out = String::new();
        c.read_to_string(&mut out).ok()?;
        Some(out)
    }

    fn get(&self, path: &str) -> Option<String> {
        self.request(
            &format!("GET {path} HTTP/1.0\r\nCookie: {}", self.cookie),
            "",
        )
    }

    fn send(&self, method: &str, path: &str, body: &str) -> String {
        self.request(
            &format!(
                "{method} {path} HTTP/1.0\r\nContent-Type: application/json\r\nCookie: {}",
                self.cookie
            ),
            body,
        )
        .unwrap()
    }

    fn post(&self, path: &str, body: &str) -> String {
        self.send("POST", path, body)
    }

    /// `PATCH /settings/logging`, which must succeed.
    fn logging(&self, body: &str) {
        let r = self.send("PATCH", "/api/v1/settings/logging", body);
        assert!(r.starts_with("HTTP/1.0 200"), "{body}: {r}");
        // `follow` applies it on its own task.
        std::thread::sleep(Duration::from_millis(300));
    }

    /// Every event on: each one turned on against the preset.
    fn every_event_on(&self) {
        let catalog = self.get("/api/v1/logs/catalog").unwrap();
        let body = catalog.split("\r\n\r\n").nth(1).unwrap();
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        let mut events = serde_json::Map::new();
        for e in v["events"].as_array().unwrap() {
            if e["required"] == false && e["opt_in"].is_null() {
                events.insert(e["id"].as_str().unwrap().to_string(), true.into());
            }
        }
        self.logging(
            &serde_json::json!({ "events": events, "verbosity": "everything" }).to_string(),
        );
    }

    fn file(&self) -> String {
        std::fs::read_to_string(self.logs().join("lodestar.jsonl")).unwrap_or_default()
    }

    fn logs(&self) -> PathBuf {
        self.dir.path().join("logs")
    }

    fn console(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    /// Where each output is now, to read only what comes after.
    fn mark(&self) -> (usize, usize) {
        (self.console().len(), self.file().len())
    }

    fn since(&self, mark: (usize, usize)) -> (String, String) {
        (
            self.console()[mark.0..].to_string(),
            self.file()[mark.1..].to_string(),
        )
    }

    /// Searches, detail, filters (a rule, a subscription whose URL carries
    /// a passkey) and, with `secrets`, revealing a sealed name and a failed
    /// sign-in (both spend the per-address password bucket, burst 5).
    fn exercise(&self, n: u32, secrets: bool) {
        let q = QUERY.replace(' ', "%20");
        let r = self.get(&format!("/api/v1/search?q={q}")).unwrap();
        assert!(r.starts_with("HTTP/1.0 200"), "{r}");
        let h = hex(&self.hash);
        let r = self.get(&format!("/api/v1/torrents/{h}")).unwrap();
        assert!(r.starts_with("HTTP/1.0 200") && r.contains(NAME), "{r}");
        let r = self.get(&format!("/api/v1/torrents/{h}/files")).unwrap();
        assert!(r.starts_with("HTTP/1.0 200"), "{r}");
        self.get("/api/v1/filters").unwrap();
        self.get("/api/v1/filters/queue").unwrap();
        let r = self.post(
            "/api/v1/filters/apply",
            &format!(r#"{{"change":{{"op":"add_rule","query":"canaryterm{n}","action":"hide"}}}}"#),
        );
        assert!(r.starts_with("HTTP/1.0 200"), "{r}");
        let r = self.post(
            "/api/v1/filters/apply",
            &format!(
                r#"{{"change":{{"op":"subscribe","url":"http://127.0.0.1:9/{PASSKEY}/list{n}.txt?passkey={PASSKEY}","kind":"content"}}}}"#
            ),
        );
        assert!(r.starts_with("HTTP/1.0 200"), "{r}");
        if !secrets {
            return;
        }
        let r = self.post(
            "/api/v1/filters/apply",
            &format!(
                r#"{{"change":{{"op":"reveal_name","hash":"{}"}},"password":"{}"}}"#,
                hex(&SEALED_HASH),
                PASSWORD
            ),
        );
        assert!(r.starts_with("HTTP/1.0 200"), "{r}");
        let r = self.request(
            "POST /api/v1/login HTTP/1.0\r\nContent-Type: application/json",
            &format!(r#"{{"password":"{WRONG}"}}"#),
        );
        assert!(r.unwrap().starts_with("HTTP/1.0 401"));
    }

    fn stop(mut self) -> (String, String) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        let _ = self.child.wait();
        std::thread::sleep(Duration::from_millis(100));
        (self.console(), self.file())
    }
}

/// A live log over a socket (HTTP/1.0, so the body isn't chunked), read
/// on its own thread.
struct Stream {
    text: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    socket: TcpStream,
}

impl Stream {
    /// Opens one, waiting out a `429` while an earlier stream's slot is
    /// still being released.
    fn open(s: &Server, query: &str, last: Option<&str>) -> Stream {
        let start = Instant::now();
        loop {
            let out = Stream::try_open(s, query, last);
            loop {
                let text = out.text();
                if text.contains("retry: 5000") {
                    return out;
                }
                if text.starts_with("HTTP/1.0 429") {
                    break;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(20),
                    "no stream: {text}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            drop(out);
            std::thread::sleep(Duration::from_millis(300));
        }
    }

    fn try_open(s: &Server, query: &str, last: Option<&str>) -> Stream {
        let mut c = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
        c.set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let last = last.map_or(String::new(), |l| format!("\r\nLast-Event-ID: {l}"));
        write!(
            c,
            "GET /api/v1/logs/stream{query} HTTP/1.0\r\nHost: localhost\r\nCookie: {}{last}\r\n\r\n",
            s.cookie
        )
        .unwrap();
        let socket = c.try_clone().unwrap();
        let text = Arc::new(Mutex::new(String::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (t, st) = (text.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while !st.load(Ordering::Relaxed) {
                match c.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => t
                        .lock()
                        .unwrap()
                        .push_str(&String::from_utf8_lossy(&buf[..n])),
                    Err(_) => continue,
                }
            }
        });
        Stream { text, stop, socket }
    }

    fn text(&self) -> String {
        self.text.lock().unwrap().clone()
    }

    fn last_id(&self) -> Option<String> {
        self.text()
            .lines()
            .rev()
            .find_map(|l| l.strip_prefix("id: "))
            .map(str::to_string)
    }

    fn first_id(&self) -> Option<String> {
        self.text()
            .lines()
            .find_map(|l| l.strip_prefix("id: "))
            .map(str::to_string)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
    }
}

fn body(r: &str) -> &str {
    r.split("\r\n\r\n").nth(1).unwrap_or("")
}

/// A zip's members, inflated (stored or deflated), from its central
/// directory.
fn unzip(zip: &[u8]) -> Vec<(String, String)> {
    let u16_at = |i: usize| u16::from_le_bytes([zip[i], zip[i + 1]]) as usize;
    let u32_at = |i: usize| u32::from_le_bytes(zip[i..i + 4].try_into().unwrap()) as usize;
    let end = zip.len() - 22;
    assert_eq!(u32_at(end), 0x0605_4b50, "a zip");
    let mut at = u32_at(end + 16);
    let mut out = Vec::new();
    for _ in 0..u16_at(end + 10) {
        let csize = u32_at(at + 20);
        let nlen = u16_at(at + 28);
        let local = u32_at(at + 42);
        let name = String::from_utf8_lossy(&zip[at + 46..at + 46 + nlen]).into_owned();
        let data = local + 30 + u16_at(local + 26) + u16_at(local + 28);
        let mut plain = String::new();
        flate2::read::DeflateDecoder::new(&zip[data..data + csize])
            .read_to_string(&mut plain)
            .unwrap();
        out.push((name, plain));
        at += 46 + nlen + u16_at(at + 30) + u16_at(at + 32);
    }
    out
}

/// What the API shows, each output by name.
struct Api {
    outputs: Vec<(String, String)>,
}

impl Api {
    fn get(&self, name: &str) -> &str {
        &self
            .outputs
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("no {name}"))
            .1
    }

    fn all(&self) -> Vec<(&str, &str)> {
        self.outputs
            .iter()
            .map(|(n, t)| (n.as_str(), t.as_str()))
            .collect()
    }

    /// Every output but the unredacted downloads.
    fn redacted(&self) -> Vec<(&str, &str)> {
        self.all()
            .into_iter()
            .filter(|(n, _)| !n.contains("redact=false"))
            .collect()
    }
}

impl Server {
    /// `GET /logs`, paged to the end.
    fn history(&self) -> String {
        let mut out = String::new();
        let mut cursor: Option<String> = None;
        for _ in 0..1000 {
            let q = cursor
                .as_ref()
                .map_or(String::new(), |c| format!("&cursor={c}"));
            let r = self.get(&format!("/api/v1/logs?limit=1000{q}")).unwrap();
            assert!(r.starts_with("HTTP/1.0 200"), "{r}");
            let v: serde_json::Value = serde_json::from_str(body(&r)).unwrap();
            out.push_str(body(&r));
            out.push('\n');
            match v["next_cursor"].as_str() {
                Some(c) => cursor = Some(c.to_string()),
                None => return out,
            }
        }
        panic!("history didn't end");
    }

    fn files(&self) -> serde_json::Value {
        let r = self.get("/api/v1/logs/files").unwrap();
        assert!(r.starts_with("HTTP/1.0 200"), "{r}");
        serde_json::from_str(body(&r)).unwrap()
    }

    fn raw_get(&self, path: &str) -> Vec<u8> {
        let mut c = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(
            c,
            "GET {path} HTTP/1.0\r\nHost: localhost\r\nCookie: {}\r\n\r\n",
            self.cookie
        )
        .unwrap();
        let mut out = Vec::new();
        c.read_to_end(&mut out).unwrap();
        let at = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        assert!(
            out.starts_with(b"HTTP/1.0 200"),
            "{path}: {}",
            String::from_utf8_lossy(&out)
        );
        out[at + 4..].to_vec()
    }

    /// Every API output: history, `stream` (as read so far), every file
    /// in both formats redacted and not, and the archive both ways.
    fn api(&self, stream: &Stream) -> Api {
        let mut outputs = vec![
            ("history".to_string(), self.history()),
            ("stream".to_string(), stream.text()),
        ];
        let files = self.files();
        for f in files["files"].as_array().unwrap() {
            let name = f["name"].as_str().unwrap();
            for format in ["jsonl", "text"] {
                for redact in ["true", "false"] {
                    let q = format!("/api/v1/logs/files/{name}?format={format}&redact={redact}");
                    let text = String::from_utf8_lossy(&self.raw_get(&q)).into_owned();
                    outputs.push((format!("{name} {format} redact={redact}"), text));
                }
            }
        }
        for redact in ["true", "false"] {
            let zip = self.raw_get(&format!("/api/v1/logs/archive.zip?redact={redact}"));
            let all: String = unzip(&zip).into_iter().map(|(_, t)| t).collect();
            outputs.push((format!("archive redact={redact}"), all));
        }
        Api { outputs }
    }
}

fn cookie(response: &str) -> String {
    response
        .lines()
        .find_map(|l| l.strip_prefix("set-cookie: "))
        .and_then(|c| c.split(';').next())
        .expect("a session cookie")
        .to_string()
}

/// The fixture's identifying values, by the kind that may show them.
fn identifiers(hash: &[u8; 20]) -> Vec<(&'static str, String)> {
    vec![
        ("hash", hex(hash)),
        ("hash", base32(hash)),
        ("hash", hex(&SEALED_HASH)),
        ("name", NAME.to_string()),
        ("path", FILE.to_string()),
        ("query", QUERY.to_string()),
        ("query", "Canary%20Fixture".to_string()),
        ("peer", PEER.to_string()),
    ]
}

#[track_caller]
fn absent(outputs: &[(&str, &str)], needles: &[String], why: &str) {
    for (output, text) in outputs {
        for n in needles {
            if let Some(i) = text.find(n.as_str()) {
                let before = text[..i].rsplit('\n').next().unwrap_or("");
                let after = text[i..].split('\n').next().unwrap_or("");
                let line = format!("{before}{after}");
                panic!("{why}: {n:?} in the {output}: {line}");
            }
        }
    }
}

#[test]
fn run_1_private_shows_no_fixture_identifier() {
    let s = Server::start();
    s.every_event_on();
    let stream = Stream::open(&s, "", None);
    let mark = s.mark();
    s.exercise(1, true);
    std::thread::sleep(Duration::from_millis(300));
    let (console, file) = s.since(mark);
    let mut needles: Vec<String> = identifiers(&s.hash).into_iter().map(|(_, v)| v).collect();
    needles.extend([PASSKEY.to_string(), SEALED_NAME.to_string()]);
    // One reconnect by `Last-Event-ID`: no gap, so no resync.
    let last = stream.last_id().expect("the stream got lines");
    let first = stream.first_id().unwrap();
    drop(stream);
    s.exercise(2, false);
    let again = Stream::open(&s, "", Some(&last));
    std::thread::sleep(Duration::from_millis(500));
    assert!(!again.text().contains("event: resync"), "{}", again.text());
    assert!(
        again.text().contains("filters.rule_set_updated"),
        "it caught up"
    );
    let replay = Stream::open(&s, &format!("?after={first}"), None);
    std::thread::sleep(Duration::from_millis(300));
    let api = s.api(&replay);
    absent(&api.all(), &needles, "Private, through the API");
    absent(
        &[("reconnected stream", &again.text())],
        &needles,
        "Private",
    );
    assert!(api.get("history").contains("filters.rule_set_updated"));
    let (all_console, all_file) = s.stop();
    // Every line, start-up included, and only Private kinds.
    absent(
        &[("console", &all_console), ("file", &all_file)],
        &needles,
        "Private",
    );
    // The run did log: the events were on, and the file has the start-up.
    assert!(
        file.contains("\"event\":\"filters.rule_set_updated\""),
        "{file}"
    );
    assert!(all_console.contains("settings.saved"), "{all_console}");
    assert!(!console.is_empty());
    assert!(
        all_file.contains("\"event\":\"app.start\""),
        "start-up lines reach the file"
    );
    // A subscription's URL is hidden with URLs off.
    assert!(!all_file.contains("127.0.0.1:9/"), "{all_file}");
}

#[test]
fn run_2_one_kind_at_a_time_shows_only_that_kind() {
    let s = Server::start();
    s.every_event_on();
    let ids = identifiers(&s.hash);
    let open = Stream::open(&s, "", None);
    for (n, kind) in ["hash", "name", "path", "url", "query", "peer"]
        .into_iter()
        .enumerate()
    {
        s.logging(&format!(r#"{{"kinds":{{"{kind}":{}}}}}"#, now() + 600));
        let mark = s.mark();
        let stream_mark = open.text().len();
        s.exercise(10 + n as u32, n == 0);
        std::thread::sleep(Duration::from_millis(300));
        let (console, file) = s.since(mark);
        let others: Vec<String> = ids
            .iter()
            .filter(|(k, _)| *k != kind)
            .map(|(_, v)| v.clone())
            .chain([PASSKEY.to_string(), SEALED_NAME.to_string()])
            .collect();
        absent(&[("console", &console), ("file", &file)], &others, kind);
        // A URL that shows never carries its passkey (the list refresh
        // that logs one runs on its own minute; `PASSKEY` is in `others`
        // for every kind, URLs included).
        // The console stays Private whatever the file has.
        let mine: Vec<String> = ids
            .iter()
            .filter(|(k, _)| *k == kind)
            .map(|(_, v)| v.clone())
            .collect();
        absent(&[("console", &console)], &mine, "the console stays Private");
        // The API shows at most this kind while it's on, and redacted
        // downloads none at all.
        let replay = Stream::open(&s, "", None);
        let api = s.api(&replay);
        let stream_now = open.text()[stream_mark..].to_string();
        // (The unredacted downloads hold what earlier kinds wrote.)
        absent(&api.redacted(), &others, kind);
        absent(&[("open stream", &stream_now)], &others, kind);
        let redacted: Vec<(&str, &str)> = api
            .redacted()
            .into_iter()
            .filter(|(n, _)| *n != "history" && *n != "stream")
            .collect();
        absent(&redacted, &mine, "redacted downloads");
        drop(replay);
        let before_off = open.text().len();
        let first = open.first_id();
        s.logging(r#"{"kinds":{}}"#);
        // Off again: the open stream was told, and history and a new
        // stream replaying the ring show none of it.
        let start = Instant::now();
        while !open.text()[before_off..].contains("event: purge") {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "{kind}: no purge frame"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let purge = open.text()[before_off..].to_string();
        assert!(purge.contains(&format!("\"{kind}\"")), "{kind}: {purge}");
        let replay = Stream::open(
            &s,
            &first.map_or(String::new(), |f| format!("?after={f}")),
            None,
        );
        std::thread::sleep(Duration::from_millis(300));
        let api = s.api(&replay);
        let after_off: Vec<(&str, &str)> = api.redacted().into_iter().collect();
        absent(&after_off, &mine, &format!("{kind} after it's off"));
    }
    s.stop();
}

#[test]
fn run_3_opt_ins_appear_only_while_on_and_only_in_the_file() {
    let s = Server::start();
    s.every_event_on();
    let open = Stream::open(&s, "", None);
    // Off: no detail.
    let mark = s.mark();
    s.exercise(20, true);
    std::thread::sleep(Duration::from_millis(300));
    let (console, file) = s.since(mark);
    absent(
        &[
            ("console", &console),
            ("file", &file),
            ("stream", &open.text()),
        ],
        &["canaryterm20".to_string()],
        "opt-in off",
    );
    // On for a few seconds: in the file and the live log, never the
    // console.
    s.logging(&format!(r#"{{"change_details_until":{}}}"#, now() + 4));
    let mark = s.mark();
    s.exercise(21, false);
    std::thread::sleep(Duration::from_millis(300));
    let (console, file) = s.since(mark);
    assert!(
        file.contains("\"event\":\"filters.change_detail\"") && file.contains("canaryterm21"),
        "{file}"
    );
    assert!(open.text().contains("canaryterm21"), "the live log has it");
    assert!(s.history().contains("canaryterm21"), "history has it");
    absent(
        &[("console", &console)],
        &[
            "canaryterm21".to_string(),
            "filters.change_detail".to_string(),
        ],
        "never on the console",
    );
    // The passkey stays out of a change's detail too.
    absent(
        &[("file", &file)],
        &[PASSKEY.to_string(), SEALED_NAME.to_string()],
        "opt-in detail",
    );
    // Its time runs out with no save: the required event says so, and
    // later changes aren't recorded.
    let before_off = open.text().len();
    let first = open.first_id().unwrap();
    std::thread::sleep(Duration::from_secs(5));
    let mark = s.mark();
    s.exercise(22, false);
    std::thread::sleep(Duration::from_millis(300));
    let (console, file) = s.since(mark);
    absent(
        &[("console", &console), ("file", &file)],
        &["canaryterm22".to_string()],
        "expired",
    );
    // Gone from history and from a new stream; the open one was told.
    assert!(
        open.text()[before_off..].contains("event: purge")
            && open.text()[before_off..].contains("filter_changes"),
        "{}",
        &open.text()[before_off..]
    );
    let replay = Stream::open(&s, &format!("?after={first}"), None);
    std::thread::sleep(Duration::from_millis(300));
    let api = s.api(&replay);
    absent(
        &api.redacted(),
        &["canaryterm21".to_string(), "canaryterm22".to_string()],
        "after the opt-in's time",
    );
    // The positive control: the file written meanwhile keeps it until
    // it's deleted, unredacted, and it's badged.
    let unredacted: String = api
        .all()
        .iter()
        .filter(|(n, _)| n.starts_with("lodestar.jsonl jsonl redact=false"))
        .map(|(_, t)| *t)
        .collect();
    assert!(unredacted.contains("canaryterm21"), "{unredacted}");
    let files = s.files();
    let current = files["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["current"] == true)
        .unwrap();
    assert_eq!(current["details"], true, "{files}");
    assert_eq!(current["opt_in"], true, "{files}");
    let (all_console, all_file) = s.stop();
    for out in [&all_console, &all_file] {
        assert!(
            out.contains("filter_changes") && out.contains("expired"),
            "{out}"
        );
    }
    // Peer activity has no real event yet (the fetch and DHT lines come
    // with B-log1b): `logging::tests` holds its rendering per output.
}

#[test]
fn run_4_full_and_both_opt_ins_never_show_a_secret_or_a_sealed_name() {
    let mut s = Server::start();
    s.every_event_on();
    s.logging(&format!(
        r#"{{"data":"full","data_until":-1,"peer_activity_until":{},"change_details_until":{},"console_detail":"file"}}"#,
        now() + 3600,
        now() + 3600
    ));
    s.exercise(30, true);
    // A password change and a sign-in with the new one.
    let r = s.post(
        "/api/v1/password",
        &format!(r#"{{"old":"{PASSWORD}","new":"{PASSWORD_2}"}}"#),
    );
    assert!(r.starts_with("HTTP/1.0 200"), "{r}");
    let login = s.request(
        "POST /api/v1/login HTTP/1.0\r\nContent-Type: application/json",
        &format!(r#"{{"password":"{PASSWORD_2}"}}"#),
    );
    let login = login.unwrap();
    assert!(login.starts_with("HTTP/1.0 200"), "{login}");
    let first_cookie = s.cookie.clone();
    s.cookie = cookie(&login);
    let cookie_2 = s.cookie.clone();
    let open = Stream::open(&s, "", None);
    s.exercise(31, false);
    std::thread::sleep(Duration::from_millis(300));
    let token = s.token.clone();
    let api = s.api(&open);
    let (console, file) = s.stop();
    let value = |c: &str| c.split_once('=').map(|(_, v)| v.to_string()).unwrap();
    let mut outputs = vec![("console", console.as_str()), ("file", file.as_str())];
    outputs.extend(api.all());
    absent(
        &outputs,
        &[
            PASSWORD.to_string(),
            PASSWORD_2.to_string(),
            WRONG.to_string(),
            value(&first_cookie),
            value(&cookie_2),
            PASSKEY.to_string(),
            SEALED_NAME.to_string(),
        ],
        "Full plus both opt-ins",
    );
    // The setup token only in its start-up line, which stops working at
    // setup; through the API, only in an unredacted download.
    for out in [&console, &file] {
        for line in out.lines().filter(|l| l.contains(&token)) {
            assert!(line.contains("setup token"), "{line}");
        }
    }
    absent(
        &api.redacted(),
        std::slice::from_ref(&token),
        "the setup token",
    );
    for (name, out) in api.all() {
        for line in out.lines().filter(|l| l.contains(&token)) {
            assert!(line.contains("setup token"), "{name}: {line}");
        }
    }
    // Full did reach the file (the opt-in detail is there), and the
    // console never gets an opt-in.
    assert!(file.contains("canaryterm30"), "{file}");
    assert!(!console.contains("canaryterm30"), "{console}");
}

const PROXY_USER: &str = "canary-proxy-user-delta";

/// Run 4's second half (its own server: B12's key and proxy changes each
/// take the password, and the per-address bucket holds five): API keys as
/// `Authorization: Bearer`, `X-Api-Key` and `?apikey=`, a feed token as
/// `?token=`, and the trusted-proxy user, under Full plus both opt-ins.
#[test]
fn run_4_keys_tokens_and_the_proxy_user_never_show_either() {
    let s = Server::start();
    s.every_event_on();
    s.logging(&format!(
        r#"{{"data":"full","data_until":-1,"peer_activity_until":{},"change_details_until":{},"console_detail":"file"}}"#,
        now() + 3600,
        now() + 3600
    ));
    let secret = |body: &str| {
        let r = s.post("/api/v1/keys", body);
        assert!(
            r.starts_with("HTTP/1.0 200") || r.starts_with("HTTP/1.0 201"),
            "{r}"
        );
        let json = r.split("\r\n\r\n").nth(1).unwrap();
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        v["secret"].as_str().unwrap().to_string()
    };
    let key = secret(&format!(
        r#"{{"kind":"api_key","name":"canary key","scopes":["search"],"password":"{PASSWORD}"}}"#
    ));
    let token = secret(&format!(
        r#"{{"kind":"feed_token","name":"canary feed","password":"{PASSWORD}"}}"#
    ));
    // Keys can't read logs (§12.1), however they're presented, on any
    // `/logs` route.
    for (method, path) in [
        ("GET", "/api/v1/logs/catalog"),
        ("GET", "/api/v1/logs"),
        ("GET", "/api/v1/logs/stream"),
        ("GET", "/api/v1/logs/files"),
        ("GET", "/api/v1/logs/files/lodestar.jsonl"),
        ("GET", "/api/v1/logs/archive.zip"),
        ("DELETE", "/api/v1/logs/files"),
    ] {
        let json = "\r\nContent-Type: application/json";
        for head in [
            format!("{method} {path} HTTP/1.0\r\nAuthorization: Bearer {key}{json}"),
            format!("{method} {path} HTTP/1.0\r\nX-Api-Key: {key}{json}"),
            format!("{method} {path}?apikey={key} HTTP/1.0{json}"),
            format!("{method} {path}?token={token} HTTP/1.0{json}"),
        ] {
            let r = s.request(&head, r#"{"which":"all"}"#).unwrap();
            assert!(r.starts_with("HTTP/1.0 401"), "{head}: {r}");
            assert!(r.contains("login_required"), "{r}");
        }
    }
    for head in [
        format!("GET /api/v1/search?q=x HTTP/1.0\r\nAuthorization: Bearer {key}"),
        format!("GET /api/v1/search?q=x&apikey={key} HTTP/1.0"),
        format!("GET /api/v1/search?q=x&token={token} HTTP/1.0"),
        "GET /api/v1/search?q=x HTTP/1.0\r\nX-Api-Key: lsk_AAAAAAAA_notarealsecretnotarealsecretnotarealsec".to_string(),
    ] {
        s.request(&head, "").unwrap();
    }
    // The trusted-proxy user, from a trusted peer.
    let r = s.send(
        "PUT",
        "/api/v1/auth/proxy",
        &format!(r#"{{"enabled":true,"trusted":["127.0.0.1"],"password":"{PASSWORD}"}}"#),
    );
    assert!(r.starts_with("HTTP/1.0 200"), "{r}");
    let r = s
        .request(
            &format!("GET /api/v1/search?q=x HTTP/1.0\r\nRemote-User: {PROXY_USER}"),
            "",
        )
        .unwrap();
    assert!(
        r.starts_with("HTTP/1.0 200"),
        "the proxy user signs in: {r}"
    );
    // No password left in the bucket for a reveal: the other run 4 covers it.
    let open = Stream::open(&s, "", None);
    s.exercise(40, false);
    std::thread::sleep(Duration::from_millis(300));
    let api = s.api(&open);
    let (console, file) = s.stop();
    let mut outputs = vec![("console", console.as_str()), ("file", file.as_str())];
    outputs.extend(api.all());
    absent(
        &outputs,
        &[
            key.clone(),
            key[key.len() - 43..].to_string(),
            token.clone(),
            token[token.len() - 43..].to_string(),
            PROXY_USER.to_string(),
            PASSWORD.to_string(),
            PASSKEY.to_string(),
            SEALED_NAME.to_string(),
        ],
        "keys, tokens and the proxy user",
    );
}
