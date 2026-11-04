//! Transparent huge pages on the real binary (X-thp-off): the supervisor
//! and its `--child` worker run with PR_SET_THP_DISABLE set (read from
//! outside as `/proc/<pid>/status`'s `THP_enabled`, which reports the same
//! flag PR_GET_THP_DISABLE does) and the worker gets `MIMALLOC_ALLOW_THP=0`;
//! `LODESTAR_THP=allow` leaves both alone. Linux only, as the switch is.
#![cfg(target_os = "linux")]

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(60);

/// A supervisor that's killed, with its worker, when the test ends.
struct Serve {
    child: Child,
    _dir: tempfile::TempDir,
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
        .stderr(Stdio::null());
    match thp {
        Some(v) => c.env("LODESTAR_THP", v),
        None => c.env_remove("LODESTAR_THP"),
    };
    Serve {
        child: c.spawn().unwrap(),
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
    let s = serve(None);
    let sup = s.child.id();
    let kid = worker(sup).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(thp_enabled(&sup.to_string()), Some(false), "the supervisor");
    assert_eq!(thp_enabled(&kid.to_string()), Some(false), "the worker");
    let env = environ(&kid.to_string()).unwrap();
    assert!(
        env.iter().any(|v| v == "MIMALLOC_ALLOW_THP=0"),
        "the worker's environment: {env:?}"
    );
}

#[test]
fn an_unknown_value_still_disables_thp() {
    let s = serve(Some("sometimes"));
    let kid = worker(s.child.id()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(thp_enabled(&kid.to_string()), Some(false));
}

#[test]
fn allow_leaves_thp_as_it_was() {
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
