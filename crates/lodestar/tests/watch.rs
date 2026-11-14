//! The real binary watches a folder (UP-1): a `.torrent` dropped into it
//! is taken as an upload is and moved to `added/`, a malformed one goes
//! to `failed/` (once it has read malformed and stayed unchanged for
//! 60 s), a copy that stalls for over 30 s and then finishes is taken,
//! and a folder inside the data directory is never read. The watch polls
//! every 10 s and reads a file on the second poll that sees it unchanged,
//! so each wait here allows a minute, and the wait for `failed/` allows
//! three.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod common;

const PASSWORD: &str = "correct horse battery";

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

fn torrent(name: &str) -> Vec<u8> {
    let mut info = b"d6:lengthi100e4:name".to_vec();
    info.extend(bstr(name.as_bytes()));
    info.extend(b"12:piece lengthi16384e6:pieces");
    info.extend(bstr(&[4; 20]));
    info.push(b'e');
    let mut file = b"d4:info".to_vec();
    file.extend(info);
    file.push(b'e');
    file
}

struct Server {
    child: Child,
    port: u16,
    cookie: String,
    log: Arc<Mutex<String>>,
    data: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start() -> Server {
    let data = tempfile::tempdir().unwrap();
    std::fs::write(
        data.path().join("config.toml"),
        "[scrape]\ntrackers = [\"udp://127.0.0.1:9\"]\n",
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lodestar"))
        .env("LODESTAR_DATA_DIR", data.path())
        .env("LODESTAR_HTTP_BIND", common::ANY_PORT)
        .env("LODESTAR_DHT_PORT", "0")
        .env("LODESTAR_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let log = Arc::new(Mutex::new(String::new()));
    let (token_tx, token_rx) = std::sync::mpsc::channel();
    let (port_tx, port_rx) = std::sync::mpsc::channel();
    let all = log.clone();
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
            let mut a = all.lock().unwrap();
            a.push_str(&line);
            a.push('\n');
        }
    });
    let port = port_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the port");
    let token = token_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("setup token");
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
        log,
        data,
    }
}

fn set_folder(s: &Server, folder: &Path) -> String {
    let body = serde_json::json!({ "folder": folder.to_string_lossy() }).to_string();
    exchange(
        s.port,
        &format!(
            "PATCH /api/v1/settings/watch HTTP/1.0\r\nCookie: {}\r\nContent-Type: application/json",
            s.cookie
        ),
        body.as_bytes(),
    )
    .unwrap()
}

/// Polls for a minute (standards §2: at least 10 s; the watch takes two
/// of its 10 s polls).
fn until(what: &str, f: impl FnMut() -> bool) {
    until_within(what, Duration::from_secs(60), f)
}

