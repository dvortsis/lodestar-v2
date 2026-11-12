//! The real binary takes a `.torrent` upload end to end (UP-0, plan §5):
//! a session and the page's own origin get a `202`, never a `503`, and
//! the file is stored and found by search; a `private=1` file is stored
//! but never served to Torznab clients.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

mod common;

const PASSWORD: &str = "correct horse battery";

/// One HTTP/1.0 exchange; the whole reply as text.
fn exchange(port: u16, head: &str, body: &[u8]) -> Option<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    let mut req = format!(
        "{head}\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    req.extend_from_slice(body);
    s.write_all(&req).ok()?;
    let mut out = Vec::new();
    s.read_to_end(&mut out).ok()?;
    Some(String::from_utf8_lossy(&out).into_owned())
}

fn bstr(s: &[u8]) -> Vec<u8> {
    let mut v = format!("{}:", s.len()).into_bytes();
    v.extend_from_slice(s);
    v
}

/// A one-file v1 `.torrent` with a tracker.
fn torrent(name: &str, private: bool) -> Vec<u8> {
    let mut info = b"d6:lengthi100e4:name".to_vec();
    info.extend(bstr(name.as_bytes()));
    info.extend(b"12:piece lengthi16384e6:pieces");
    info.extend(bstr(&[3; 20]));
    if private {
        info.extend(b"7:privatei1e");
    }
    info.push(b'e');
    let mut file = b"d8:announce".to_vec();
    file.extend(bstr(b"udp://tracker.example:6969/announce"));
    file.extend(b"4:info");
    file.extend(info);
    file.push(b'e');
    file
}

struct Server {
    child: Child,
    port: u16,
    cookie: String,
    _dir: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start() -> Server {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "[scrape]\ntrackers = [\"udp://127.0.0.1:9\"]\n",
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lodestar"))
        .env("LODESTAR_DATA_DIR", dir.path())
        .env("LODESTAR_HTTP_BIND", common::ANY_PORT)
        .env("LODESTAR_DHT_PORT", "0")
        .env("LODESTAR_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (token_tx, token_rx) = std::sync::mpsc::channel();
    let (port_tx, port_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            if let Some(rest) = line.split("setup token ").nth(1) {
                let _ = token_tx.send(rest.split_whitespace().next().unwrap_or("").to_string());
            }
            if let Some(port) = common::serving_port(&line) {
                let _ = port_tx.send(port);
            }
        }
    });
    let port = port_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the port it serves on");
    let token = token_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("setup token logged");
    let setup = exchange(
        port,
        "POST /api/v1/setup HTTP/1.0\r\nContent-Type: application/json",
        format!(
            r#"{{"token":"{token}","password":"{PASSWORD}","notice_version":{v}}}"#,
            v = lodestar_api::legal::NOTICE_VERSION
        )
        .as_bytes(),
    )
    .unwrap();
    assert!(setup.starts_with("HTTP/1.0 200"), "{setup}");
    let cookie = setup
        .lines()
        .find_map(|l| l.strip_prefix("set-cookie: "))
        .and_then(|c| c.split(';').next())
        .expect("session cookie")
        .to_string();
    Server {
        child,
        port,
        cookie,
        _dir: dir,
    }
}

fn upload(s: &Server, body: &[u8]) -> String {
    exchange(
        s.port,
        &format!(
            "POST /api/v1/torrents/upload HTTP/1.0\r\nCookie: {}\r\n\
             Content-Type: application/x-bittorrent\r\nOrigin: http://127.0.0.1:{}\r\n\
             Sec-Fetch-Site: same-origin",
            s.cookie, s.port
        ),
        body,
    )
    .unwrap()
}

fn get(s: &Server, path: &str) -> String {
    exchange(
        s.port,
        &format!("GET {path} HTTP/1.0\r\nCookie: {}", s.cookie),
        b"",
    )
    .unwrap()
}

