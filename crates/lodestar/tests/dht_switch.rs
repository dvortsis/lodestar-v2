//! The real binary's `/metrics` reports each crawler switch's effective
//! value (X-dht-switch-identity): set by the environment, by the config
//! file, or left at its compiled default.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod common;

fn metrics(port: u16) -> Option<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(s, "GET /metrics HTTP/1.0\r\nHost: localhost\r\n\r\n").ok()?;
    let mut out = String::new();
    s.read_to_string(&mut out).ok()?;
    out.starts_with("HTTP/1.0 200").then_some(out)
}

#[test]
fn metrics_report_each_switch_as_the_process_resolved_it() {
    let dir = tempfile::tempdir().unwrap();
    let witness = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "[scrape]\ntrackers = [\"udp://127.0.0.1:9\"]\n\n[dht]\ntarget_prefixes = true\nutp = false\n",
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lodestar"))
        .env("LODESTAR_DATA_DIR", dir.path())
        .env("LODESTAR_HTTP_BIND", common::ANY_PORT)
        .env("LODESTAR_DHT_PORT", "0")
        .env("LODESTAR_DHT_WITNESS_DIR", witness.path())
        .env("LODESTAR_LOG", "info")
        // The environment turns `utp` on over the file's `false`, and the
        // on-by-default `safe_restart` off.
        .env("LODESTAR_DHT_UTP", "true")
        .env("LODESTAR_DHT_SAFE_RESTART", "false")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (port_tx, port_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            if let Some(port) = common::serving_port(&line) {
                let _ = port_tx.send(port);
            }
        }
    });
    let port = port_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the port it serves on");
    let start = Instant::now();
    let body = loop {
        if let Some(b) = metrics(port) {
            break b;
        }
        if start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            panic!("/metrics didn't answer within 30 s");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        body.contains("# TYPE lodestar_dht_switch gauge\n"),
        "{body}"
    );
    for line in [
        // The environment.
        "lodestar_dht_switch{switch=\"utp\"} 1",
        "lodestar_dht_switch{switch=\"safe_restart\"} 0",
        // The file.
        "lodestar_dht_switch{switch=\"target_prefixes\"} 1",
        // Compiled defaults.
        "lodestar_dht_switch{switch=\"coordinated_admission\"} 0",
        "lodestar_dht_switch{switch=\"route_backoff\"} 0",
    ] {
        assert!(
            body.lines().any(|l| l == line),
            "missing {line:?} in\n{body}"
        );
    }
    let series = body
        .lines()
        .filter(|l| l.starts_with("lodestar_dht_switch{"))
        .count();
    assert_eq!(series, 13, "{body}");
}
