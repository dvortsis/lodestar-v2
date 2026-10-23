//! `lodestar serve --fixture` through the real binary, as `tests/serve.rs`
//! runs it: search, facets, detail and Discover answer what the manifest
//! says, with Adult hidden and shown; a foreign folder is refused
//! untouched; the DHT and the fetch pool never run; a temporary fixture is
//! removed on a clean exit.
#![cfg(all(unix, feature = "fixture"))]

use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use lodestar_bench::fixture as data;
use serde_json::Value;

const PASSWORD: &str = "correct horse battery";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn request(port: u16, head: &str, body: &str) -> Option<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(30))).ok()?;
    write!(
        s,
        "{head}\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .ok()?;
    let mut out = String::new();
    s.read_to_string(&mut out).ok()?;
    Some(out)
}

/// A request's JSON body; panics unless it's a 200.
fn json(port: u16, path: &str, cookie: &str) -> Value {
    let r = request(
        port,
        &format!("GET {path} HTTP/1.0\r\nCookie: {cookie}"),
        "",
    )
    .unwrap();
    assert!(r.starts_with("HTTP/1.0 200"), "{path}: {r}");
    let body = r.split_once("\r\n\r\n").unwrap().1;
    serde_json::from_str(body).unwrap_or_else(|e| panic!("{path}: {e}: {body}"))
}

/// Percent-encodes a query value.
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A running `serve --fixture`, with its log collected.
struct Server {
    child: Child,
    port: u16,
    log: std::thread::JoinHandle<String>,
}

fn spawn(args: &[&str], env: &[(&str, &str)]) -> Server {
    let port = free_port();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lodestar"));
    cmd.arg("serve")
        .args(args)
        .env("LODESTAR_HTTP_BIND", format!("127.0.0.1:{port}"))
        .env("LODESTAR_LOG", "info")
        .env_remove("LODESTAR_DATA_DIR")
        .env_remove("LODESTAR_SHOW_ADULT")
        .env_remove("LODESTAR_FIXTURE_PASSWORD");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let log = std::thread::spawn(move || {
        let mut all = String::new();
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            all.push_str(&line);
            all.push('\n');
        }
        all
    });
    Server { child, port, log }
}

