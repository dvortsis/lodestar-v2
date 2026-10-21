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

fn get(port: u16, path: &str) -> Option<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(s, "GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").ok()?;
    let mut out = String::new();
    s.read_to_string(&mut out).ok()?;
    Some(out)
}

#[test]
fn serves_status_and_stops_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
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

    let status = get(port, "/api/status").unwrap();
    assert!(status.contains("\"plugin\":\"dht\""), "{status}");
    assert!(status.contains("\"torrents\":0"), "{status}");
    let metrics = get(port, "/metrics").unwrap();
    assert!(
        metrics.contains("lodestar_source_up{source=\"dht\"} 1"),
        "{metrics}"
    );
    assert!(metrics.contains("# TYPE lodestar_fetch_lost_total counter"));
    let page = get(port, "/").unwrap();
    assert!(page.contains("Content-Security-Policy") || page.contains("content-security-policy"));

    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let exit = child.wait().unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(exit.success(), "exit {exit:?}: {stderr}");
    assert!(dir.path().join("dht-state.bin").exists());
    assert!(dir.path().join("lodestar.db").exists());

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
