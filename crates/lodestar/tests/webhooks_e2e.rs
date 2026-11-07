//! Sonarr and Radarr webhooks through the real binary (SRC-11c): a token
//! made through the API, the app's calls authenticated by it, and a Grab
//! reaching the store through the pipeline's claims path as a private
//! unverified item. A wrong token never reaches the plugin; while the
//! source is off a real event is answered `200` dropped and the app's Test
//! call `503 source_off`.
#![cfg(unix)]

use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use lodestar_store::unverified::Network;
use lodestar_store::{Store, StoreConfig};
use serde_json::Value;

mod common;

const PASSWORD: &str = "correct horse battery";
/// The `downloadId` of `sonarr_grab.json`, and of `radarr_grab.json`.
const BUNNY: &str = "fb9ff1ef824d61bd83b24fb91811804db71c0c53";
const SINTEL: &str = "6cd8bae816112560c859c22a04238d8f22c552df";
/// Long enough for the slowest CI runner (Windows is excluded, but macOS
/// and loaded Linux runners are slow to start the binary).
const DEADLINE: Duration = Duration::from_secs(30);

/// The plugin crate's hand-written bodies in the apps' documented shapes.
fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../sources-plugins/fixtures/arr")
            .join(name),
    )
    .unwrap()
}

fn key(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

/// Waits for `f` to give a value, failing with `what` past the deadline.
fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < DEADLINE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

struct Server {
    port: u16,
    child: Child,
    log: Arc<Mutex<String>>,
    cookie: String,
}

impl Server {
    /// Starts the binary on `dir`: no DHT and no tracker scrapes, so
    /// nothing leaves the machine. The first start sets the password up;
    /// a later one logs in with it.
    fn start(dir: &Path, first: bool) -> Server {
        Server::start_with(dir, first, false)
    }

    /// As [`Server::start`]; `dht` runs the DHT too, on a loopback port
    /// with no bootstrap nodes, so it still contacts nobody.
    fn start_with(dir: &Path, first: bool, dht: bool) -> Server {
        if dht {
            std::fs::write(
                dir.join("config.toml"),
                "[dht]\nbind = [\"127.0.0.1:0\"]\nbootstrap = []\n",
            )
            .unwrap();
        }
        let mut child = Command::new(env!("CARGO_BIN_EXE_lodestar"))
            .env("LODESTAR_DATA_DIR", dir)
            .env("LODESTAR_HTTP_BIND", common::ANY_PORT)
            .env("LODESTAR_DHT_ENABLED", dht.to_string())
            .env("LODESTAR_DHT_PORT", "0")
            .env("LODESTAR_SCRAPE_ENABLED", "false")
            .env("LODESTAR_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let log = Arc::new(Mutex::new(String::new()));
        {
            let log = log.clone();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(stderr)
                    .lines()
                    .map_while(Result::ok)
                {
                    let mut l = log.lock().unwrap();
                    l.push_str(&line);
                    l.push('\n');
                }
            });
        }
        let port = wait_for("the serving port", || {
            common::serving_port(&log.lock().unwrap())
        });
        let mut s = Server {
            port,
            child,
            log,
            cookie: String::new(),
        };
        wait_for("health", || s.request("GET /healthz HTTP/1.0", ""));
        let r = if first {
            let token = wait_for("the setup token", || {
                let l = s.log.lock().unwrap();
                let rest = l.split("setup token ").nth(1)?;
                rest.split_whitespace().next().map(str::to_owned)
            });
            s.post(
                "/api/v1/setup",
                &format!(
                    r#"{{"token":"{token}","password":"{PASSWORD}","notice_version":{}}}"#,
                    lodestar_api::legal::NOTICE_VERSION
                ),
            )
        } else {
            s.post("/api/v1/login", &format!(r#"{{"password":"{PASSWORD}"}}"#))
        };
        assert!(r.starts_with("HTTP/1.0 200"), "{r}");
        s.cookie = r
            .lines()
            .find_map(|l| l.strip_prefix("set-cookie: "))
            .and_then(|c| c.split(';').next())
            .expect("a session cookie")
            .to_owned();
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
        let mut out = Vec::new();
        if c.read_to_end(&mut out).is_err() && out.is_empty() {
            return None;
        }
        Some(String::from_utf8_lossy(&out).into_owned())
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

    fn get_json(&self, path: &str) -> Value {
        let r = self.send("GET", path, "");
        assert!(r.starts_with("HTTP/1.0 200"), "{path}: {r}");
        serde_json::from_str(body(&r)).unwrap()
    }

    /// A call as Sonarr or Radarr makes it: no session, the token as Basic
    /// auth's password.
    fn webhook(&self, integration: &str, token: &str, event: &str) -> String {
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("lodestar:{token}"));
        self.request(
            &format!(
                "POST /api/v1/webhooks/{integration} HTTP/1.0\r\n\
                 Content-Type: application/json\r\nAuthorization: Basic {basic}"
            ),
            event,
        )
        .unwrap()
    }

    /// `GET /webhooks`'s counts for `integration`.
    fn counts(&self, integration: &str) -> Value {
        let list = self.get_json("/api/v1/webhooks");
        list["integrations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["integration"] == integration)
            .unwrap()
            .clone()
    }

    fn stop(mut self) -> String {
        let killed = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(killed.success());
        let status = self.child.wait().unwrap();
        let log = self.log.lock().unwrap().clone();
        assert!(status.success(), "{log}");
        log
    }
}

fn body(r: &str) -> &str {
    r.split("\r\n\r\n").nth(1).unwrap_or("")
}

fn status(r: &str) -> u16 {
    r.split_whitespace().nth(1).unwrap().parse().unwrap()
}

fn code(r: &str) -> String {
    let v: Value = serde_json::from_str(body(r)).unwrap();
    v["error"]["code"].as_str().unwrap_or_default().to_owned()
}

#[test]
fn a_grab_with_the_token_reaches_the_store_private_and_a_wrong_one_never_does() {
    let dir = tempfile::tempdir().unwrap();
    let grab = fixture("sonarr_grab.json");
    let test = fixture("sonarr_test.json");

    // First start: the source is listed, off by default.
    let s = Server::start(dir.path(), true);
    let sources = s.get_json("/api/v1/sources");
    let source = sources
        .as_array()
        .or_else(|| sources["sources"].as_array())
        .unwrap()
        .iter()
        .find(|x| x["plugin"] == "arr_webhooks")
        .expect("the webhooks source is registered")
        .clone();
    assert_eq!(source["enabled"], false, "{source}");
    // Its finds reflect your own setup: never named to a public viewer.
    assert_eq!(source["finds_private"], true, "{source}");
    let id = source["id"].as_u64().unwrap();

    // A token, made with the password and shown once.
    let r = s.post(
        "/api/v1/webhooks/sonarr/token",
        &format!(r#"{{"password":"{PASSWORD}"}}"#),
    );
    assert_eq!(status(&r), 200, "{r}");
    let token: Value = serde_json::from_str(body(&r)).unwrap();
    let token = token["token"].as_str().unwrap().to_owned();
    assert!(token.starts_with("lsw_"));

    // The source is off: a real event is dropped with a 200, so the app
    // doesn't back off; its Test call is told, so the button shows it.
    let r = s.webhook("sonarr", &token, &grab);
    assert_eq!(status(&r), 200, "{r}");
    let v: Value = serde_json::from_str(body(&r)).unwrap();
    assert_eq!(
        (v["found"].clone(), v["dropped"].clone()),
        (false.into(), true.into()),
        "{r}"
    );
    let r = s.webhook("sonarr", &token, &test);
    assert_eq!(
        (status(&r), code(&r)),
        (503, "source_off".to_owned()),
        "{r}"
    );
    // Both are counted as refused: authenticated, not delivered.
    let counts = s.counts("sonarr");
    assert_eq!(counts["refused"], 2, "{counts}");

    // Enabled, it runs from the next start.
    let r = s.send(
        "PATCH",
        &format!("/api/v1/sources/{id}"),
        r#"{"enabled":true}"#,
    );
    assert_eq!(status(&r), 200, "{r}");
    s.stop();

    let s = Server::start(dir.path(), false);
    wait_for("the source running", || {
        let v = s.get_json(&format!("/api/v1/sources/{id}"));
        (v["health"]["running"] == true).then_some(())
    });
    // Every failure to authenticate is the same 401, and none reaches the
    // plugin: a wrong token, another integration's, none at all.
    let wrong = format!("lsw_{}", "A".repeat(43));
    let radarr_grab = fixture("radarr_grab.json");
    for r in [
        s.webhook("sonarr", &wrong, &grab),
        s.webhook("radarr", &token, &radarr_grab),
        s.request(
            "POST /api/v1/webhooks/radarr HTTP/1.0\r\nContent-Type: application/json",
            &radarr_grab,
        )
        .unwrap(),
    ] {
        assert_eq!(
            (status(&r), code(&r)),
            (401, "login_required".to_owned()),
            "{r}"
        );
    }

    // The right token: found, and through the pipeline to the store.
    let r = s.webhook("sonarr", &token, &grab);
    assert_eq!(status(&r), 200, "{r}");
    let v: Value = serde_json::from_str(body(&r)).unwrap();
    assert_eq!(
        (v["found"].clone(), v["dropped"].clone()),
        (true.into(), false.into()),
        "{r}"
    );
    // Running, the Test call is a plain success.
    let r = s.webhook("sonarr", &token, &test);
    assert_eq!(status(&r), 200, "{r}");
    assert_eq!(body(&r), r#"{"found":false,"dropped":false}"#, "{r}");
    // A body that isn't an event: refused and counted, never delivered.
    let r = s.webhook("sonarr", &token, "[]");
    assert_eq!(
        (status(&r), code(&r)),
        (400, "malformed_webhook".to_owned()),
        "{r}"
    );
    let counts = s.counts("sonarr");
    for (k, n) in [
        ("found", 1),
        ("ignored", 1),
        ("malformed", 1),
        ("refused", 0),
    ] {
        assert_eq!(counts[k], n, "{k}: {counts}");
    }
    // The refused calls above never reached Radarr's side at all.
    let radarr = s.counts("radarr");
    for k in ["found", "ignored", "malformed", "refused"] {
        assert_eq!(radarr[k], 0, "{k}: {radarr}");
    }
    // Radarr with its own token: its route parses as Radarr.
    let r = s.post(
        "/api/v1/webhooks/radarr/token",
        &format!(r#"{{"password":"{PASSWORD}"}}"#),
    );
    assert_eq!(status(&r), 200, "{r}");
    let radarr_token: Value = serde_json::from_str(body(&r)).unwrap();
    let radarr_token = radarr_token["token"].as_str().unwrap().to_owned();
    let r = s.webhook("radarr", &radarr_token, &radarr_grab);
    assert_eq!(status(&r), 200, "{r}");
    assert_eq!(body(&r), r#"{"found":true,"dropped":false}"#, "{r}");

    let db = dir.path().join("lodestar.db");
    let item = wait_for("the grab's unverified item", || {
        let store = Store::open_reader(&db, StoreConfig::default()).ok()?;
        store.unverified(Network::Bt, &key(BUNNY)).ok()?
    });
    // §18.7: it came through the user's own indexers, which may be
    // private trackers, so it's private and its claims are too.
    assert!(item.private_source, "{item:?}");
    assert!(item.claims_private, "{item:?}");
    let claims = item.claims.as_ref().expect("its claims");
    assert_eq!(
        claims.name.as_deref(),
        Some("Example.Open.Series.S01E01.1080p.WEB-DL-OPEN")
    );
    assert_eq!(claims.size, Some(1_503_238_553));
    // The series' ids: read from `series`, so the route mapped to Sonarr.
    let has = |c: &lodestar_store::claims::Claims, scheme: &str, value: &str| {
        c.ids.iter().any(|i| i.scheme == scheme && i.value == value)
    };
    assert!(has(claims, "tvdb", "81189"), "{claims:?}");
    // Radarr's: read from `movie`.
    let item = wait_for("Radarr's grab", || {
        let store = Store::open_reader(&db, StoreConfig::default()).ok()?;
        store.unverified(Network::Bt, &key(SINTEL)).ok()?
    });
    assert!(item.private_source, "{item:?}");
    let claims = item.claims.as_ref().expect("its claims");
    assert!(has(claims, "tmdb", "45745"), "{claims:?}");

    let log = s.stop();
    assert!(
        !log.contains(&token) && !log.contains(&radarr_token),
        "a token is never logged"
    );
}

/// G1 of the v0.5.2 manifest (`docs/plan/2026-11-06-v052-feature-manifest.md`
/// §3.3): a release build registers exactly three sources, the DHT, the
/// torrent cache and the Sonarr/Radarr webhooks, and only the DHT starts
/// on. Every other plugin in `sources-plugins` is compiled in but must stay
/// unregistered; registering one changes what v0.5.2 ships, so it fails
/// here until the manifest and this list say so.
#[test]
fn the_binary_registers_only_the_dht_the_torrent_cache_and_the_webhooks() {
    let dir = tempfile::tempdir().unwrap();
    let s = Server::start_with(dir.path(), true, true);
    let sources = s.get_json("/api/v1/sources");
    let list = sources
        .as_array()
        .or_else(|| sources["sources"].as_array())
        .unwrap_or_else(|| panic!("a list of sources: {sources}"));
    let mut seen: Vec<(String, bool)> = list
        .iter()
        .map(|x| {
            (
                x["plugin"].as_str().unwrap_or_default().to_owned(),
                x["enabled"].as_bool().unwrap_or_else(|| panic!("{x}")),
            )
        })
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        [
            ("arr_webhooks".to_owned(), false),
            ("dht".to_owned(), true),
            ("torrent_cache".to_owned(), false),
        ],
        "{sources}"
    );
    s.stop();
}