impl Server {
    /// Waits for `/healthz` (the first start builds the fixture).
    fn ready(&mut self) {
        let start = Instant::now();
        loop {
            if let Some(r) = request(self.port, "GET /healthz HTTP/1.0", "") {
                assert!(r.starts_with("HTTP/1.0 200"), "{r}");
                return;
            }
            if let Some(exit) = self.child.try_wait().unwrap() {
                panic!("exited {exit:?}");
            }
            assert!(start.elapsed() < Duration::from_secs(180), "didn't come up");
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn login(&self) -> String {
        let r = request(
            self.port,
            "POST /api/v1/login HTTP/1.0\r\nContent-Type: application/json",
            &format!(r#"{{"password":"{PASSWORD}"}}"#),
        )
        .unwrap();
        assert!(r.starts_with("HTTP/1.0 200"), "{r}");
        r.lines()
            .find_map(|l| l.strip_prefix("set-cookie: "))
            .and_then(|c| c.split(';').next())
            .expect("session cookie")
            .to_string()
    }

    /// SIGTERM; returns the log after a clean exit.
    fn stop(mut self) -> String {
        let killed = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(killed.success());
        let exit = self.child.wait().unwrap();
        let log = self.log.join().unwrap();
        assert!(exit.success(), "exit {exit:?}: {log}");
        log
    }
}

fn manifest(dir: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join(data::MANIFEST)).unwrap()).unwrap()
}

fn u(v: &Value) -> u64 {
    v.as_u64().unwrap_or_else(|| panic!("not a count: {v}"))
}

/// The top `values.len()` of an expected facet, by count then value.
fn top(expected: &serde_json::Map<String, Value>, n: usize) -> Vec<(String, u64)> {
    let mut all: Vec<(String, u64)> = expected.iter().map(|(k, v)| (k.clone(), u(v))).collect();
    all.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    all.truncate(n);
    all
}

/// Every query of the manifest's set with Adult `adult`: totals and the
/// `type` and `ext` facets.
fn check_queries(port: u16, cookie: &str, m: &Value, adult: bool) {
    let mut checked = 0;
    for q in m["queries"].as_array().unwrap() {
        if q["adult"].as_bool() != Some(adult) {
            continue;
        }
        let params = format!(
            "q={}&hidden={}",
            enc(q["q"].as_str().unwrap()),
            q["hidden"].as_bool().unwrap()
        );
        let search = json(port, &format!("/api/v1/search?{params}&limit=1"), cookie);
        assert_eq!(search["total"], q["total"], "search {params} adult={adult}");
        assert_eq!(search["total_approx"], false);
        let facets = json(port, &format!("/api/v1/facets?{params}"), cookie);
        assert_eq!(facets["total"], q["total"], "facets {params}");
        for name in ["type", "ext"] {
            let want = q["facets"][name].as_object().unwrap();
            let got = &facets["facets"][name];
            let values = got["values"].as_array().unwrap();
            let listed: Vec<(String, u64)> = values
                .iter()
                .map(|v| (v["value"].as_str().unwrap().to_owned(), u(&v["count"])))
                .collect();
            assert_eq!(
                listed,
                top(want, values.len()),
                "{name} for {params} adult={adult}"
            );
            assert_eq!(
                u(&got["distinct"]),
                want.len() as u64,
                "{name} distinct for {params}"
            );
            let listed_sum: u64 = listed.iter().map(|(_, n)| n).sum();
            let all: u64 = want.values().map(u).sum();
            assert_eq!(
                u(&got["other"]),
                all - listed_sum,
                "{name} other for {params}"
            );
        }
        checked += 1;
    }
    assert_eq!(checked, 5);
}

const SWARM: [&str; 6] = [
    "seeders",
    "leechers",
    "completed",
    "trackers_reporting",
    "trackers_checked",
    "last_scrape_at",
];

#[test]
fn serves_what_the_manifest_says() {
    let root = tempfile::tempdir().unwrap();
    // Reachable by PUID/PGID if started as root.
    std::fs::set_permissions(
        root.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let dir = root.path().join("fx");
    let shown = dir.to_string_lossy().into_owned();
    let mut server = spawn(
        &["--fixture", &shown],
        &[("LODESTAR_FIXTURE_PASSWORD", PASSWORD)],
    );
    server.ready();
    let m = manifest(&dir);
    assert_eq!(u(&m["format"]), u64::from(data::FORMAT));
    let port = server.port;
    let cookie = server.login();
    check_queries(port, &cookie, &m, false);

    // `hidden=true` adds exactly the filter-hidden torrents.
    let counts = &m["counts"];
    let browse = json(port, "/api/v1/search?q=&limit=1", &cookie);
    let with_hidden = json(port, "/api/v1/search?q=&hidden=true&limit=1", &cookie);
    assert_eq!(u(&browse["total"]), u(&counts["visible"]));
    assert_eq!(
        u(&with_hidden["total"]) - u(&browse["total"]),
        u(&counts["filter_hidden"])
    );

    check_details(port, &cookie, &m, false);

    check_discover(port, &cookie, &m, false);

    // Nothing reaches out: no DHT source, nothing fetched or looked up.
    let metrics = request(port, "GET /metrics HTTP/1.0", "").unwrap();
    assert!(!metrics.contains("source=\"dht\""), "{metrics}");
    for line in [
        "lodestar_fetch_attempts_total{transport=\"tcp\"} 0",
        "lodestar_fetch_attempts_total{transport=\"utp\"} 0",
        "lodestar_fetch_lookups 0",
        "lodestar_fetch_pending 0",
    ] {
        assert!(metrics.contains(line), "{line}: {metrics}");
    }
    let status = json(port, "/api/status", &cookie);
    assert!(status["dht"].is_null(), "{status}");
    assert_eq!(status["fetch"]["fetched"], 0);
    let log = server.stop();
    assert!(log.contains("fixture built"), "{log}");
    assert!(
        log.contains("claimed with LODESTAR_FIXTURE_PASSWORD"),
        "{log}"
    );
    assert!(!log.contains("setup token"), "claimed, so no token: {log}");
    assert!(!dir.join("dht-state.bin").exists(), "the DHT never ran");

    // The same folder again, with Adult shown: reused as it is.
    let before = std::fs::read_to_string(dir.join(data::MANIFEST)).unwrap();
    let mut server = spawn(&["--fixture", &shown], &[("LODESTAR_SHOW_ADULT", "true")]);
    server.ready();
    let cookie = server.login();
    check_queries(server.port, &cookie, &m, true);
    check_details(server.port, &cookie, &m, true);
    check_discover(server.port, &cookie, &m, true);
    let adult = json(
        server.port,
        "/api/v1/search?q=type%3Aadult&limit=1",
        &cookie,
    );
    assert_eq!(adult["total"], counts["adult"]);
    let log = server.stop();
    assert!(log.contains("serving the fixture"), "{log}");
    assert!(!log.contains("fixture built"), "{log}");
    assert_eq!(
        std::fs::read_to_string(dir.join(data::MANIFEST)).unwrap(),
        before
    );
    check_store(&dir, &m);
}

/// Discover's Newest, every tab, with Adult `adult`.
fn check_discover(port: u16, cookie: &str, m: &Value, adult: bool) {
    let mut tabs = 0;
    for d in m["discover"].as_array().unwrap() {
        if d["adult"].as_bool() != Some(adult) {
            continue;
        }
        tabs += 1;
        let path = match d["type"].as_str() {
            Some(t) => format!("/api/v1/discover?type={t}&limit=5"),
            None => "/api/v1/discover?limit=5".to_owned(),
        };
        let got = json(port, &path, cookie);
        assert_eq!(got["newest"]["total"], d["total"], "{path} adult={adult}");
        let hashes: Vec<&Value> = got["newest"]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| &h["hash"])
            .collect();
        let want: Vec<&Value> = d["newest"].as_array().unwrap().iter().collect();
        assert_eq!(hashes, want, "{path} adult={adult}");
    }
    assert!(tabs > 1);
}

/// Detail: every hand-specified torrent as specified (Adult ones only
/// while shown); never-scraped swarm fields are null. Scraped ones show
/// their values once the API reads the swarm (M4, #48), null until then.
fn check_details(port: u16, cookie: &str, m: &Value, adult: bool) {
    let (mut scraped, mut scraped_shown) = (0, 0);
    for t in m["torrents"].as_array().unwrap() {
        let key = t["key"].as_str().unwrap();
        let path = format!("/api/v1/torrents/{}", t["hash"].as_str().unwrap());
        if t["content_type"] == "adult" && !adult {
            let r = request(
                port,
                &format!("GET {path} HTTP/1.0\r\nCookie: {cookie}"),
                "",
            )
            .unwrap();
            assert!(
                r.starts_with("HTTP/1.0 404") && r.contains("adult_hidden"),
                "{key}: {r}"
            );
            continue;
        }
        let d = json(port, &path, cookie);
        for f in ["id", "hash", "name", "size", "files", "discovered_at"] {
            assert_eq!(d[f], t[f], "{key}: {f}");
        }
        match t["swarm"].as_object() {
            None => {
                for f in SWARM.iter().chain(&["dht_seeds", "dht_peers"]) {
                    assert!(d[*f].is_null(), "{key}: never scraped, {f} = {}", d[*f]);
                }
            }
            Some(s) => {
                scraped += 1;
                if !d["seeders"].is_null() {
                    for f in SWARM {
                        assert_eq!(d[f], s[f], "{key}: {f}");
                    }
                    scraped_shown += 1;
                }
            }
        }
    }
    assert!(scraped > 0);
    assert!(
        scraped_shown == 0 || scraped_shown == scraped,
        "all or none (before #48)"
    );
}

/// What only the store shows yet: swarm rows as built, and the saved
/// searches' ASTs parse back to their queries.
fn check_store(dir: &Path, m: &Value) {
    use lodestar_store::{Store, StoreConfig};
    let store = Store::open_reader(dir.join(data::DB), StoreConfig::default()).unwrap();
    // Serving changed no swarm value: nothing was scraped.
    let answered: i64 = store
        .connection()
        .query_row(
            "SELECT count(*) FROM swarm WHERE trackers_checked > 0 OR dht_seeds IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(answered as u64, u(&m["swarm"]["populated"]));
    for t in m["torrents"].as_array().unwrap() {
        if let Some(s) = t["swarm"].as_object() {
            let row = store.swarm(t["id"].as_i64().unwrap()).unwrap().unwrap();
            assert_eq!(i64::from(row.seeders), s["seeders"].as_i64().unwrap());
            assert_eq!(row.last_scrape_at, s["last_scrape_at"].as_i64().unwrap());
        }
    }
    let saved = store.saved_searches().unwrap();
    assert_eq!(saved.len(), m["saved_searches"].as_array().unwrap().len());
    for (s, want) in saved.iter().zip(m["saved_searches"].as_array().unwrap()) {
        assert_eq!(s.search.name, want["name"].as_str().unwrap());
        let ast: lodestar_search::Node = serde_json::from_str(&s.search.ast).unwrap();
        assert_eq!(
            ast,
            lodestar_search::parse(&s.search.query).unwrap(),
            "{}",
            s.search.name
        );
        assert_eq!(s.alerts, u(&want["alerts"]));
        assert_eq!(s.unread, u(&want["unread"]));
    }
}

#[test]
fn a_foreign_folder_is_refused_untouched() {
    for (file, content) in [("notes.txt", "mine"), ("lodestar.db", "a real install")] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(file), content).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_lodestar"))
            .args(["serve", "--fixture"])
            .arg(dir.path())
            .env("LODESTAR_HTTP_BIND", format!("127.0.0.1:{}", free_port()))
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(3), "{out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("refusing"), "{stderr}");
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, [file], "nothing written");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(file)).unwrap(),
            content
        );
    }
}

