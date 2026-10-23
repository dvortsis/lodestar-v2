//! The real binary on Windows (architecture §11): as a service registered
//! with `sc.exe` (started, serving, logging to its file at below-normal
//! priority, then stopped gracefully), and as an ordinary process with no
//! console, which must not wait on the Service Control Manager.
#![cfg(windows)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn healthz(port: u16) -> Option<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    s.write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .ok()?;
    let mut out = String::new();
    s.read_to_string(&mut out).ok()?;
    Some(out)
}

/// Polls `check` every 250 ms until it returns `Some` or `limit` passes.
fn wait_for<T>(limit: Duration, mut check: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    loop {
        if let Some(v) = check() {
            return Some(v);
        }
        if start.elapsed() > limit {
            return None;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// A config in `dir`: data beside it, HTTP on `port`, any DHT port.
fn write_config(dir: &Path, port: u16) -> std::path::PathBuf {
    let path = dir.join("config.toml");
    std::fs::write(
        &path,
        format!(
            "data_dir = '{}'\nlog = \"info\"\n[http]\nbind = \"127.0.0.1:{port}\"\n\
             [dht]\nbind = [\"0.0.0.0:0\"]\n",
            dir.join("data").display()
        ),
    )
    .unwrap();
    path
}

#[test]
fn an_ordinary_run_without_a_console_serves() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let config = write_config(dir.path(), port);
    let mut child = Command::new(env!("CARGO_BIN_EXE_lodestar"))
        .arg("--config")
        .arg(&config)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let health = wait_for(Duration::from_secs(60), || healthz(port));
    let _ = child.kill();
    let _ = child.wait();
    let health = health.expect("server didn't come up");
    assert!(health.starts_with("HTTP/1.0 200"), "{health}");
}

fn sc(args: &[&str]) -> String {
    let out = Command::new("sc.exe").args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A field of `sc query`/`sc queryex` output: `STATE : 4  RUNNING` → `4`.
fn field(out: &str, name: &str) -> Option<String> {
    out.lines()
        .find(|l| l.trim_start().starts_with(name))
        .and_then(|l| l.split_once(':'))
        .and_then(|(_, v)| v.split_whitespace().next())
        .map(String::from)
}

/// Deletes the test service however the test ends.
struct Registered(String);

impl Drop for Registered {
    fn drop(&mut self) {
        sc(&["stop", &self.0]);
        wait_for(Duration::from_secs(60), || {
            (field(&sc(&["query", &self.0]), "STATE").as_deref() != Some("3")).then_some(())
        });
        sc(&["delete", &self.0]);
    }
}

#[test]
fn runs_as_a_service() {
    // Registering a service needs an administrator: CI's runner is one.
    if std::env::var_os("CI").is_none() && std::env::var_os("LODESTAR_SERVICE_TEST").is_none() {
        eprintln!("skipped: set LODESTAR_SERVICE_TEST=1 in an administrator shell to run");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let config = write_config(dir.path(), port);
    let log_path = dir.path().join("data").join("logs").join("lodestar.log");
    let log = || std::fs::read_to_string(&log_path).unwrap_or_default();

    let name = format!("LodestarTest{}", std::process::id());
    let bin_path = format!(
        "\"{}\" --config \"{}\"",
        env!("CARGO_BIN_EXE_lodestar"),
        config.display()
    );
    let created = sc(&["create", &name, "binPath=", &bin_path, "start=", "demand"]);
    assert!(created.contains("SUCCESS"), "sc create: {created}");
    let service = Registered(name);
    sc(&["start", &service.0]);

    let health = wait_for(Duration::from_secs(90), || healthz(port));
    let health = health.unwrap_or_else(|| {
        panic!(
            "service didn't come up: {}\nlog:\n{}",
            sc(&["query", &service.0]),
            log()
        )
    });
    assert!(health.starts_with("HTTP/1.0 200"), "{health}");
    assert!(log().contains("service: starting"), "{}", log());
    // The first run's setup token is only in the file.
    wait_for(Duration::from_secs(10), || {
        log().contains("setup token ").then_some(())
    })
    .unwrap_or_else(|| panic!("no setup token in the log:\n{}", log()));

    // The default data folder was secured before anything was read:
    // nothing inherited from ProgramData, nothing for Users.
    let program_data = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
    let secured = Path::new(&program_data).join("Lodestar");
    assert!(secured.join(".lodestar-secured").exists());
    let out = Command::new("icacls.exe").arg(&secured).output().unwrap();
    let acl = String::from_utf8_lossy(&out.stdout);
    assert!(acl.contains("NT AUTHORITY\\SYSTEM"), "{acl}");
    assert!(acl.contains("BUILTIN\\Administrators"), "{acl}");
    assert!(acl.contains(&format!("NT SERVICE\\{}", service.0)), "{acl}");
    assert!(!acl.contains("BUILTIN\\Users"), "{acl}");
    assert!(!acl.contains("(I)"), "{acl}");

    let pid = field(&sc(&["queryex", &service.0]), "PID").expect("a PID");
    let priority = wait_for(Duration::from_secs(30), || {
        let out = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!("(Get-Process -Id {pid}).PriorityClass"))
            .output()
            .ok()?;
        let class = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (class == "BelowNormal").then_some(class)
    });
    assert!(priority.is_some(), "not below normal; log:\n{}", log());

    sc(&["stop", &service.0]);
    let stopped = wait_for(Duration::from_secs(60), || {
        let out = sc(&["query", &service.0]);
        (field(&out, "STATE").as_deref() == Some("1")).then_some(out)
    });
    let stopped = stopped.unwrap_or_else(|| panic!("didn't stop; log:\n{}", log()));
    assert_eq!(
        field(&stopped, "WIN32_EXIT_CODE").as_deref(),
        Some("0"),
        "{stopped}"
    );
    assert_eq!(
        field(&stopped, "SERVICE_EXIT_CODE").as_deref(),
        Some("0"),
        "{stopped}"
    );
    let log = log();
    assert!(log.contains("service: stopping"), "{log}");
    assert!(log.contains("shutting down"), "{log}");
    assert!(log.contains("service: stopped"), "{log}");
}

#[test]
fn reads_sc_output() {
    let out = "SERVICE_NAME: x\r\n        TYPE               : 10  WIN32_OWN_PROCESS\r\n\
               \x20       STATE              : 4  RUNNING\r\n\
               \x20       WIN32_EXIT_CODE    : 0  (0x0)\r\n\
               \x20       PID                : 4242\r\n";
    assert_eq!(field(out, "STATE").as_deref(), Some("4"));
    assert_eq!(field(out, "PID").as_deref(), Some("4242"));
    assert_eq!(field(out, "WIN32_EXIT_CODE").as_deref(), Some("0"));
    assert_eq!(field(out, "FLAGS"), None);
}
