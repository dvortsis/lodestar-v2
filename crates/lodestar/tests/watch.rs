//! The real binary watches a folder (UP-1): a `.torrent` dropped into it
//! is taken as an upload is and moved to `added/`, a malformed one goes
//! to `failed/`, and a folder inside the data directory is never read.
//! The watch polls every 10 s and reads a file on the second poll that
//! sees it unchanged, so each wait here allows a minute.
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
fn until(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "timed out waiting for {what}"
        );
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
    std::fs::write(folder.path().join("Zq9Broken.torrent"), b"not bencode").unwrap();
    until("both files taken", || {
        names(&folder.path().join("added")) == ["Zq9Good Cut.torrent"]
            && names(&folder.path().join("failed")) == ["Zq9Broken.malformed.torrent"]
    });
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
    let log = s.log.lock().unwrap().clone();
    assert!(log.contains("watch.started"), "{log}");
    assert!(log.contains("watch.took"), "{log}");
    assert!(!log.contains("Zq9"), "no file name in the log: {log}");
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
