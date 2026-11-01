//! A `lodestar serve --fixture` child process and plain HTTP/1.0 requests
//! to it, shared by `tests/fixture.rs` and `tests/analytics_validation.rs`.

use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

/// The password both tests claim the fixture with.
pub const PASSWORD: &str = "correct horse battery";

/// One request; `None` when the server can't be reached.
pub fn request(port: u16, head: &str, body: &str) -> Option<String> {
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
pub fn json(port: u16, path: &str, cookie: &str) -> Value {
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

/// A running `serve --fixture`, with its log collected. `port` is known
/// once [`ready`](Server::ready) has seen its `app.serving` line.
pub struct Server {
    child: Child,
    pub port: u16,
    ports: std::sync::mpsc::Receiver<u16>,
    log: std::thread::JoinHandle<String>,
}

pub fn spawn(args: &[&str], env: &[(&str, &str)]) -> Server {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lodestar"));
    cmd.arg("serve")
        .args(args)
        .env("LODESTAR_HTTP_BIND", super::common::ANY_PORT)
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
    let (port_tx, ports) = std::sync::mpsc::channel();
    let log = std::thread::spawn(move || {
        let mut all = String::new();
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            if let Some(port) = super::common::serving_port(&line) {
                let _ = port_tx.send(port);
            }
            all.push_str(&line);
            all.push('\n');
        }
        all
    });
    Server {
        child,
        port: 0,
        ports,
        log,
    }
}

impl Server {
    /// Waits for `/healthz` (the first start builds the fixture).
    pub fn ready(&mut self) {
        let start = Instant::now();
        loop {
            if self.port == 0 {
                self.port = self.ports.try_recv().unwrap_or(0);
            }
            if self.port != 0
                && let Some(r) = request(self.port, "GET /healthz HTTP/1.0", "")
            {
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

    pub fn login(&self) -> String {
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
    pub fn stop(mut self) -> String {
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
