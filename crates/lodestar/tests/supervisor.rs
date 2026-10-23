//! The supervisor on the real binary (§12.2), with the debug-only test
//! overrides (`LODESTAR_UPDATE_TEST`): a TEST ONLY trust root, a pretend
//! image version, short probation clocks and fault points. Overlays are
//! the test binary itself, stripped into `<data>/bin/<version>/` and
//! pinned by a manifest signed with a test key.
#![cfg(unix)]

use std::io::{BufRead, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use lodestar_store::{Store, StoreConfig};
use lodestar_update::testing::{TestTrust, bytes};
use serde_json::{Value, json};
use sha2::Digest as _;

const IMAGE: &str = "0.5.0";
const NEW: &str = "0.9.0";

struct Env {
    dir: tempfile::TempDir,
    port: u16,
    trust: TestTrust,
}

struct Running {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Env {
    fn new() -> Env {
        let env = Env {
            dir: tempfile::tempdir().unwrap(),
            port: std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port(),
            trust: TestTrust::default(),
        };
        let store = Store::open(env.db(), StoreConfig::default()).unwrap();
        set_marker(&store, "first");
        drop(store);
        env
    }

    fn data(&self) -> &Path {
        self.dir.path()
    }

    fn db(&self) -> PathBuf {
        self.data().join("lodestar.db")
    }

    fn bin(&self, name: &str) -> PathBuf {
        self.data().join("bin").join(name)
    }

    fn overrides(&self, extra: Value) -> String {
        let key = |k: &lodestar_update::testing::TestKey| k.public().to_base64();
        let mut o = json!({
            "trust_root": {
                "recovery": key(&self.trust.recovery),
                "release": key(&self.trust.release),
                "nightly": key(&self.trust.nightly),
                "freshness": key(&self.trust.freshness),
            },
            "target": "x86_64-unknown-linux-musl",
            "version": IMAGE,
            "probation_ms": 8000,
            "stall_ms": 4000,
            // Long enough for a graceful shutdown: a forwarded stop the
            // child outlasts by this much becomes SIGKILL.
            "grace_ms": 10_000,
        });
        if let (Some(o), Some(extra)) = (o.as_object_mut(), extra.as_object()) {
            o.extend(extra.clone());
        }
        o.to_string()
    }

    /// Stages `version` as an overlay running this binary (or `binary`, a
    /// small file for one that never runs), its manifest expiring at
    /// `expires`.
    fn stage(&self, version: &str, binary: Option<&[u8]>, expires: u64) {
        let dir = self.bin(version);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(self.data().join("bin"), PermissionsExt::from_mode(0o700))
            .unwrap();
        let target = dir.join("lodestar");
        match binary {
            Some(b) => std::fs::write(&target, b).unwrap(),
            // The debug binary is over the 256 MiB artifact cap; its
            // stripped copy (tens of MB) isn't.
            None => {
                let exe = env!("CARGO_BIN_EXE_lodestar");
                let ok = Command::new("strip")
                    .arg("-o")
                    .arg(&target)
                    .arg(exe)
                    .status()
                    .is_ok_and(|s| s.success());
                assert!(ok, "`strip` (binutils) is needed to stage a test overlay");
            }
        }
        std::fs::set_permissions(&target, PermissionsExt::from_mode(0o700)).unwrap();
        let (sha, size) = hash(&target);
        let manifest = json!({
            "type": "lodestar-manifest",
            "format": 1,
            "channel": "stable",
            "timestamp": expires - 3600,
            "expires": expires,
            "statements": [],
            "releases": [{
                "version": version, "date": expires - 7200, "level": "fix",
                "security": false, "manual_steps": false, "min_from": null,
                "withdrawn": false, "migrates": false, "reindex": false,
                "reindex_bytes": 0, "notes": "", "notes_url": null,
                "artifacts": [{
                    "target": "x86_64-unknown-linux-musl", "kind": "binary",
                    "url": "https://github.com/dvortsis/lodestar-v2/releases/download/x/lodestar",
                    "sha256": sha, "size": size,
                }],
            }],
        });
        let body = bytes(&manifest);
        std::fs::write(dir.join("manifest.json"), &body).unwrap();
        std::fs::write(
            dir.join("manifest.json.minisig"),
            self.trust.release.sign(&body),
        )
        .unwrap();
    }

    /// `version` active, the image before it; in probation with `pending`.
    fn activate(&self, version: &str, pending: bool) {
        std::fs::write(
            self.bin("active"),
            json!({"version": version, "previous": "image"}).to_string(),
        )
        .unwrap();
        if pending {
            std::fs::write(
                self.bin("pending"),
                json!({
                    "format": 1, "version": version, "previous": "image", "kind": "install",
                    "attempts": 0, "used_ms": 0, "index_before": [],
                })
                .to_string(),
            )
            .unwrap();
        }
    }

    /// As if `version` had migrated the store: a `pre-v<L+1>` snapshot
    /// holding marker `first`, `migration.json`, and the store at `second`.
    fn migrated_by(&self, version: &str) {
        let latest = lodestar_store::latest_schema_version();
        let store = Store::open(self.db(), StoreConfig::default()).unwrap();
        let snap = self.data().join(format!("lodestar.pre-v{}.db", latest + 1));
        store
            .connection()
            .execute("VACUUM INTO ?1", [snap.to_str().unwrap()])
            .unwrap();
        set_marker(&store, "second");
        drop(store);
        std::fs::write(
            self.bin(version).join("migration.json"),
            json!({"from": latest, "to": latest + 1, "started_at": 1}).to_string(),
        )
        .unwrap();
    }

    fn command(&self, overrides: &str, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_lodestar"));
        c.args(args)
            .env("LODESTAR_DATA_DIR", self.data())
            .env("LODESTAR_HTTP_BIND", format!("127.0.0.1:{}", self.port))
            .env("LODESTAR_DHT_ENABLED", "false")
            .env("LODESTAR_LOG", "warn")
            .env("LODESTAR_UPDATE_TEST", overrides)
            .env("PUID", "0")
            .env("PGID", "0")
            .env("LODESTAR_ALLOW_ROOT", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        c
    }

    fn start(&self, overrides: &str) -> Running {
        let mut child = self
            .command(overrides, &[])
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
            {
                let _ = tx.send(line);
            }
        });
        Running { child, lines }
    }

    /// Runs the supervisor until it exits on its own: its status and log.
    fn run_to_exit(&self, overrides: &str, within: Duration) -> (i32, String) {
        let mut r = self.start(overrides);
        let start = Instant::now();
        loop {
            if let Some(s) = r.child.try_wait().unwrap() {
                return (s.code().unwrap_or(-1), r.log());
            }
            if start.elapsed() > within {
                let _ = r.child.kill();
                panic!("the supervisor kept running: {}", r.log());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn healthy(&self) -> bool {
        let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", self.port)) else {
            return false;
        };
        let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
        use std::io::Write;
        if s.write_all(b"GET /healthz HTTP/1.0\r\n\r\n").is_err() {
            return false;
        }
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out.starts_with("HTTP/1.1 200") || out.starts_with("HTTP/1.0 200")
    }

    fn json(&self, name: &str) -> Option<Value> {
        let text = std::fs::read_to_string(self.bin(name)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Rolled back from NEW to the image, which then passed its own
    /// probation.
    fn assert_rolled_back(&self) {
        assert!(!self.bin(NEW).exists(), "the failed overlay is deleted");
        assert!(!self.bin("active").exists(), "back on the image");
        assert!(!self.bin("rollback.json").exists());
        assert_eq!(
            self.json("skipped").unwrap(),
            json!([{"version": NEW, "reason": "failed"}])
        );
        let history = self.json("history.json").unwrap();
        assert_eq!(history["entries"].as_array().unwrap().len(), 1, "{history}");
        assert_eq!(history["entries"][0]["result"], "rolled_back");
        assert_eq!(marker(&self.db()), "first", "the snapshot is restored");
    }
}

impl Running {
    fn log(&mut self) -> String {
        let mut out = String::new();
        while let Ok(l) = self.lines.recv_timeout(Duration::from_millis(200)) {
            out.push_str(&l);
            out.push('\n');
        }
        out
    }

    fn stop(mut self) -> i32 {
        let pid = self.child.id().to_string();
        assert!(
            Command::new("kill")
                .args(["-TERM", &pid])
                .status()
                .unwrap()
                .success()
        );
        let status = self.child.wait().unwrap();
        status.code().unwrap_or(-1)
    }
}

fn wait_until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < within, "{what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn set_marker(store: &Store, v: &str) {
    store
        .connection()
        .execute(
            "INSERT INTO settings (key, value) VALUES ('marker', ?1)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [v],
        )
        .unwrap();
}

fn marker(db: &Path) -> String {
    let conn = lodestar_store::rusqlite::Connection::open(db).unwrap();
    conn.query_row("SELECT value FROM settings WHERE key = 'marker'", [], |r| {
        r.get(0)
    })
    .unwrap()
}

fn hash(path: &Path) -> (String, u64) {
    let mut f = std::fs::File::open(path).unwrap();
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut len = 0;
    loop {
        let n = f.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        len += n as u64;
        h.update(&buf[..n]);
    }
    let sha = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    (sha, len)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn children(pid: u32) -> Vec<u32> {
    std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect()
}

const LONG: Duration = Duration::from_secs(120);

/// A binary that crashes on start, one that hangs without confirming, and
/// one that fails its self-test each end with the snapshot and the image
/// restored, the overlay deleted and the version skipped.
#[test]
fn failed_probation_rolls_back() {
    for fault in ["crash", "hang", "selftest"] {
        let env = Env::new();
        env.stage(NEW, None, now() + 86_400);
        env.migrated_by(NEW);
        env.activate(NEW, true);
        let r = env.start(&env.overrides(json!({"fault": format!("child:{fault}@{NEW}")})));
        wait_until(&format!("{fault}: no rollback"), LONG, || {
            env.json("skipped").is_some() && !env.bin("pending").exists() && env.healthy()
        });
        env.assert_rolled_back();
        assert_eq!(r.stop(), 0, "{fault}");
    }
}

/// A kill right after each journal step, then a restart: the rollback
/// finishes, the store holds the snapshot (never the old WAL), and
/// history has one entry.
#[test]
fn a_crash_at_every_journal_step_finishes_at_the_next_start() {
    for k in 2..=7 {
        let env = Env::new();
        env.stage(NEW, None, now() + 86_400);
        env.migrated_by(NEW);
        env.activate(NEW, true);
        let crash = env.overrides(json!({"fault": format!("child:crash@{NEW},journal:{k}")}));
        let (status, log) = env.run_to_exit(&crash, LONG);
        assert_eq!(status, 86, "journal:{k}: {log}");
        if k < 7 {
            assert!(env.bin("rollback.json").exists(), "journal:{k}");
            // Nothing opens the store meanwhile.
            let out = env
                .command(&crash, &["serve", "--child"])
                .stderr(Stdio::piped())
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(3), "journal:{k}");
        }
        let r = env.start(&env.overrides(json!({})));
        wait_until(&format!("journal:{k}: not finished"), LONG, || {
            !env.bin("pending").exists() && env.healthy()
        });
        env.assert_rolled_back();
        assert_eq!(r.stop(), 0);
    }
}

/// The restored image failing too halts instead of looping, until a newer
/// image arrives.
#[test]
fn a_failing_restored_version_halts_until_a_newer_image() {
    let env = Env::new();
    env.stage(NEW, None, now() + 86_400);
    env.activate(NEW, true);
    let both = env.overrides(json!({"fault": format!("child:crash@{NEW},child:crash@{IMAGE}")}));
    let (status, log) = env.run_to_exit(&both, LONG);
    assert_eq!(status, 1, "{log}");
    assert!(log.contains("won't start either"), "{log}");
    let halted = env.json("halted.json").unwrap();
    assert_eq!(
        (halted["failed"].as_str(), halted["previous"].as_str()),
        (Some(NEW), Some(IMAGE))
    );
    let (status, _) = env.run_to_exit(&env.overrides(json!({})), Duration::from_secs(20));
    assert_eq!(status, 1, "still halted");
    let r = env.start(&env.overrides(json!({"version": "1.0.0"})));
    wait_until("the newer image didn't start", LONG, || env.healthy());
    assert!(!env.bin("halted.json").exists());
    assert_eq!(r.stop(), 0);
}

/// An installed overlay starts with no network after its manifest
/// expired, runs the verified file, and passes; then a newer image runs
/// its own code and removes it.
#[test]
fn an_expired_overlay_starts_then_a_newer_image_wins() {
    let env = Env::new();
    env.stage(NEW, None, now() - 30 * 86_400);
    env.stage("0.4.0", None, now() + 86_400);
    env.activate(NEW, true);
    let r = env.start(&env.overrides(json!({})));
    wait_until("the overlay didn't pass", LONG, || {
        !env.bin("pending").exists() && env.healthy()
    });
    // What runs is the sealed in-memory copy that was verified, not the
    // file in `<data>/bin`.
    let kids = children(r.child.id());
    let exe = std::fs::read_link(format!("/proc/{}/exe", kids[0])).unwrap();
    assert!(
        exe.to_string_lossy().starts_with("/memfd:lodestar"),
        "{}",
        exe.display()
    );
    assert!(env.bin("active").exists() && env.bin(NEW).exists());
    assert!(
        env.bin("0.4.0").exists(),
        "nothing is deleted while the child runs"
    );
    assert_eq!(r.stop(), 0);
    assert!(
        !env.bin("0.4.0").exists(),
        "older overlays go once probation passed"
    );

    let r = env.start(&env.overrides(json!({"version": "1.0.0"})));
    wait_until("the newer image didn't start", LONG, || env.healthy());
    assert!(!env.bin("active").exists());
    assert!(!env.bin(NEW).exists());
    assert_eq!(r.stop(), 0);
}

/// A tampered overlay isn't run or deleted: it's set aside and the image
/// runs with the notice.
#[test]
fn a_tampered_overlay_falls_back_to_the_image() {
    let env = Env::new();
    env.stage(NEW, Some(b"#!/bin/sh\nexit 0\n"), now() + 86_400);
    std::fs::write(env.bin(NEW).join("lodestar"), b"#!/bin/sh\nexit 9\n").unwrap();
    env.activate(NEW, false);
    let mut r = env.start(&env.overrides(json!({})));
    wait_until("the image didn't start", LONG, || env.healthy());
    assert!(env.bin(&format!("{NEW}.bad")).join("lodestar").exists());
    let log = r.log();
    assert!(log.contains("failed verification"), "{log}");
    assert_eq!(r.stop(), 0);
}

/// One supervisor per data folder; signals reach the child; the child's
/// status passes through (0, 1, 128 + n).
#[test]
fn one_supervisor_per_folder_and_the_status_passes_through() {
    let env = Env::new();
    let o = env.overrides(json!({}));
    let r = env.start(&o);
    wait_until("it didn't start", LONG, || env.healthy());
    let (status, log) = env.run_to_exit(&o, Duration::from_secs(20));
    assert_eq!(status, 3, "{log}");
    assert!(
        log.contains("another Lodestar is using this data folder"),
        "{log}"
    );
    let kid = children(r.child.id())[0];
    assert!(
        Command::new("kill")
            .args(["-KILL", &kid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    let mut r = r;
    assert_eq!(r.child.wait().unwrap().code(), Some(128 + 9));

    let (status, _) = env.run_to_exit(
        &env.overrides(json!({"fault": format!("child:crash@{IMAGE}")})),
        Duration::from_secs(30),
    );
    assert_eq!(status, 1);
    // HUP, USR1 and USR2 are forwarded; the child doesn't handle them,
    // so its status (128 + n) is the supervisor's.
    for (sig, n) in [("HUP", 1), ("USR1", 10), ("USR2", 12)] {
        let mut r = env.start(&o);
        wait_until("it didn't start", LONG, || env.healthy());
        let pid = r.child.id().to_string();
        assert!(
            Command::new("kill")
                .args([&format!("-{sig}"), &pid])
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(r.child.wait().unwrap().code(), Some(128 + n), "{sig}");
    }
    let r = env.start(&o);
    wait_until("it didn't start", LONG, || env.healthy());
    assert_eq!(r.stop(), 0, "a forwarded TERM stops the child cleanly");
}

/// `lodestar rollback --update` runs the journal offline, and refuses
/// while the server holds the lock.
#[test]
fn rollback_update_runs_offline_only() {
    let env = Env::new();
    env.stage(NEW, None, now() + 86_400);
    env.activate(NEW, false);
    let o = env.overrides(json!({}));
    let r = env.start(&o);
    wait_until("it didn't start", LONG, || env.healthy());
    let out = env
        .command(&o, &["rollback", "--update"])
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(r.stop(), 0);
    // No terminal and no --yes: refused, nothing changed.
    let out = env
        .command(&o, &["rollback", "--update"])
        .stdout(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(env.bin("active").exists());
    let out = env
        .command(&o, &["rollback", "--update", "--yes"])
        .stdout(Stdio::piped())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("the store is kept as it is"));
    assert!(String::from_utf8_lossy(&out.stdout).contains("back to image"));
    assert!(!env.bin("active").exists() && !env.bin(NEW).exists());
    assert_eq!(env.json("skipped").unwrap()[0]["version"], NEW);
}

fn attempts(env: &Env) -> u64 {
    env.json("pending")
        .and_then(|p| p["attempts"].as_u64())
        .unwrap_or(99)
}

/// A stop the supervisor forwards during probation uses no attempt, and
/// neither does a supervised restart (`EXIT_RESTART`).
#[test]
fn stops_and_restarts_in_probation_use_no_attempt() {
    let env = Env::new();
    env.stage(NEW, None, now() + 86_400);
    env.activate(NEW, true);
    let r = env.start(
        &env.overrides(json!({"fault": format!("child:hang@{NEW}"), "probation_ms": 60_000})),
    );
    wait_until("no attempt charged", LONG, || attempts(&env) == 1);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(r.stop(), 128 + 15, "the child's status passes through");
    assert_eq!(attempts(&env), 0, "the forwarded stop was refunded");

    let r = env.start(
        &env.overrides(json!({"fault": format!("child:restart@{NEW}"), "probation_ms": 60_000})),
    );
    std::thread::sleep(Duration::from_secs(10));
    assert!(env.json("skipped").is_none(), "restarts aren't failures");
    assert_eq!(
        attempts(&env),
        1,
        "only the current run's attempt is charged"
    );
    assert_eq!(r.stop(), 0);
}

/// A migration that stops beating fails; so does one past the cap; one
/// longer than the probation deadline that keeps beating passes.
#[test]
fn migrations_are_judged_by_their_heartbeat_and_cap() {
    for (fault, extra) in [
        ("stall", json!({"stall_ms": 2000})),
        (
            "migrate=60",
            json!({"beat_ms": 300, "stall_ms": 10_000, "migration_cap_ms": 2000}),
        ),
    ] {
        let env = Env::new();
        env.stage(NEW, None, now() + 86_400);
        env.activate(NEW, true);
        let mut o: Value = extra;
        o["fault"] = json!(format!("child:{fault}@{NEW}"));
        let r = env.start(&env.overrides(o));
        wait_until(&format!("{fault}: no rollback"), LONG, || {
            env.json("skipped").is_some() && !env.bin("pending").exists() && env.healthy()
        });
        assert!(!env.bin(NEW).exists(), "{fault}");
        assert_eq!(
            marker(&env.db()),
            "first",
            "{fault}: no snapshot of its own, nothing restored"
        );
        let history = env.json("history.json").unwrap();
        assert_eq!(history["entries"][0]["lost_torrents"], 0, "{fault}");
        assert_eq!(r.stop(), 0);
    }
    let env = Env::new();
    env.stage(NEW, None, now() + 86_400);
    env.activate(NEW, true);
    let o = json!({"fault": format!("child:migrate=8@{NEW}"), "beat_ms": 300,
                   "stall_ms": 3000, "probation_ms": 5000});
    let r = env.start(&env.overrides(o));
    wait_until("the long migration didn't pass", LONG, || {
        !env.bin("pending").exists() && env.healthy()
    });
    assert!(env.bin("active").exists() && env.json("skipped").is_none());
    assert_eq!(r.stop(), 0);
}

/// A stop signal during a rollback finishes the current step only, leaves
/// the journal, starts nothing and exits non-zero; the next start ends it.
#[test]
fn a_stop_during_a_rollback_leaves_the_journal() {
    let env = Env::new();
    env.stage(NEW, None, now() + 86_400);
    env.migrated_by(NEW);
    env.activate(NEW, true);
    let mut r =
        env.start(&env.overrides(json!({"fault": format!("child:crash@{NEW},journal:slow")})));
    wait_until("no journal", LONG, || env.bin("rollback.json").exists());
    let pid = r.child.id();
    assert!(
        Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    let status = r.child.wait().unwrap();
    assert_eq!(status.code(), Some(1), "{}", r.log());
    assert!(env.bin("rollback.json").exists());
    assert!(children(pid).is_empty());
    let r = env.start(&env.overrides(json!({})));
    wait_until("not finished", LONG, || {
        !env.bin("pending").exists() && env.healthy()
    });
    env.assert_rolled_back();
    assert_eq!(r.stop(), 0);
}

/// `EXIT_SWITCH` verifies and switches; with `pending` unwritable (a full
/// disk) nothing switches and the current version keeps running.
#[test]
fn a_switch_needs_pending_written_first() {
    let env = Env::new();
    env.stage(NEW, None, now() + 86_400);
    std::fs::create_dir_all(env.bin(".pending.tmp")).unwrap();
    std::fs::write(env.bin(".pending.tmp").join("x"), b"x").unwrap();
    let o = env.overrides(json!({"fault": format!("child:switch={NEW}@{IMAGE}")}));
    let mut r = env.start(&o);
    let mut log = String::new();
    wait_until("no refusal", LONG, || {
        log.push_str(&r.log());
        log.contains("can't write pending")
    });
    assert!(!env.bin("active").exists() && !env.bin("pending").exists());
    r.stop();

    std::fs::remove_dir_all(env.bin(".pending.tmp")).unwrap();
    let r = env.start(&o);
    wait_until("no switch", LONG, || {
        env.json("active").is_some_and(|a| a["version"] == NEW)
            && !env.bin("pending").exists()
            && env.healthy()
    });
    assert_eq!(env.json("active").unwrap()["previous"], "image");
    assert_eq!(r.stop(), 0);
}

/// An overlay signed by a key this build's embedded statements revoke is
/// refused; an overlay no newer than the image is removed.
#[test]
fn revoked_and_not_newer_overlays_dont_run() {
    use lodestar_update::Role;
    use lodestar_update::testing::{TestKey, statement};
    let env = Env::new();
    env.stage(NEW, None, now() + 86_400);
    env.activate(NEW, false);
    let t = &env.trust;
    let key = |k: &TestKey| k.public().to_base64();
    let rotation = statement(
        &t.recovery,
        1,
        Role::Release,
        &TestKey::new(5).public(),
        now() - 60,
    );
    let o = env.overrides(json!({"trust_root": {
        "recovery": key(&t.recovery), "release": key(&t.release),
        "nightly": key(&t.nightly), "freshness": key(&t.freshness),
        "statements": [rotation],
    }}));
    let mut r = env.start(&o);
    wait_until("the image didn't start", LONG, || env.healthy());
    let log = r.log();
    assert!(log.contains("signed by a key this build revokes"), "{log}");
    assert_eq!(r.stop(), 0);

    let env = Env::new();
    env.stage(IMAGE, None, now() + 86_400);
    env.activate(IMAGE, false);
    let r = env.start(&env.overrides(json!({})));
    wait_until("it didn't start", LONG, || env.healthy());
    assert!(!env.bin("active").exists() && !env.bin(IMAGE).exists());
    assert_eq!(r.stop(), 0);
}

/// A grandchild left behind is reparented to the supervisor and reaped.
#[test]
fn orphans_are_reaped() {
    let env = Env::new();
    let r = env.start(&env.overrides(json!({"fault": format!("child:orphan@{IMAGE}")})));
    let pid = r.child.id();
    wait_until("the orphan never came to the supervisor", LONG, || {
        children(pid).len() == 2
    });
    wait_until("the orphan wasn't reaped", LONG, || {
        children(pid).len() == 1
    });
    for kid in children(pid) {
        let stat = std::fs::read_to_string(format!("/proc/{kid}/stat")).unwrap();
        assert!(!stat.contains(") Z "), "a zombie: {stat}");
    }
    assert_eq!(r.stop(), 0);
}

/// As if `version` had migrated the store with no snapshot of its own
/// left: `migration.json` says it began at an older schema than the
/// store's now.
fn migrated_without_a_snapshot(env: &Env, version: &str) {
    let latest = lodestar_store::latest_schema_version();
    assert!(latest >= 2);
    std::fs::write(
        env.bin(version).join("migration.json"),
        json!({"from": latest - 1, "to": latest, "started_at": 1}).to_string(),
    )
    .unwrap();
}

/// A version that migrated the store, with no snapshot that verifies, is
/// never rolled back: after its attempts it halts; a requested rollback
/// keeps it running; `rollback --update` refuses. The store and `active`
/// are untouched throughout.
#[test]
fn a_migrated_store_without_a_snapshot_is_never_rolled_back() {
    let env = Env::new();
    env.stage(NEW, None, now() + 86_400);
    migrated_without_a_snapshot(&env, NEW);
    env.activate(NEW, true);
    let o = env.overrides(json!({"fault": format!("child:crash@{NEW}")}));
    let (status, log) = env.run_to_exit(&o, LONG);
    assert_eq!(status, 1, "{log}");
    assert!(log.contains("can't be rolled back"), "{log}");
    assert!(!env.bin("rollback.json").exists());
    assert_eq!(env.json("active").unwrap()["version"], NEW);
    assert!(env.json("halted.json").unwrap()["refused"].is_string());
    assert_eq!(marker(&env.db()), "first");
    let (status, _) = env.run_to_exit(&env.overrides(json!({})), Duration::from_secs(20));
    assert_eq!(status, 1, "still halted");

    let out = env
        .command(&o, &["rollback", "--update", "--yes"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no snapshot"),
        "{out:?}"
    );
    assert!(!env.bin("rollback.json").exists() && env.bin(NEW).exists());

    std::fs::remove_file(env.bin("halted.json")).unwrap();
    let mut r = env.start(&env.overrides(json!({"fault": format!("child:rollback@{NEW}")})));
    let mut log = String::new();
    wait_until("no refusal", LONG, || {
        log.push_str(&r.log());
        log.contains("not rolling")
    });
    assert_eq!(env.json("active").unwrap()["version"], NEW);
    assert!(env.json("skipped").is_none());
    assert_eq!(r.stop(), 0);
}

/// A child that can't record its migration doesn't start: each attempt
/// fails, and the rollback (nothing to restore) goes back to the image.
#[test]
fn a_migration_that_cant_be_recorded_never_starts() {
    let env = Env::new();
    env.stage(NEW, None, now() + 86_400);
    env.activate(NEW, true);
    let mut r = env.start(&env.overrides(json!({"fault": format!("child:unrecorded@{NEW}")})));
    wait_until("no rollback", LONG, || {
        env.json("skipped").is_some() && !env.bin("pending").exists() && env.healthy()
    });
    assert!(!env.bin(NEW).exists());
    let log = r.log();
    assert!(log.contains("recording the store upgrade"), "{log}");
    assert_eq!(r.stop(), 0);
}

/// Versions only go up: a switch to an older version is refused.
#[test]
fn a_switch_to_an_older_version_is_refused() {
    let env = Env::new();
    env.stage("0.4.0", None, now() + 86_400);
    let mut r = env.start(&env.overrides(json!({"fault": format!("child:switch=0.4.0@{IMAGE}")})));
    let mut log = String::new();
    wait_until("no refusal", LONG, || {
        log.push_str(&r.log());
        log.contains("versions only go up")
    });
    assert!(!env.bin("active").exists() && !env.bin("pending").exists());
    r.stop();
}