#[test]
fn a_temporary_fixture_is_removed_on_a_clean_exit() {
    let tmp = tempfile::tempdir().unwrap();
    let shown = tmp.path().to_string_lossy().into_owned();
    // Built in $TMPDIR; handed to PUID/PGID if started as root.
    std::fs::set_permissions(
        tmp.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o1777),
    )
    .unwrap();
    let mut server = spawn(&["--fixture"], &[("TMPDIR", &shown)]);
    server.ready();
    let made: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().collect();
    assert_eq!(made.len(), 1);
    let log = server.stop();
    assert!(
        log.contains("setup token"),
        "unclaimed: the token is printed: {log}"
    );
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0, "{log}");
}

#[test]
fn funnel_codes_match_the_sources_crate() {
    use data::stage;
    use lodestar_sources::{Loss, Stage};
    assert_eq!(Stage::Harvested as u8, stage::HARVESTED);
    assert_eq!(Stage::PeerFound as u8, stage::PEER_FOUND);
    assert_eq!(Stage::Fetched as u8, stage::FETCHED);
    assert_eq!(Stage::Stored as u8, stage::STORED);
    assert_eq!(Stage::Searchable as u8, stage::SEARCHABLE);
    assert_eq!(Stage::Lost as u8, stage::LOST);
    let losses: BTreeMap<u8, &str> = data::LOSSES.into_iter().collect();
    for (loss, name) in [
        (Loss::NoPeers, "no_peers"),
        (Loss::TimedOut, "timed_out"),
        (Loss::Refused, "refused"),
        (Loss::EncryptionFailed, "encryption_failed"),
        (Loss::Oversize, "oversize"),
        (Loss::Invalid, "invalid"),
        (Loss::Blocked, "blocked"),
        (Loss::Evicted, "evicted"),
        (Loss::GaveUp, "gave_up"),
        (Loss::Unreachable, "unreachable"),
    ] {
        assert_eq!(losses[&(loss as u8)], name);
    }
    assert_eq!(losses.len(), 10);
}