/// Polls until `f` holds, failing after `deadline` (at least 10 s).
fn until_within(what: &str, deadline: Duration, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|r| {
            r.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[test]
fn a_served_instance_takes_files_from_its_watch_folder() {
    let s = start();
    let folder = tempfile::tempdir().unwrap();
    // As a bind mount is set up: writable by the account Lodestar runs as
    // (it drops root's rights at start, privdrop).
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    }
    let r = set_folder(&s, folder.path());
    assert!(r.starts_with("HTTP/1.0 200"), "{r}");

    std::fs::write(
        folder.path().join("Zq9Good Cut.torrent"),
        torrent("Watched Cut 2022 1080p.mkv"),
    )
    .unwrap();
    let broken = Instant::now();
    std::fs::write(folder.path().join("Zq9Broken.torrent"), b"not bencode").unwrap();
    until("the good file taken", || {
        names(&folder.path().join("added")) == ["Zq9Good Cut.torrent"]
    });
    // X-watch-stall-retry: a file that stays malformed and unchanged is
    // failed, but only once it has been so for over 60 s: first sight,
    // a still poll that reads it, then 60 s of wall-clock time.
    until_within("the broken file failed", Duration::from_secs(180), || {
        names(&folder.path().join("failed")) == ["Zq9Broken.malformed.torrent"]
    });
    assert!(
        broken.elapsed() > Duration::from_secs(60),
        "failed after only {:?}",
        broken.elapsed()
    );
    // Stored and found, as an upload is.
    until("search finds it", || {
        exchange(
            s.port,
            &format!(
                "GET /api/v1/search?q=Watched%20Cut HTTP/1.0\r\nCookie: {}",
                s.cookie
            ),
            b"",
        )
        .is_some_and(|r| r.contains("\"total\":1"))
    });
    // The poll logs once its moves are done, and the log reaches us
    // through the reader thread.
    until("the log lines", || {
        let log = s.log.lock().unwrap();
        log.contains("watch.started") && log.contains("watch.took")
    });
    let log = s.log.lock().unwrap().clone();
    assert!(!log.contains("Zq9"), "no file name in the log: {log}");
}

/// X-watch-stall-retry: a copy that stalls looks complete to the watch,
/// which reads it as malformed. It's held, not failed, through a stall of
/// over 30 s, and once the copy finishes it's taken. A valid file that
/// sorts after it, written just after it, shows when the stalled one has
/// been read: both are read on the same poll, in name order.
#[test]
fn a_stalled_copy_that_finishes_is_taken_not_failed() {
    let s = start();
    let folder = tempfile::tempdir().unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    }
    let r = set_folder(&s, folder.path());
    assert!(r.starts_with("HTTP/1.0 200"), "{r}");

    let whole = torrent("Stalled Copy 2023 1080p.mkv");
    let stalled = folder.path().join("Zq9A Stalled.torrent");
    let written = Instant::now();
    // The first half arrives, then the copy stalls.
    std::fs::write(&stalled, &whole[..whole.len() / 2]).unwrap();
    std::fs::write(
        folder.path().join("Zq9B Marker.torrent"),
        torrent("Marker Cut 2023 1080p.mkv"),
    )
    .unwrap();
    until("the marker taken", || {
        names(&folder.path().join("added")) == ["Zq9B Marker.torrent"]
    });
    // Unchanged across two polls 10 s apart, read once as malformed, and
    // still where it was.
    assert!(written.elapsed() >= Duration::from_secs(10));
    assert!(stalled.exists(), "held, not moved");
    // The stall goes on to 50 s: well past 30 s, and past what a 10 s span
    // would allow (checked). This wait is the stall being tested, not a
    // wait for the watch: every check on the way says the file is still
    // held.
    until("a stall of 50 s", || {
        assert!(stalled.exists(), "held, not moved");
        assert!(names(&folder.path().join("failed")).is_empty());
        written.elapsed() > Duration::from_secs(50)
    });
    // The copy resumes and finishes.
    std::fs::write(&stalled, &whole).unwrap();
    until("the finished copy taken", || {
        names(&folder.path().join("added")) == ["Zq9A Stalled.torrent", "Zq9B Marker.torrent"]
    });
    assert!(names(&folder.path().join("failed")).is_empty());
    until("search finds it", || {
        exchange(
            s.port,
            &format!(
                "GET /api/v1/search?q=Stalled%20Copy HTTP/1.0\r\nCookie: {}",
                s.cookie
            ),
            b"",
        )
        .is_some_and(|r| r.contains("\"total\":1"))
    });
}

#[test]
fn a_folder_inside_the_data_directory_is_never_read() {
    let s = start();
    let inside = s.data.path().join("incoming");
    std::fs::create_dir(&inside).unwrap();
    std::fs::write(inside.join("x.torrent"), torrent("Inside 2022.mkv")).unwrap();
    // The save passes (it can't see the data directory); applying it
    // doesn't.
    let r = set_folder(&s, &inside);
    assert!(r.starts_with("HTTP/1.0 200"), "{r}");
    until("the reason", || {
        s.log.lock().unwrap().contains("reason=inside_data_dir")
    });
    assert_eq!(names(&inside), ["x.torrent"], "nothing read or moved");
    // A relative path is refused when it's saved.
    let r = set_folder(&s, Path::new("relative/watch"));
    assert!(
        r.starts_with("HTTP/1.0 422") || r.starts_with("HTTP/1.0 400"),
        "{r}"
    );
}
