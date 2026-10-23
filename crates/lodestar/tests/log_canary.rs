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
//! B-log2 adds the ring buffer, the live stream and the download to the
//! outputs read here.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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
    let mark = s.mark();
    s.exercise(1, true);
    std::thread::sleep(Duration::from_millis(300));
    let (console, file) = s.since(mark);
    let mut needles: Vec<String> = identifiers(&s.hash).into_iter().map(|(_, v)| v).collect();
    needles.extend([PASSKEY.to_string(), SEALED_NAME.to_string()]);
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
    for (n, kind) in ["hash", "name", "path", "url", "query", "peer"]
        .into_iter()
        .enumerate()
    {
        s.logging(&format!(r#"{{"kinds":{{"{kind}":{}}}}}"#, now() + 600));
        let mark = s.mark();
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
        s.logging(r#"{"kinds":{}}"#);
    }
    s.stop();
}

#[test]
fn run_3_opt_ins_appear_only_while_on_and_only_in_the_file() {
    let s = Server::start();
    s.every_event_on();
    // Off: no detail.
    let mark = s.mark();
    s.exercise(20, true);
    std::thread::sleep(Duration::from_millis(300));
    let (console, file) = s.since(mark);
    absent(
        &[("console", &console), ("file", &file)],
        &["canaryterm20".to_string()],
        "opt-in off",
    );
    // On for a few seconds: in the file, never the console.
    s.logging(&format!(r#"{{"change_details_until":{}}}"#, now() + 4));
    let mark = s.mark();
    s.exercise(21, false);
    std::thread::sleep(Duration::from_millis(300));
    let (console, file) = s.since(mark);
    assert!(
        file.contains("\"event\":\"filters.change_detail\"") && file.contains("canaryterm21"),
        "{file}"
    );
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
    s.exercise(31, false);
    std::thread::sleep(Duration::from_millis(300));
    let token = s.token.clone();
    let (console, file) = s.stop();
    let value = |c: &str| c.split_once('=').map(|(_, v)| v.to_string()).unwrap();
    absent(
        &[("console", &console), ("file", &file)],
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
    // setup.
    for out in [&console, &file] {
        for line in out.lines().filter(|l| l.contains(&token)) {
            assert!(line.contains("setup token"), "{line}");
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
    // Keys can't read logs (§12.1), however they're presented.
    for head in [
        format!("GET /api/v1/logs/catalog HTTP/1.0\r\nAuthorization: Bearer {key}"),
        format!("GET /api/v1/logs/catalog HTTP/1.0\r\nX-Api-Key: {key}"),
        format!("GET /api/v1/logs/catalog?apikey={key} HTTP/1.0"),
        format!("GET /api/v1/logs/catalog?token={token} HTTP/1.0"),
    ] {
        let r = s.request(&head, "").unwrap();
        assert!(
            r.starts_with("HTTP/1.0 401") || r.starts_with("HTTP/1.0 403"),
            "{head}: {r}"
        );
        assert!(!r.contains("\"events\""), "{r}");
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
    s.exercise(40, false);
    std::thread::sleep(Duration::from_millis(300));
    let (console, file) = s.stop();
    absent(
        &[("console", &console), ("file", &file)],
        &[
            key.clone(),
            key.split('_').next_back().unwrap().to_string(),
            token.clone(),
            token.split('_').next_back().unwrap().to_string(),
            PROXY_USER.to_string(),
            PASSWORD.to_string(),
            PASSKEY.to_string(),
            SEALED_NAME.to_string(),
        ],
        "keys, tokens and the proxy user",
    );
}
