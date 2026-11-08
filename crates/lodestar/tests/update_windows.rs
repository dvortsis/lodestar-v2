//! `/updates` from the real binary on Windows (PK-win-upd-1): the check
//! and notify engine answers (no `unavailable: "windows_helper"`), nothing
//! is installable, Install and Roll back refuse with `not_built_yet`, and
//! the running server holds `<data>\bin\.lock`, so a second writer can't
//! take it.
#![cfg(windows)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod common;

/// Polls `check` every 250 ms until it returns `Some` or `limit` passes.
fn wait_for<T>(limit: Duration, mut check: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    loop {
        if let Some(v) = check() {
            return Some(v);
        }
        if start.elapsed() > limit {
            return None;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn request(port: u16, head: &str, body: &str) -> Option<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
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

fn signed_in(port: u16, method: &str, path: &str, cookie: &str, body: &str) -> String {
    request(
        port,
        &format!("{method} {path} HTTP/1.0\r\nCookie: {cookie}\r\nContent-Type: application/json"),
        body,
    )
    .unwrap_or_default()
}

fn json_body(response: &str) -> serde_json::Value {
    let (_, body) = response.split_once("\r\n\r\n").expect("a body");
    serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {response}"))
}

/// A config in `dir`: data beside it, HTTP on any port, no DHT, and
/// updates off, so nothing is fetched on its own.
fn write_config(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("config.toml");
    std::fs::write(
        &path,
        format!(
            "data_dir = '{}'\nlog = \"info\"\n[http]\nbind = \"{}\"\n\
             [dht]\nenabled = false\n[updates]\nmode = \"off\"\n",
            dir.join("data").display(),
            common::ANY_PORT
        ),
    )
    .unwrap();
    path
}

struct Kill(std::process::Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn updates_check_and_notify_without_installing() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(dir.path());
    let mut child = Command::new(env!("CARGO_BIN_EXE_lodestar"))
        .arg("--config")
        .arg(&config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let _child = Kill(child);
    let console = Arc::new(Mutex::new(String::new()));
    std::thread::spawn({
        let console = console.clone();
        move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
            {
                let mut c = console.lock().unwrap();
                c.push_str(&line);
                c.push('\n');
            }
        }
    });
    let jsonl = dir.path().join("data").join("logs").join("lodestar.jsonl");
    let port = wait_for(Duration::from_secs(60), || {
        common::serving_port(&std::fs::read_to_string(&jsonl).unwrap_or_default())
    })
    .expect("the server didn't come up");
    let log = wait_for(Duration::from_secs(30), || {
        let c = console.lock().unwrap().clone();
        c.contains("setup token ").then_some(c)
    })
    .expect("a setup token on the console");
    let token = log
        .lines()
        .rev()
        .find_map(|l| l.split("setup token ").nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap();
    let setup = request(
        port,
        "POST /api/v1/setup HTTP/1.0\r\nContent-Type: application/json",
        &format!(
            r#"{{"token":"{token}","password":"correct horse battery","notice_version":{v}}}"#,
            v = lodestar_api::legal::NOTICE_VERSION
        ),
    )
    .unwrap_or_default();
    assert!(setup.starts_with("HTTP/1.0 200"), "{setup}");
    let cookie = setup
        .lines()
        .find_map(|l| l.strip_prefix("set-cookie: "))
        .and_then(|c| c.split(';').next())
        .expect("a session cookie")
        .to_string();

    let status = signed_in(port, "GET", "/api/v1/updates", &cookie, "");
    assert!(status.starts_with("HTTP/1.0 200"), "{status}");
    let json = json_body(&status);
    assert_eq!(json["unavailable"], serde_json::Value::Null, "{json}");
    assert_eq!(json["capability"]["can_install"], false, "{json}");
    // A test build is a dev build: that's the reason it names first.
    let reason = json["capability"]["reason"].as_str().unwrap();
    assert!(reason == "not_built_yet" || reason == "dev_build", "{json}");
    assert_eq!(json["rollback"]["available"], false, "{json}");
    assert_eq!(json["rollback"]["reason"], "not_built_yet", "{json}");
    assert_eq!(json["policy"]["mode"], "off", "{json}");
    assert_eq!(json["status"]["next_check_at"], serde_json::Value::Null);

    let install = signed_in(
        port,
        "POST",
        "/api/v1/updates/install",
        &cookie,
        r#"{"version":"99.0.0","when":"now"}"#,
    );
    assert!(install.starts_with("HTTP/1.0 409"), "{install}");
    let body = json_body(&install);
    assert_eq!(body["error"]["code"], "install_unavailable", "{body}");
    assert_eq!(body["error"]["reason"], "not_built_yet", "{body}");
    let rollback = signed_in(
        port,
        "POST",
        "/api/v1/updates/rollback",
        &cookie,
        r#"{"confirm":true}"#,
    );
    assert!(rollback.starts_with("HTTP/1.0 409"), "{rollback}");
    assert_eq!(
        json_body(&rollback)["error"]["code"],
        "rollback_unavailable"
    );

    // The server is `<data>\bin`'s one writer: its lock is held.
    let lock = dir.path().join("data").join("bin").join(".lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock)
        .expect("the server made .lock");
    assert!(
        matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "a second writer took .lock"
    );
}
