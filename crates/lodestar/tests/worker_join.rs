//! The real binary as a remote worker (WK-2, architecture §19.6): `lodestar
//! worker --join … --token-file …` joins a real `serve` and connects; a
//! restart runs from the saved credentials; a reused token fails with one
//! message; a first join may take `LODESTAR_TOKEN`, which is ignored with a
//! warning once joined; and the token reaches no output or argv.
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use lodestar_cluster::Coordinator;
use lodestar_integrations::SecretStore;
use lodestar_store::{Store, StoreConfig};

mod common;
mod ports;

const DEADLINE: Duration = Duration::from_secs(30);

/// A child process whose stderr is collected, line by line.
struct Proc {
    child: Child,
    lines: Receiver<String>,
    /// Lines already received, in order.
    seen: Vec<String>,
    log: JoinHandle<String>,
}

fn spawn(mut cmd: Command) -> Proc {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, lines) = channel();
    let log = std::thread::spawn(move || {
        let mut all = String::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx.send(line.clone());
            all.push_str(&line);
            all.push('\n');
        }
        all
    });
    Proc {
        child,
        lines,
        seen: Vec::new(),
        log,
    }
}

impl Proc {
    /// Waits for a line containing `what`, or finds one already seen.
    fn wait_for(&mut self, what: &str) -> String {
        if let Some(line) = self.seen.iter().find(|l| l.contains(what)) {
            return line.clone();
        }
        let start = std::time::Instant::now();
        loop {
            let left = DEADLINE.saturating_sub(start.elapsed());
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    self.seen.push(line.clone());
                    if line.contains(what) {
                        return line;
                    }
                }
                Err(_) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    panic!("no line with {what:?}");
                }
            }
        }
    }

    fn stop(mut self) -> (std::process::ExitStatus, String) {
        let killed = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(killed.success());
        let exit = self.child.wait().unwrap();
        (exit, self.log.join().unwrap())
    }

    fn finish(mut self) -> (std::process::ExitStatus, String) {
        let exit = self.child.wait().unwrap();
        (exit, self.log.join().unwrap())
    }
}

fn serve(dir: &Path, port: u16) -> Proc {
    std::fs::write(
        dir.join("config.toml"),
        format!("[cluster]\nenabled = true\nlisten = \"127.0.0.1:{port}\"\n"),
    )
    .unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lodestar"));
    cmd.env("LODESTAR_DATA_DIR", dir)
        .env("LODESTAR_HTTP_BIND", common::ANY_PORT)
        .env("LODESTAR_DHT_ENABLED", "false")
        .env("LODESTAR_SCRAPE_ENABLED", "false")
        .env("LODESTAR_LOG", "info");
    let mut p = spawn(cmd);
    let serving = p.wait_for("app.serving");
    assert!(common::serving_port(&serving).is_some(), "{serving}");
    p.wait_for("cluster.listening");
    p
}

/// A join token, made the way the CLI or the API makes one: on the
/// coordinator's own store, which the running server reloads.
fn make_token(dir: &Path, name: &str) -> String {
    let mut store = Store::open(dir.join("lodestar.db"), StoreConfig::default()).unwrap();
    let secrets = SecretStore::open_or_create(dir).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mut coord = Coordinator::open(&mut store, &secrets, now).unwrap();
    let token = coord.create_join_token(&mut store, name, now).unwrap();
    token.expose().to_string()
}

fn worker(data: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lodestar"));
    cmd.arg("worker")
        .args(args)
        .arg("--data")
        .arg(data)
        .env("LODESTAR_DATA_DIR", data)
        .env("LODESTAR_LOG", "debug")
        .env_remove("LODESTAR_TOKEN");
    cmd
}

/// The token's text, or its secret part alone, anywhere in `text`.
fn leaks(text: &str, token: &str) -> bool {
    let text = text.to_lowercase();
    text.contains(token) || text.contains(&token[4..60])
}

#[test]
fn joins_restarts_and_never_shows_the_token() {
    let coord_dir = tempfile::tempdir().unwrap();
    let worker_dir = tempfile::tempdir().unwrap();
    let port = ports::test_port();
    let server = serve(coord_dir.path(), port);
    let token = make_token(coord_dir.path(), "w1");
    let token_file = coord_dir.path().join("join-token");
    std::fs::write(&token_file, format!("{token}\n")).unwrap();
    let url = format!("https://127.0.0.1:{port}");
    let join_args = [
        "--join",
        url.as_str(),
        "--token-file",
        token_file.to_str().unwrap(),
    ];

    // Join, and stay connected.
    let mut joined = spawn(worker(worker_dir.path(), &join_args));
    joined.wait_for("joined 127.0.0.1");
    joined.wait_for("connected as worker w1");
    #[cfg(target_os = "linux")]
    for part in ["cmdline", "environ"] {
        let bytes = std::fs::read(format!("/proc/{}/{part}", joined.child.id())).unwrap();
        assert!(
            !leaks(&String::from_utf8_lossy(&bytes), &token),
            "the token is in the worker's {part}"
        );
    }
    let (exit, log) = joined.stop();
    assert!(exit.success(), "{exit:?}: {log}");
    assert!(log.contains("lodestar worker: stopped"), "{log}");
    assert!(!leaks(&log, &token), "the token reached the output:\n{log}");
    assert!(worker_dir.path().join("cluster/credentials").exists());

    // The same token again: one message, and the saved worker is kept.
    let (exit, log) = spawn(worker(worker_dir.path(), &join_args)).finish();
    assert_eq!(exit.code(), Some(1), "{log}");
    let errors: Vec<&str> = log
        .lines()
        .filter(|l| l.starts_with("lodestar worker: "))
        .collect();
    assert_eq!(errors.len(), 1, "{log}");
    assert!(
        errors[0].contains("refused the join token") && errors[0].contains("worker w1"),
        "{log}"
    );
    assert!(!leaks(&log, &token), "{log}");

    // A restart runs from the saved credentials.
    let mut restarted = spawn(worker(worker_dir.path(), &[]));
    restarted.wait_for("connected as worker w1");
    let (exit, log) = restarted.stop();
    assert!(exit.success(), "{exit:?}: {log}");

    // A first join may take the token from the environment (plan Q2),
    // with the address in LODESTAR_JOIN as the Unraid template sets it.
    let second = make_token(coord_dir.path(), "w2");
    let fresh = tempfile::tempdir().unwrap();
    let with_env = || {
        let mut cmd = worker(fresh.path(), &[]);
        cmd.env("LODESTAR_TOKEN", &second)
            .env("LODESTAR_JOIN", &url);
        cmd
    };
    let mut first = spawn(with_env());
    first.wait_for("connected as worker w2");
    let (exit, log) = first.stop();
    assert!(exit.success(), "{exit:?}: {log}");
    assert!(log.contains("joined 127.0.0.1"), "{log}");
    assert!(!log.contains("LODESTAR_TOKEN is still set"), "{log}");
    assert!(!leaks(&log, &second), "{log}");

    // Once joined, the variable is ignored, with a warning to remove it.
    let mut again = spawn(with_env());
    again.wait_for("LODESTAR_TOKEN is still set");
    again.wait_for("connected as worker w2");
    let (exit, log) = again.stop();
    assert!(exit.success(), "{exit:?}: {log}");
    assert!(!log.contains("joined 127.0.0.1"), "no second join: {log}");
    assert!(!leaks(&log, &second), "{log}");

    let (exit, log) = server.stop();
    assert!(exit.success(), "{exit:?}: {log}");
    assert!(
        !leaks(&log, &token),
        "the token is in the coordinator's log"
    );
}
