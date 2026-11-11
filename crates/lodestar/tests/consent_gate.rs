//! The consent gate through the real binary (X-consent-gate-052,
//! `docs/plan/2026-11-06-v052-feature-manifest.md` §3.4): until an
//! owner-approved consent text ships (SRC-15b), `PATCH /sources/{id}`
//! refuses, with a clear error, to turn on a source whose class asks for
//! consent (the torrent cache, External), even when the request names the
//! class and version `GET /sources` gives. A source that needs no consent
//! (Sonarr/Radarr webhooks, None) can still be turned on. One an earlier
//! build turned on with an acknowledgement is switched off at the next
//! start, before it runs, and the log says so.
#![cfg(unix)]

use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lodestar_store::{Store, StoreConfig};
use serde_json::Value;

mod common;

const PASSWORD: &str = "correct horse battery";
/// Long enough for the slowest CI runner to start the binary.
const DEADLINE: Duration = Duration::from_secs(30);

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
    /// Starts the binary on `dir` with nothing leaving the machine: no
    /// DHT and no tracker scrapes. The first start sets the password up;
    /// a later one logs in with it.
    fn start(dir: &Path, first: bool) -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_lodestar"))
            .env("LODESTAR_DATA_DIR", dir)
            .env("LODESTAR_HTTP_BIND", common::ANY_PORT)
            .env("LODESTAR_DHT_ENABLED", "false")
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
            s.send(
                "POST",
                "/api/v1/setup",
                &format!(
                    r#"{{"token":"{token}","password":"{PASSWORD}","notice_version":{}}}"#,
                    lodestar_api::legal::NOTICE_VERSION
                ),
            )
        } else {
            s.send(
                "POST",
                "/api/v1/login",
                &format!(r#"{{"password":"{PASSWORD}"}}"#),
            )
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

    /// `plugin`'s entry in `GET /sources`.
    fn source(&self, plugin: &str) -> Value {
        let r = self.send("GET", "/api/v1/sources", "");
        assert_eq!(status(&r), 200, "{r}");
        let v: Value = serde_json::from_str(body(&r)).unwrap();
        v.as_array()
            .or_else(|| v["sources"].as_array())
            .unwrap()
            .iter()
            .find(|x| x["plugin"] == plugin)
            .unwrap_or_else(|| panic!("{plugin} is registered: {v}"))
            .clone()
    }

    fn patch(&self, id: u64, body: &str) -> String {
        self.send("PATCH", &format!("/api/v1/sources/{id}"), body)
    }

    fn stop(&mut self) -> String {
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

impl Drop for Server {
    /// A failed assertion doesn't leave the server running. After
    /// [`Server::stop`] the process is already reaped and this does nothing.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn body(r: &str) -> &str {
    r.split("\r\n\r\n").nth(1).unwrap_or("")
}

fn status(r: &str) -> u16 {
    r.split_whitespace().nth(1).unwrap().parse().unwrap()
}

/// Asserts `r` is the gate's refusal: a `400` that names `enabled` and
/// says why in words a user can act on.
fn assert_gated(r: &str) {
    assert_eq!(status(r), 400, "{r}");
    let v: Value = serde_json::from_str(body(r)).unwrap();
    let e = &v["error"];
    assert_eq!(e["code"], "invalid_setting", "{r}");
    assert_eq!(e["field"], "enabled", "{r}");
    let message = e["message"].as_str().unwrap();
    assert!(
        message.contains("asks for your consent")
            && message.contains("can't show you an approved consent text yet"),
        "{r}"
    );
}

#[test]
fn a_source_that_needs_consent_cant_be_turned_on_and_one_that_doesnt_can() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Server::start(dir.path(), true);

    let cache = s.source("torrent_cache");
    assert_eq!(cache["consent"], "external", "{cache}");
    assert_eq!(cache["consent_required"], true, "{cache}");
    assert_eq!(cache["enabled"], false, "off by default: {cache}");
    let cache_id = cache["id"].as_u64().unwrap();
    let version = cache["consent_version"].as_str().unwrap().to_owned();
    assert!(!version.is_empty(), "{cache}");

    // Refused however it's asked: bare, with the acknowledgement
    // `GET /sources` asks for, and with the password as well.
    for patch in [
        r#"{"enabled":true}"#.to_owned(),
        format!(r#"{{"enabled":true,"consent":"external","consent_version":"{version}"}}"#),
        format!(
            r#"{{"enabled":true,"consent":"external","consent_version":"{version}","password":"{PASSWORD}"}}"#
        ),
        format!(
            r#"{{"enabled":true,"priority":"max","consent":"external","consent_version":"{version}"}}"#
        ),
    ] {
        assert_gated(&s.patch(cache_id, &patch));
    }
    // Nothing was saved: still off, still asking, priority unchanged.
    let after = s.source("torrent_cache");
    assert_eq!(after["enabled"], false, "{after}");
    assert_eq!(after["consent_required"], true, "{after}");
    assert_eq!(after["priority"], cache["priority"], "{after}");
    // Turning it off, or a change that doesn't turn it on, still works.
    let r = s.patch(cache_id, r#"{"enabled":false}"#);
    assert_eq!(status(&r), 200, "{r}");

    // The webhooks need no consent: they stay API-enable-able.
    let arr = s.source("arr_webhooks");
    assert_eq!(arr["consent"], "none", "{arr}");
    let arr_id = arr["id"].as_u64().unwrap();
    let r = s.patch(arr_id, r#"{"enabled":true}"#);
    assert_eq!(status(&r), 200, "{r}");
    s.stop();
    drop(s);

    // A build before the gate (a nightly) recorded the acknowledgement
    // and turned the cache on.
    {
        let mut store =
            Store::open(dir.path().join("lodestar.db"), StoreConfig::default()).unwrap();
        let id = store
            .sources()
            .unwrap()
            .into_iter()
            .find(|r| r.plugin == "torrent_cache")
            .unwrap()
            .id;
        store
            .enable_source(id, Some(&format!("external|{version}|1")))
            .unwrap();
    }

    // At the next start the cache is switched off before it runs, and the
    // log says so; the webhooks run.
    let mut s = Server::start(dir.path(), false);
    wait_for("the webhooks source running", || {
        let v = s.source("arr_webhooks");
        (v["health"]["running"] == true).then_some(())
    });
    let cache = s.source("torrent_cache");
    assert_eq!(cache["enabled"], false, "{cache}");
    assert_ne!(cache["health"]["running"], true, "{cache}");
    // The stored record covers what's shown, so nothing is left to
    // acknowledge: it's off because no consent text is approved yet, not
    // because its consent lapsed. The log names that reason.
    assert_eq!(cache["needs"], Value::Null, "{cache}");
    {
        let log = s.log.lock().unwrap();
        assert!(
            log.lines().any(|l| l.contains("Source state changed")
                && l.contains("torrent_cache")
                && l.contains("disabled")
                && l.contains("consent_text_unapproved")),
            "{log}"
        );
        assert!(!log.contains("consent_lapsed"), "{log}");
    }
    // The record is kept for when an approved text ships.
    let kept = Store::open_reader(dir.path().join("lodestar.db"), StoreConfig::default())
        .unwrap()
        .source_consent("torrent_cache")
        .unwrap();
    assert_eq!(kept, Some(format!("external|{version}|1")));
    assert_gated(&s.patch(
        cache_id,
        &format!(r#"{{"enabled":true,"consent":"external","consent_version":"{version}"}}"#),
    ));
    let log = s.stop();
    assert!(!log.contains(PASSWORD), "the password is never logged");
}

/// Prowlarr / Jackett (X-prowlarr-settings): registered, off, and kept
/// off by the gate however it's asked, even with its address and API key
/// saved. Its settings are saved through the plugin's rules, apply at the
/// next start, and its key is never echoed or logged.
#[test]
fn prowlarr_is_registered_off_and_its_settings_save() {
    const KEY: &str = "0123456789abcdef0123456789abcdef";
    let dir = tempfile::tempdir().unwrap();
    let mut s = Server::start(dir.path(), true);

    let p = s.source("prowlarr");
    assert_eq!(p["consent"], "account", "{p}");
    assert_eq!(p["enabled"], false, "off by default: {p}");
    assert_eq!(p["needs"], "credentials", "{p}");
    assert!(
        p["editable"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "config"),
        "{p}"
    );
    let id = p["id"].as_u64().unwrap();

    // Hostile addresses are refused, naming the rule and never the value.
    for url in [
        "http://169.254.169.254/latest",
        "http://user:hunter2@prowlarr.lan:9696",
        "http://prowlarr.lan:9696/#hunter2",
        "file:///hunter2",
    ] {
        let r = s.patch(
            id,
            &format!(r#"{{"config":{{"url":"{url}","manager":"prowlarr"}}}}"#),
        );
        assert_eq!(status(&r), 400, "{url}: {r}");
        assert!(body(&r).contains("invalid_config"), "{r}");
        assert!(
            !body(&r).contains("hunter2") && !body(&r).contains("169.254"),
            "{r}"
        );
    }
    let r = s.patch(
        id,
        &format!(r#"{{"config":{{"url":"http://prowlarr.lan:9696","manager":"prowlarr","api_key":"{KEY}"}}}}"#),
    );
    assert_eq!(status(&r), 400, "a key isn't a setting: {r}");
    assert!(!r.contains(KEY), "{r}");

    let r = s.patch(
        id,
        r#"{"config":{"url":"http://prowlarr.lan:9696/","manager":"prowlarr","agents":3}}"#,
    );
    assert_eq!(status(&r), 200, "{r}");
    let r = s.send(
        "PATCH",
        &format!("/api/v1/sources/{id}/credentials"),
        &format!(r#"{{"credentials":{{"api_key":"{KEY}"}}}}"#),
    );
    assert_eq!(status(&r), 200, "{r}");
    assert!(!r.contains(KEY), "{r}");
    let p = s.source("prowlarr");
    assert_eq!(
        p["restart_required"], true,
        "saved settings apply at the next start: {p}"
    );
    assert_eq!(p["credentials"], serde_json::json!(["api_key"]), "{p}");
    assert!(!p.to_string().contains(KEY), "{p}");

    // The gate keeps it off, whatever is stored.
    assert_gated(&s.patch(id, r#"{"enabled":true}"#));
    assert_gated(&s.patch(
        id,
        &format!(r#"{{"enabled":true,"password":"{PASSWORD}"}}"#),
    ));
    let log = s.stop();
    assert!(!log.contains(KEY), "the key is never logged");
    drop(s);

    // At the next start it's built from what was saved, and still off.
    let mut s = Server::start(dir.path(), false);
    let p = s.source("prowlarr");
    assert_eq!(p["enabled"], false, "{p}");
    assert_ne!(p["health"]["running"], true, "{p}");
    assert_eq!(p["restart_required"], false, "running what was saved: {p}");
    assert!(!p.to_string().contains(KEY), "{p}");
    assert_gated(&s.patch(id, r#"{"enabled":true}"#));
    let log = s.stop();
    assert!(!log.contains(KEY), "the key is never logged");
    let db = std::fs::read(dir.path().join("lodestar.db")).unwrap();
    assert!(
        !db.windows(KEY.len()).any(|w| w == KEY.as_bytes()),
        "the database holds the key sealed only"
    );
}