/// Polls for 20 s (standards §2: at least 10 s).
fn until(what: &str, mut f: impl FnMut() -> Option<String>) -> String {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_served_instance_takes_an_upload_end_to_end() {
    let s = start();
    // A saved search with an RSS feed, matching both uploads.
    let r = exchange(
        s.port,
        &format!(
            "POST /api/v1/saved-searches HTTP/1.0\r\nCookie: {}\r\nContent-Type: application/json",
            s.cookie
        ),
        br#"{"name":"Cuts","q":"Cut","alert":true,"rss":true}"#,
    )
    .unwrap();
    assert!(r.starts_with("HTTP/1.0 201"), "{r}");
    let search: i64 = r
        .split("\"id\":")
        .nth(1)
        .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse().ok())
        .expect("the saved search's id");
    let r = upload(&s, &torrent("Public Cut 2021 1080p.mkv", false));
    assert!(r.starts_with("HTTP/1.0 202"), "{r}");
    assert!(r.contains("\"private\":false"), "{r}");
    let r = upload(&s, &torrent("Private Cut 2021 1080p.mkv", true));
    assert!(r.starts_with("HTTP/1.0 202"), "{r}");
    assert!(r.contains("\"private\":true"), "{r}");
    let private_hash = r
        .split("\"info_hash\":\"")
        .nth(1)
        .and_then(|r| r.split('"').next())
        .expect("the private one's hash")
        .to_string();
    // Without the page's origin, and without a session: refused.
    let r = exchange(
        s.port,
        &format!(
            "POST /api/v1/torrents/upload HTTP/1.0\r\nCookie: {}\r\n\
             Content-Type: application/x-bittorrent\r\nOrigin: https://evil.example",
            s.cookie
        ),
        &torrent("Other 2021.mkv", false),
    )
    .unwrap();
    assert!(r.starts_with("HTTP/1.0 403"), "{r}");
    let r = exchange(
        s.port,
        "POST /api/v1/torrents/upload HTTP/1.0\r\nContent-Type: application/x-bittorrent",
        &torrent("Other 2021.mkv", false),
    )
    .unwrap();
    assert!(r.starts_with("HTTP/1.0 401"), "{r}");

    // Stored and found, the private one too (it's this instance's own).
    for name in ["Public", "Private"] {
        until(name, || {
            let r = get(&s, &format!("/api/v1/search?q={name}%20Cut"));
            (r.starts_with("HTTP/1.0 200") && r.contains("\"total\":1")).then_some(r)
        });
    }

    // A Torznab client sees the public one, never the private one.
    let r = exchange(
        s.port,
        &format!(
            "POST /api/v1/keys HTTP/1.0\r\nCookie: {}\r\nContent-Type: application/json",
            s.cookie
        ),
        format!(
            r#"{{"name":"Sonarr","kind":"api_key","scopes":["torznab"],"password":"{PASSWORD}"}}"#
        )
        .as_bytes(),
    )
    .unwrap();
    assert!(r.starts_with("HTTP/1.0 201"), "{r}");
    let key = r
        .split("\"secret\":\"")
        .nth(1)
        .and_then(|r| r.split('"').next())
        .expect("the key")
        .to_string();
    let feed = until("torznab", || {
        let r = exchange(
            s.port,
            &format!("GET /api/v1/torznab/api?t=search&q=Cut&apikey={key} HTTP/1.0"),
            b"",
        )
        .unwrap();
        r.contains("Public Cut 2021").then_some(r)
    });
    assert!(!feed.contains("Private Cut"), "{feed}");

    // The saved search's RSS feed: the public one, never the private
    // one's name, hash or magnet (§18.7, BEP 27).
    let rss = until("the feed", || {
        let r = get(&s, &format!("/api/v1/feeds/{search}"));
        r.contains("Public Cut 2021").then_some(r)
    });
    assert!(!rss.contains("Private Cut"), "{rss}");
    assert!(!rss.contains(&private_hash), "{rss}");
    // The plain exports (CSV, NDJSON, magnets): the public upload, never
    // the private one.
    for format in ["csv", "json", "magnets"] {
        let r = get(&s, &format!("/api/v1/export?format={format}"));
        assert!(r.starts_with("HTTP/1.0 200"), "{format}: {r}");
        assert!(
            !r.contains("Private Cut") && !r.contains(&private_hash),
            "{format}: {r}"
        );
        assert!(r.contains("Public"), "{format}: {r}");
    }
    // And the instance's own alert list does show it: it's kept local.
    until("the private alert", || {
        let r = get(&s, &format!("/api/v1/saved-searches/{search}/alerts"));
        r.contains(&private_hash).then_some(r)
    });
}
