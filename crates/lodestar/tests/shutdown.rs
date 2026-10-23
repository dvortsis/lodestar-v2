//! The bounded graceful shutdown (B-log1b) against the real binary, as
//! `tests/serve.rs` runs it: a client holding a log download open without
//! reading no longer keeps `SIGTERM` from saving state and exiting.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A rotated log file, named as the writer names them.
const ROTATED: &str = "lodestar-20261001T000000.000Z.jsonl";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Whether the server's stderr shows it gave up before serving.
fn child_exited(log: &Arc<Mutex<String>>) -> bool {
    let text = log.lock().unwrap();
    text.contains("PUID/PGID") || text.contains("refusing")
}

/// Waits for `ok`, polling, for at most `secs`; panics with `what`.
fn wait_for(secs: u64, what: &str, mut ok: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ok() {
        assert!(
            start.elapsed() < Duration::from_secs(secs),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

struct Server {
    dir: tempfile::TempDir,
    port: u16,
    child: Child,
    stderr: Arc<Mutex<String>>,
    cookie: String,
}

impl Server {
    /// A server set up and signed in, its logs folder seeded with a
    /// rotated file of `mib` MiB of valid lines.
    fn start(mib: usize) -> Server {
        let dir = tempfile::tempdir().unwrap();
        // Lodestar's own folder (run as root, the privilege drop hands
        // only such a folder to its account).
        std::fs::write(dir.path().join("config.toml"), "").unwrap();
        let logs = dir.path().join("logs");
        std::fs::create_dir(&logs).unwrap();
        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o700)).unwrap();
        if mib > 0 {
            seed(&logs.join(ROTATED), mib);
        }
        let port = free_port();
        let mut child = Command::new(env!("CARGO_BIN_EXE_lodestar"))
            .env_remove("LODESTAR_LOG")
            .env("LODESTAR_DATA_DIR", dir.path())
            .env("LODESTAR_HTTP_BIND", format!("127.0.0.1:{port}"))
            .env("LODESTAR_DHT_PORT", "0")
            .env("LODESTAR_DHT_ENABLED", "false")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = Arc::new(Mutex::new(String::new()));
        let mut pipe = child.stderr.take().unwrap();
        let sink = stderr.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        let mut token = String::new();
        let log = stderr.clone();
        wait_for(30, "the setup token", || {
            if child_exited(&log) {
                panic!("the server stopped: {}", log.lock().unwrap());
            }
            let text = stderr.lock().unwrap().clone();
            let Some(rest) = text.split("setup token ").nth(1) else {
                return false;
            };
            if !rest.contains('\n') {
                return false;
            }
            token = rest.split_whitespace().next().unwrap_or("").to_string();
            true
        });
        let mut s = Server {
            dir,
            port,
            child,
            stderr,
            cookie: String::new(),
        };
        wait_for(30, "/healthz", || {
            s.request("GET /healthz HTTP/1.0", "")
                .is_some_and(|r| r.starts_with("HTTP/1.0 200"))
        });
        let r = s
            .request(
                "POST /api/v1/setup HTTP/1.0\r\nContent-Type: application/json",
                &format!(r#"{{"token":"{token}","password":"shutdown-test-password"}}"#),
            )
            .unwrap();
        assert!(r.starts_with("HTTP/1.0 200"), "{r}");
        s.cookie = r
            .lines()
            .find_map(|l| l.strip_prefix("set-cookie: "))
            .and_then(|c| c.split(';').next())
            .unwrap()
            .to_string();
        s
    }

    fn request(&self, head: &str, body: &str) -> Option<String> {
        let mut c = TcpStream::connect(("127.0.0.1", self.port)).ok()?;
        c.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
        write!(
            c,
            "{head}\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .ok()?;
        let mut out = String::new();
        c.read_to_string(&mut out).ok()?;
        Some(out)
    }

    /// `SIGTERM`, then the exit status and how long it took.
    fn terminate(&mut self) -> (std::process::ExitStatus, Duration) {
        let start = Instant::now();
        let killed = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(killed.success());
        let mut status = None;
        wait_for(30, "the process to exit", || {
            status = self.child.try_wait().unwrap();
            status.is_some()
        });
        (status.unwrap(), start.elapsed())
    }

    fn file(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("logs").join("lodestar.jsonl"))
            .unwrap_or_default()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `mib` MiB of the file's own JSON lines, private as the writer makes
/// them.
fn seed(path: &Path, mib: usize) {
    let mut out = String::with_capacity(mib << 20);
    let mut n = 0u64;
    while out.len() < mib << 20 {
        out.push_str(&format!(
            r#"{{"ts":"2026-10-01T00:00:00.000Z","level":"info","component":"system","target":"lodestar::filler","msg":"filler line {n} for the shutdown test","fields":{{}}}}"#
        ));
        out.push('\n');
        n += 1;
    }
    std::fs::write(path, out).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// Where the first line of event `id` is in `file`.
fn at(file: &str, id: &str) -> Option<usize> {
    file.find(&format!(r#""event":"{id}""#))
}

#[test]
fn a_stalled_download_cannot_hold_the_shutdown() {
    // More than the socket buffers on both ends can hold, so the
    // download can't finish into the kernel's buffers.
    let mut s = Server::start(48);
    let mut stalled = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    write!(
        stalled,
        "GET /api/v1/logs/files/{ROTATED}?redact=false HTTP/1.0\r\nHost: localhost\r\nCookie: {}\r\n\r\n",
        s.cookie
    )
    .unwrap();
    // The response has started (its head and a first chunk); from here
    // on the client reads nothing.
    stalled
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let mut first = [0u8; 4096];
    let n = stalled.read(&mut first).unwrap();
    let head = String::from_utf8_lossy(&first[..n]);
    assert!(head.starts_with("HTTP/1.0 200"), "{head}");

    let (status, took) = s.terminate();
    let stderr = s.stderr.lock().unwrap().clone();
    assert!(status.success(), "exit {status:?} after {took:?}: {stderr}");
    let file = s.file();
    let forced = at(&file, "app.shutdown_forced").unwrap_or_else(|| panic!("{stderr}"));
    let stop = at(&file, "app.stop").unwrap_or_else(|| panic!("{stderr}"));
    assert!(
        forced < stop,
        "the forced shutdown comes before the stop line"
    );
    assert!(
        at(&file, "app.stopped_cleanly").is_some(),
        "the state was saved: {stderr}"
    );
    assert!(s.dir.path().join("lodestar.db").exists());
    drop(stalled);
}

#[test]
fn without_a_stalled_client_the_shutdown_doesnt_wait_the_bound() {
    let mut s = Server::start(0);
    let r = s
        .request(
            &format!("GET /api/v1/logs/files HTTP/1.0\r\nCookie: {}", s.cookie),
            "",
        )
        .unwrap();
    assert!(r.starts_with("HTTP/1.0 200"), "{r}");
    let (status, took) = s.terminate();
    let stderr = s.stderr.lock().unwrap().clone();
    assert!(status.success(), "exit {status:?} after {took:?}: {stderr}");
    let file = s.file();
    assert!(at(&file, "app.stop").is_some(), "{stderr}");
    assert!(
        at(&file, "app.shutdown_forced").is_none(),
        "nothing held it, so it didn't wait the bound: {file}"
    );
}
