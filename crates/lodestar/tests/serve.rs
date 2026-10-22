//! The real binary: serve, answer health and status, stop on SIGTERM with
//! its state saved.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn request(port: u16, head: &str, body: &str) -> Option<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
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

fn get(port: u16, path: &str) -> Option<String> {
    request(port, &format!("GET {path} HTTP/1.0"), "")
}

fn get_with(port: u16, path: &str, cookie: &str) -> String {
    request(
        port,
        &format!("GET {path} HTTP/1.0\r\nCookie: {cookie}"),
        "",
    )
    .unwrap()
}

#[test]
fn serves_status_and_stops_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
    // Scrape a local tracker nothing listens on, never the public list.
    std::fs::write(
        dir.path().join("config.toml"),
        "[scrape]\ntrackers = [\"udp://127.0.0.1:9\"]\n",
    )
    .unwrap();
    let env = |c: &mut Command| {
        c.env("LODESTAR_DATA_DIR", dir.path())
            .env("LODESTAR_HTTP_BIND", &bind)
            .env("LODESTAR_DHT_PORT", "0")
            .env("LODESTAR_LOG", "warn");
    };
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lodestar"));
    env(&mut cmd);
    let mut child = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The first run prints its setup token to the log (§12).
    let stderr = child.stderr.take().unwrap();
    let (token_tx, token_rx) = std::sync::mpsc::channel();
    let log = std::thread::spawn(move || {
        use std::io::BufRead;
        let mut all = String::new();
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            if let Some(rest) = line.split("setup token ").nth(1) {
                let _ = token_tx.send(rest.split_whitespace().next().unwrap_or("").to_string());
            }
            all.push_str(&line);
            all.push('\n');
        }
        all
    });

    let start = Instant::now();
    let health = loop {
        if let Some(r) = get(port, "/healthz") {
            break r;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "server didn't come up"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(health.starts_with("HTTP/1.0 200"), "{health}");

    let mut check = Command::new(env!("CARGO_BIN_EXE_lodestar"));
    env(&mut check);
    assert!(check.arg("healthcheck").status().unwrap().success());

    // Status and the API need a session; metrics and health don't.
    assert!(
        get(port, "/api/status")
            .unwrap()
            .starts_with("HTTP/1.0 401")
    );
    let token = token_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("setup token logged");
    let setup = request(
        port,
        "POST /api/v1/setup HTTP/1.0\r\nContent-Type: application/json",
        &format!(r#"{{"token":"{token}","password":"correct horse battery"}}"#),
    )
    .unwrap();
    assert!(setup.starts_with("HTTP/1.0 200"), "{setup}");
    let cookie = setup
        .lines()
        .find_map(|l| l.strip_prefix("set-cookie: "))
        .and_then(|c| c.split(';').next())
        .expect("session cookie")
        .to_string();
    let status = get_with(port, "/api/status", &cookie);
    let search = get_with(port, "/api/v1/search?q=anything", &cookie);
    assert!(
        search.starts_with("HTTP/1.0 200") && search.contains("\"total\":0"),
        "{search}"
    );
    assert!(status.contains("\"plugin\":\"dht\""), "{status}");
    assert!(status.contains("\"torrents\":0"), "{status}");
    let metrics = get(port, "/metrics").unwrap();
    assert!(
        metrics.contains("lodestar_source_up{source=\"dht\"} 1"),
        "{metrics}"
    );
    assert!(metrics.contains("# TYPE lodestar_fetch_lost_total counter"));
    assert!(
        metrics.contains("lodestar_leases_outstanding 0"),
        "{metrics}"
    );
    assert!(metrics.contains("lodestar_search_docs 0"), "{metrics}");
    assert!(
        metrics.contains("# TYPE lodestar_scrape_rounds_total counter"),
        "{metrics}"
    );
    assert!(
        metrics.contains("lodestar_tracker_health{tracker=\"1\"}"),
        "{metrics}"
    );
    assert!(
        !metrics.contains("127.0.0.1:9"),
        "no tracker URLs in metrics"
    );
    assert!(status.contains("\"search\":{"), "{status}");
    assert!(status.contains("\"swarm\":{"), "{status}");
    assert!(status.contains("udp://127.0.0.1:9"), "{status}");
    // `/` is the UI (or its stub) under the strict policy; the status page
    // moved to `/status` with its own.
    let page = get(port, "/").unwrap();
    assert!(page.starts_with("HTTP/1.0 200"), "{page}");
    assert!(
        page.contains("content-security-policy: default-src 'self'; script-src 'self'"),
        "{page}"
    );
    assert!(page.contains("<html"), "{page}");
    let spa = get(port, "/search?q=x").unwrap();
    assert!(
        spa.starts_with("HTTP/1.0 200") && spa.contains("<html"),
        "{spa}"
    );
    let status_page = get(port, "/status").unwrap();
    assert!(status_page.starts_with("HTTP/1.0 200"), "{status_page}");
    assert!(
        status_page.contains("script-src 'unsafe-inline'"),
        "{status_page}"
    );
    assert!(status_page.contains("fetch(\"/api/status\")"));
    // The UI never shadows the API: unknown paths keep the envelope.
    for path in ["/api/v1/nope", "/api/nope"] {
        let r = get(port, path).unwrap();
        assert!(
            r.starts_with("HTTP/1.0 404") && r.contains("\"code\":\"no_route\""),
            "{path}: {r}"
        );
    }

    // The live stream goes out through the whole router as written: never
    // compressed (a compression layer would hold frames back), and the
    // first crawler snapshot within one 5 s period. It stays open across
    // the shutdown below, which must still finish.
    let mut events = TcpStream::connect(("127.0.0.1", port)).unwrap();
    events
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    write!(
        events,
        "GET /api/v1/events?topics=crawler HTTP/1.1\r\nHost: localhost\r\n\
         Cookie: {cookie}\r\nAccept-Encoding: gzip, br\r\n\r\n"
    )
    .unwrap();
    let mut stream = String::new();
    let start = Instant::now();
    let mut buf = [0u8; 4096];
    while !stream.contains("event: crawler\n") {
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "no crawler event: {stream}"
        );
        match events.read(&mut buf) {
            Ok(0) => panic!("the stream closed: {stream}"),
            Ok(n) => stream.push_str(&String::from_utf8_lossy(&buf[..n])),
            Err(_) => {}
        }
    }
    assert!(stream.contains("retry: 5000\n\n"), "{stream}");
    let head = stream.to_ascii_lowercase();
    assert!(head.starts_with("http/1.1 200"), "{head}");
    assert!(head.contains("content-type: text/event-stream"), "{head}");
    assert!(!head.contains("content-encoding"), "{head}");

    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let exit = child.wait().unwrap();
    let stderr = log.join().unwrap();
    assert!(exit.success(), "exit {exit:?}: {stderr}");
    // The shutdown ended the open stream.
    events
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    while let Ok(n) = events.read(&mut buf) {
        if n == 0 {
            break;
        }
    }
    assert!(dir.path().join("dht-state.bin").exists());
    assert!(dir.path().join("lodestar.db").exists());
    assert!(dir.path().join("index").is_dir(), "search index created");
    assert!(
        dir.path().join("hot").join("seeders.col").exists(),
        "hot columns saved at shutdown"
    );

    // Nothing listening any more: the health check fails.
    let mut check = Command::new(env!("CARGO_BIN_EXE_lodestar"));
    env(&mut check);
    assert!(
        !check
            .arg("healthcheck")
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
}
