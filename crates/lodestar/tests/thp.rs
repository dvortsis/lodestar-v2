//! Transparent huge pages on the real binary (X-thp-off): the supervisor
//! and its `--child` worker run with PR_SET_THP_DISABLE set (read from
//! outside as `/proc/<pid>/status`'s `THP_enabled`, which reports the same
//! flag PR_GET_THP_DISABLE does) and the worker gets `MIMALLOC_ALLOW_THP=0`;
//! `LODESTAR_THP=allow` leaves both alone. Linux only, as the switch is.
#![cfg(target_os = "linux")]

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(60);

/// A supervisor that's killed, with its worker, when the test ends.
struct Serve {
    child: Child,
    /// Everything the supervisor and its worker printed to stderr so far.
    stderr: Arc<Mutex<String>>,
    reader: Option<JoinHandle<()>>,
    _dir: tempfile::TempDir,
}

impl Serve {
    /// Stops both processes and returns all they printed to stderr, once
    /// the pipe has closed (both have exited) or the deadline passes.
    fn stop(mut self) -> String {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let start = Instant::now();
        let reader = self.reader.take().unwrap();
        while !reader.is_finished() && start.elapsed() < DEADLINE {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(reader.is_finished(), "stderr stayed open for {DEADLINE:?}");
        reader.join().unwrap();
        self.stderr.lock().unwrap().clone()
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        // The worker stops with its supervisor (PR_SET_PDEATHSIG).
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn serve(thp: Option<&str>) -> Serve {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_lodestar"));
    c.env("LODESTAR_DATA_DIR", dir.path())
        .env("LODESTAR_HTTP_BIND", "127.0.0.1:0")
        .env("LODESTAR_DHT_ENABLED", "false")
        .env("LODESTAR_SCRAPE_ENABLED", "false")
        .env("LODESTAR_LOG", "warn")
        .env("PUID", "0")
        .env("PGID", "0")
        .env("LODESTAR_ALLOW_ROOT", "1")
        .env_remove("MIMALLOC_ALLOW_THP")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    match thp {
        Some(v) => c.env("LODESTAR_THP", v),
        None => c.env_remove("LODESTAR_THP"),
    };
    let mut child = c.spawn().unwrap();
    // Drained as it comes, so a full pipe can never block either process.
    let mut pipe = child.stderr.take().unwrap();
    let stderr = Arc::new(Mutex::new(String::new()));
    let sink = stderr.clone();
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n @ 1..) = pipe.read(&mut buf) {
            sink.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });
    Serve {
        child,
        stderr,
        reader: Some(reader),
        _dir: dir,
    }
}

/// `THP_enabled` from `/proc/<pid>/status`: false when the THP-disable
/// flag is set.
fn thp_enabled(pid: &str) -> Option<bool> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find(|l| l.starts_with("THP_enabled:"))?;
    match line["THP_enabled:".len()..].trim() {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

/// Waits until `pid` runs more than one thread: `main` applies
/// `LODESTAR_THP` before it spawns any, so it has run by then.
fn past_startup(pid: u32) -> Result<(), String> {
    let start = Instant::now();
    loop {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
        let threads = status
            .lines()
            .find_map(|l| l.strip_prefix("Threads:"))
            .and_then(|v| v.trim().parse::<u32>().ok());
        if threads.is_some_and(|t| t > 1) {
            return Ok(());
        }
        if start.elapsed() > DEADLINE {
            return Err(format!(
                "{pid} still single-threaded after {DEADLINE:?}: {threads:?}"
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `THP_enabled` is in `/proc/<pid>/status` from Linux 5.0. Without it
/// there's nothing to read the flag from outside the process.
fn kernel_reports_thp() -> bool {
    let known = thp_enabled("self").is_some();
    if !known {
        eprintln!("skipped: this kernel has no THP_enabled line in /proc/<pid>/status");
    }
    known
}

/// The worker's pid once the supervisor has spawned it and it has exec'd
/// (its `environ` holds the supervisor's hint), and what was seen if not.
fn worker(supervisor: u32) -> Result<u32, String> {
    let start = Instant::now();
    loop {
        let kids =
            std::fs::read_to_string(format!("/proc/{supervisor}/task/{supervisor}/children"))
                .unwrap_or_default();
        for kid in kids.split_whitespace() {
            if environ(kid).is_some_and(|e| e.iter().any(|v| v.starts_with("LODESTAR_SUPERVISOR=")))
            {
                return Ok(kid.parse().unwrap());
            }
        }
        if start.elapsed() > DEADLINE {
            return Err(format!("no worker within {DEADLINE:?}; children: {kids:?}"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn environ(pid: &str) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    Some(
        raw.split(|&b| b == 0)
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .collect(),
    )
}

#[test]
fn the_worker_starts_with_thp_disabled() {
    if !kernel_reports_thp() {
        return;
    }
    let s = serve(None);
    let sup = s.child.id();
    let kid = worker(sup).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(thp_enabled(&sup.to_string()), Some(false), "the supervisor");
    // The worker reads 0 as soon as it exists, before its own `main` runs:
    // the flag came across fork and exec from the supervisor.
    assert_eq!(thp_enabled(&kid.to_string()), Some(false), "the worker");
    let env = environ(&kid.to_string()).unwrap();
    assert!(
        env.iter().any(|v| v == "MIMALLOC_ALLOW_THP=0"),
        "the worker's environment: {env:?}"
    );
}

#[test]
fn an_unknown_value_still_disables_thp_and_warns_once() {
    if !kernel_reports_thp() {
        return;
    }
    let s = serve(Some("sometimes"));
    let sup = s.child.id();
    let kid = worker(sup).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(thp_enabled(&sup.to_string()), Some(false), "the supervisor");
    assert_eq!(thp_enabled(&kid.to_string()), Some(false), "the worker");
    let env = environ(&kid.to_string()).unwrap();
    assert!(
        env.iter().any(|v| v == "MIMALLOC_ALLOW_THP=0"),
        "the worker's environment: {env:?}"
    );
    // Once the worker is past its own start, a second warning would be out.
    past_startup(kid).unwrap_or_else(|e| panic!("{e}"));
    let stderr = s.stop();
    let warning = "LODESTAR_THP: \"sometimes\" isn't `allow` or `off`";
    assert_eq!(stderr.matches(warning).count(), 1, "stderr: {stderr}");
}

#[test]
fn allow_leaves_thp_as_it_was() {
    if !kernel_reports_thp() {
        return;
    }
    // A test runner already started with the flag set passes it on to
    // everything; `allow` doesn't clear it, so there's nothing to compare.
    if thp_enabled("self") != Some(true) {
        eprintln!("skipped: this test process already has THP disabled");
        return;
    }
    let s = serve(Some("allow"));
    let sup = s.child.id();
    let kid = worker(sup).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(thp_enabled(&sup.to_string()), Some(true), "the supervisor");
    assert_eq!(thp_enabled(&kid.to_string()), Some(true), "the worker");
    let env = environ(&kid.to_string()).unwrap();
    assert!(
        !env.iter().any(|v| v.starts_with("MIMALLOC_ALLOW_THP=")),
        "the worker's environment: {env:?}"
    );
}
