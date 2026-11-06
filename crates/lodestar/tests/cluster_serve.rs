//! The real binary's cluster port (WK-1, architecture §19.6, workers plan
//! Q3): with workers off, `serve` binds no cluster port and creates no
//! cluster CA or `cluster.` settings rows; turned on, it binds the
//! configured port and refuses a client that isn't an authenticated
//! worker.
#![cfg(unix)]

use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lodestar_store::{Store, StoreConfig};

mod common;
mod ports;

const DEADLINE: Duration = Duration::from_secs(30);

struct Server {
    child: Child,
    http: u16,
    log: JoinHandle<String>,
    listening: Receiver<String>,
}

/// Starts `serve` on `dir` with the DHT and scraping off (no network), and
/// waits for its HTTP port.
fn start(dir: &Path) -> Server {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lodestar"))
        .env("LODESTAR_DATA_DIR", dir)
        .env("LODESTAR_HTTP_BIND", common::ANY_PORT)
        .env("LODESTAR_DHT_ENABLED", "false")
        .env("LODESTAR_SCRAPE_ENABLED", "false")
        .env("LODESTAR_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (port_tx, port_rx) = channel();
    let (listening_tx, listening) = channel();
    let log = std::thread::spawn(move || {
        let mut all = String::new();
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            if let Some(port) = common::serving_port(&line) {
                let _ = port_tx.send(port);
            }
            if line.contains("cluster.listening") {
                let _ = listening_tx.send(line.clone());
            }
            all.push_str(&line);
            all.push('\n');
        }
        all
    });
    let http = match port_rx.recv_timeout(DEADLINE) {
        Ok(p) => p,
        Err(_) => {
            let _ = child.kill();
            panic!("no app.serving line: {}", log.join().unwrap());
        }
    };
    Server {
        child,
        http,
        log,
        listening,
    }
}

impl Server {
    fn metrics(&self) -> String {
        let start = Instant::now();
        loop {
            if let Some(body) = get(self.http, "/metrics") {
                return body;
            }
            assert!(start.elapsed() < DEADLINE, "/metrics never answered");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Polls `/metrics` until `series` reads at least `min`.
    fn wait_for(&self, series: &str, min: u64) -> u64 {
        let start = Instant::now();
        loop {
            let m = self.metrics();
            let v = value(&m, series);
            if v.is_some_and(|v| v >= min) {
                return v.unwrap();
            }
            assert!(start.elapsed() < DEADLINE, "{series} is {v:?}:\n{m}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

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

fn get(port: u16, path: &str) -> Option<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(s, "GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").ok()?;
    let mut out = String::new();
    s.read_to_string(&mut out).ok()?;
    out.starts_with("HTTP/1.0 200").then_some(out)
}

fn value(metrics: &str, series: &str) -> Option<u64> {
    metrics
        .lines()
        .find_map(|l| l.strip_prefix(series)?.strip_prefix(' ')?.parse().ok())
}

fn cluster_rows(dir: &Path) -> Vec<(String, String)> {
    let store = Store::open_reader(dir.join("lodestar.db"), StoreConfig::default()).unwrap();
    store.settings_with_prefix("cluster.").unwrap()
}

fn config(dir: &Path, enabled: bool, port: u16) {
    std::fs::write(
        dir.join("config.toml"),
        format!("[cluster]\nenabled = {enabled}\nlisten = \"127.0.0.1:{port}\"\n"),
    )
    .unwrap();
}

#[test]
fn the_cluster_port_is_off_until_workers_are_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let port = ports::test_port();

    // Off (the default): nothing listens on the configured port, and no
    // cluster PKI exists.
    config(dir.path(), false, port);
    let off = start(dir.path());
    let m = off.metrics();
    assert_eq!(value(&m, "lodestar_cluster_allowed"), Some(0), "{m}");
    assert_eq!(value(&m, "lodestar_cluster_listening"), Some(0), "{m}");
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "nothing binds the cluster port while workers are off"
    );
    let log = off.stop();
    assert!(!log.contains("cluster.listening"), "{log}");
    assert_eq!(cluster_rows(dir.path()), Vec::new(), "no cluster CA");

    // Allowed: the configured port is bound and the CA created.
    config(dir.path(), true, port);
    let on = start(dir.path());
    let line = on
        .listening
        .recv_timeout(DEADLINE)
        .expect("a cluster.listening line");
    assert!(line.contains(&format!("127.0.0.1:{port}")), "{line}");
    let m = on.metrics();
    assert_eq!(value(&m, "lodestar_cluster_allowed"), Some(1), "{m}");
    assert_eq!(value(&m, "lodestar_cluster_listening"), Some(1), "{m}");

    // A client that isn't an authenticated worker gets no session: here,
    // a plain HTTP upgrade request without TLS.
    let mut raw = TcpStream::connect(("127.0.0.1", port)).expect("the cluster port");
    raw.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    raw.write_all(b"GET /cluster HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\r\n")
        .unwrap();
    let mut reply = Vec::new();
    let _ = raw.read_to_end(&mut reply);
    assert!(
        !String::from_utf8_lossy(&reply).contains(" 101 "),
        "no upgrade without mutual TLS"
    );
    on.wait_for(
        "lodestar_cluster_preauth_refused_total{reason=\"handshake_failed\"}",
        1,
    );
    assert_eq!(
        value(&on.metrics(), "lodestar_cluster_sessions_total"),
        Some(0)
    );
    let log = on.stop();
    assert!(log.contains("cluster.closed"), "{log}");
    let rows = cluster_rows(dir.path());
    for key in [
        "cluster.ca.current",
        "cluster.server",
        "cluster.next_worker",
    ] {
        assert!(rows.iter().any(|(k, _)| k == key), "{key} in {rows:?}");
    }
    // Private keys are sealed, never stored in the clear: the CA row
    // holds a certificate and a sealed key, and the database alone opens
    // neither without `secrets.key`.
    assert!(dir.path().join("secrets.key").exists());
}
