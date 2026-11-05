//! The setup hold (LEGAL-2), through the real binary: on a data directory
//! nobody has set up, the DHT never starts (it binds no socket, so it
//! announces nothing) until `POST /setup` succeeds, across a restart;
//! then it starts on its own, and an instance that's set up starts
//! crawling at once.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod common;

/// The longest any condition here is waited for.
const DEADLINE: Duration = Duration::from_secs(30);
/// The DHT's `pipeline.dht_started` line: logged once it has bound.
const DHT_STARTED: &str = "pipeline.dht_started";

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

/// Waits for `ok` to give a value, up to [`DEADLINE`].
fn until<T>(what: &str, mut ok: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = ok() {
            return v;
        }
        assert!(start.elapsed() < DEADLINE, "timed out waiting: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The session cookie a response sets.
fn cookie(response: &str) -> String {
    response
        .lines()
        .find_map(|l| l.strip_prefix("set-cookie: "))
        .and_then(|c| c.split(';').next())
        .expect("session cookie")
        .to_string()
}

struct Server {
    child: Child,
    log: Arc<Mutex<String>>,
    reader: std::thread::JoinHandle<()>,
    port: u16,
}

impl Server {
    /// Starts `serve` on `dir` and waits until its health check answers.
    /// Its DHT asks no public router and it scrapes a local tracker
    /// nothing listens on: nothing leaves the machine either way.
    fn start(dir: &Path) -> Server {
        std::fs::write(
            dir.join("config.toml"),
            "[dht]\nbootstrap = []\n[scrape]\ntrackers = [\"udp://127.0.0.1:9\"]\n",
        )
        .unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_lodestar"))
            .env("LODESTAR_DATA_DIR", dir)
            .env("LODESTAR_HTTP_BIND", common::ANY_PORT)
            .env("LODESTAR_DHT_PORT", "0")
            .env("LODESTAR_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let log = Arc::new(Mutex::new(String::new()));
        let reader = {
            let (log, stderr) = (log.clone(), child.stderr.take().unwrap());
            std::thread::spawn(move || {
                use std::io::BufRead;
                for line in std::io::BufReader::new(stderr)
                    .lines()
                    .map_while(Result::ok)
                {
                    let mut all = log.lock().unwrap();
                    all.push_str(&line);
                    all.push('\n');
                }
            })
        };
        let port = until("the port it serves on", || {
            common::serving_port(&log.lock().unwrap())
        });
        until("the health check", || {
            request(port, "GET /healthz HTTP/1.0", "").filter(|r| r.starts_with("HTTP/1.0 200"))
        });
        Server {
            child,
            log,
            reader,
            port,
        }
    }

    fn log(&self) -> String {
        self.log.lock().unwrap().clone()
    }

    /// Stops it with SIGTERM; returns everything it logged.
    fn stop(mut self) -> String {
        let killed = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(killed.success());
        let exit = self.child.wait().unwrap();
        let log = self.log.clone();
        self.reader.join().unwrap();
        let log = log.lock().unwrap().clone();
        assert!(exit.success(), "exit {exit:?}: {log}");
        log
    }

    fn metrics(&self) -> String {
        request(self.port, "GET /metrics HTTP/1.0", "").unwrap()
    }

    /// The setup token from the log (§12).
    fn token(&self) -> String {
        until("the setup token", || {
            let log = self.log();
            let rest = log.split("setup token ").nth(1)?;
            rest.split_whitespace().next().map(str::to_string)
        })
    }

    /// Starts and stops it without setting it up; returns its log, after
    /// checking the DHT never started (a node that ran logs
    /// [`DHT_STARTED`] when it binds).
    fn run_unset(dir: &Path) -> String {
        let server = Server::start(dir);
        let metrics = server.metrics();
        assert!(
            !metrics.contains("lodestar_dht_packets_out_total"),
            "{metrics}"
        );
        let log = server.stop();
        assert!(!log.contains(DHT_STARTED), "{log}");
        log
    }

    /// `POST /login` with the password `setup` chose; returns the cookie.
    fn login(&self) -> String {
        let r = request(
            self.port,
            "POST /api/v1/login HTTP/1.0\r\nContent-Type: application/json",
            r#"{"password":"correct horse battery"}"#,
        )
        .unwrap();
        assert!(r.starts_with("HTTP/1.0 200"), "{r}");
        cookie(&r)
    }

    /// `POST /setup`; returns the session cookie.
    fn setup(&self) -> String {
        let token = self.token();
        let r = request(
            self.port,
            "POST /api/v1/setup HTTP/1.0\r\nContent-Type: application/json",
            &format!(
                r#"{{"token":"{token}","password":"correct horse battery","notice_version":{v}}}"#,
                v = lodestar_api::legal::NOTICE_VERSION
            ),
        )
        .unwrap();
        assert!(r.starts_with("HTTP/1.0 200"), "{r}");
        cookie(&r)
    }

    fn post(&self, path: &str, cookie: &str) -> String {
        request(
            self.port,
            &format!(
                "POST {path} HTTP/1.0\r\nCookie: {cookie}\r\nOrigin: http://localhost\r\n\
                 Content-Type: application/json"
            ),
            "{}",
        )
        .unwrap()
    }

    fn status(&self, cookie: &str) -> String {
        request(
            self.port,
            &format!("GET /api/status HTTP/1.0\r\nCookie: {cookie}"),
            "",
        )
        .unwrap()
    }
}

#[test]
fn the_crawler_waits_for_setup_across_a_restart_then_starts() {
    let dir = tempfile::tempdir().unwrap();

    // A fresh data directory, served and stopped twice without setup: the
    // DHT never ran (its state file is written only by a node that ran).
    Server::run_unset(dir.path());
    Server::run_unset(dir.path());
    assert!(!dir.path().join("dht-state.bin").exists());

    // Setup releases it: the DHT starts and the crawler runs.
    let server = Server::start(dir.path());
    let cookie = server.setup();
    until("the DHT to start after setup", || {
        server.log().contains(DHT_STARTED).then_some(())
    });
    until("the crawler to run", || {
        let status = server.status(&cookie);
        status
            .contains(r#""crawler":{"paused":false,"reason":null}"#)
            .then_some(())
    });
    server.stop();
    assert!(dir.path().join("dht-state.bin").exists());

    // Set up: the next start crawls at once, with no setup.
    let server = Server::start(dir.path());
    until("the DHT to start", || {
        server.log().contains(DHT_STARTED).then_some(())
    });
    let cookie = server.login();
    let status = server.status(&cookie);
    assert!(
        status.contains(r#""crawler":{"paused":false,"reason":null}"#),
        "{status}"
    );
    let log = server.stop();
    assert!(!log.contains("setup token "), "{log}");
}

#[test]
fn a_pause_the_user_chose_stays_after_setup() {
    let dir = tempfile::tempdir().unwrap();
    // Set up and paused by the user…
    let server = Server::start(dir.path());
    let cookie = server.setup();
    let paused = server.post("/api/v1/crawler/pause", &cookie);
    assert!(paused.starts_with("HTTP/1.0 200"), "{paused}");
    server.stop();
    // …then the password reset (`auth.json` removed): held for setup
    // again (a paused DHT still binds, so a node that ran would show)…
    std::fs::remove_file(dir.path().join("auth.json")).unwrap();
    Server::run_unset(dir.path());
    // …and after setup the user's pause still stands.
    let server = Server::start(dir.path());
    let cookie = server.setup();
    until("the DHT to start after setup", || {
        server.log().contains(DHT_STARTED).then_some(())
    });
    until("the hold to lift", || {
        server
            .status(&cookie)
            .contains(r#""crawler":{"paused":true,"reason":"user"}"#)
            .then_some(())
    });
    server.stop();
}
