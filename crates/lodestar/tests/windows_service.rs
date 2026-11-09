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

mod common;

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

/// A config in `dir`: data beside it, HTTP and the DHT on any port.
fn write_config(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("config.toml");
    std::fs::write(
        &path,
        format!(
            "data_dir = '{}'\nlog = \"info\"\n[http]\nbind = \"{}\"\n\
             [dht]\nbind = [\"0.0.0.0:0\"]\n",
            dir.join("data").display(),
            common::ANY_PORT
        ),
    )
    .unwrap();
    path
}

/// The port the server's `app.serving` line in `log_path` names.
fn port_in(log_path: &Path, limit: Duration) -> Option<u16> {
    wait_for(limit, || {
        common::serving_port(&std::fs::read_to_string(log_path).unwrap_or_default())
    })
}

#[test]
fn an_ordinary_run_without_a_console_serves() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(dir.path());
    let mut child = Command::new(env!("CARGO_BIN_EXE_lodestar"))
        .arg("--config")
        .arg(&config)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let jsonl = dir.path().join("data").join("logs").join("lodestar.jsonl");
    let health = port_in(&jsonl, Duration::from_secs(60))
        .and_then(|port| wait_for(Duration::from_secs(60), || healthz(port)));
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

/// Deletes the test service, and `cleanup` if set, however the test ends.
struct Registered {
    name: String,
    cleanup: Option<std::path::PathBuf>,
}

impl Drop for Registered {
    fn drop(&mut self) {
        sc(&["stop", &self.name]);
        wait_for(Duration::from_secs(60), || {
            (field(&sc(&["query", &self.name]), "STATE").as_deref() != Some("3")).then_some(())
        });
        sc(&["delete", &self.name]);
        if let Some(dir) = &self.cleanup {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Registering a service needs an administrator: CI's runner is one.
fn can_register() -> bool {
    let ok =
        std::env::var_os("CI").is_some() || std::env::var_os("LODESTAR_SERVICE_TEST").is_some();
    if !ok {
        eprintln!("skipped: set LODESTAR_SERVICE_TEST=1 in an administrator shell to run");
    }
    ok
}

/// Registers service `name` running `exe` as `account` (LocalSystem when
/// `None`). Its environment points `%ProgramData%` at `program_data`, so
/// its default data folder is `program_data\Lodestar`, and lets HTTP and
/// the DHT take any port.
fn register(
    name: &str,
    exe: &Path,
    account: Option<&str>,
    program_data: &Path,
    cleanup: Option<std::path::PathBuf>,
) -> Registered {
    let bin_path = format!("\"{}\"", exe.display());
    let mut args = vec!["create", name, "binPath=", &bin_path, "start=", "demand"];
    if let Some(account) = account {
        args.extend(["obj=", account]);
    }
    let created = sc(&args);
    assert!(created.contains("SUCCESS"), "sc create: {created}");
    let service = Registered {
        name: name.to_string(),
        cleanup,
    };
    let env = format!(
        "ProgramData={}\\0LODESTAR_HTTP_BIND={}\\0LODESTAR_DHT_PORT=0\\0LODESTAR_LOG=info",
        program_data.display(),
        common::ANY_PORT
    );
    let key = format!("HKLM\\SYSTEM\\CurrentControlSet\\Services\\{name}");
    let out = Command::new("reg.exe")
        .args([
            "add",
            &key,
            "/v",
            "Environment",
            "/t",
            "REG_MULTI_SZ",
            "/d",
            &env,
            "/f",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "reg add: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    service
}

/// The service's SID, from `sc showsid`.
fn service_sid(name: &str) -> String {
    sc(&["showsid", name])
        .split_whitespace()
        .find(|w| w.starts_with("S-1-5-80-"))
        .expect("a service SID")
        .to_string()
}

/// A folder's security descriptor in SDDL.
fn sddl(path: &Path) -> String {
    let out = Command::new("powershell.exe")
        .env_remove("PSModulePath")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!("(Get-Acl -LiteralPath '{}').Sddl", path.display()))
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Starts `service`, waits for /healthz on the port its log names and
/// returns the log.
fn start_and_check(service: &Registered, log_path: &Path) -> String {
    let log = || std::fs::read_to_string(log_path).unwrap_or_default();
    sc(&["start", &service.name]);
    let health = port_in(log_path, Duration::from_secs(90))
        .and_then(|port| wait_for(Duration::from_secs(90), || healthz(port)));
    let health = health.unwrap_or_else(|| {
        panic!(
            "service didn't come up: {}\nlog:\n{}",
            sc(&["query", &service.name]),
            log()
        )
    });
    assert!(health.starts_with("HTTP/1.0 200"), "{health}");
    assert!(log().contains("service: starting"), "{}", log());
    // The first run's setup token is only in the file.
    wait_for(Duration::from_secs(30), || {
        log().contains("setup token ").then_some(())
    })
    .unwrap_or_else(|| panic!("no setup token in the log:\n{}", log()));
    log()
}

/// Stops `service` and returns `sc query` once it's stopped.
fn stop(service: &Registered) -> String {
    sc(&["stop", &service.name]);
    wait_until_stopped(service)
}

fn wait_until_stopped(service: &Registered) -> String {
    wait_for(Duration::from_secs(90), || {
        let out = sc(&["query", &service.name]);
        (field(&out, "STATE").as_deref() == Some("1")).then_some(out)
    })
    .unwrap_or_else(|| panic!("didn't stop: {}", sc(&["query", &service.name])))
}

#[test]
fn runs_as_a_service() {
    if !can_register() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let name = format!("LodestarTest{}", std::process::id());
    let exe = Path::new(env!("CARGO_BIN_EXE_lodestar"));
    let service = register(&name, exe, None, root.path(), None);
    let data = root.path().join("Lodestar");
    let log_path = data.join("logs").join("lodestar.log");
    start_and_check(&service, &log_path);

    // The service created its data folder private: owned by SYSTEM or
    // Administrators, nothing inherited, only SYSTEM, Administrators and
    // its own SID.
    let sd = sddl(&data);
    assert!(sd.starts_with("O:BA") || sd.starts_with("O:SY"), "{sd}");
    assert!(sd.contains("D:PAI") || sd.contains("D:P("), "{sd}");
    assert!(sd.contains(&service_sid(&name)), "{sd}");
    for other in [";BU)", ";AU)", ";LS)", ";WD)", "ID;"] {
        assert!(!sd.contains(other), "{other} in {sd}");
    }

    let pid = field(&sc(&["queryex", &service.name]), "PID").expect("a PID");
    let priority = wait_for(Duration::from_secs(30), || {
        let out = Command::new("powershell.exe")
            .env_remove("PSModulePath")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!("(Get-Process -Id {pid}).PriorityClass"))
            .output()
            .ok()?;
        let class = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (class == "BelowNormal").then_some(class)
    });
    let log = || std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(priority.is_some(), "not below normal; log:\n{}", log());

    let stopped = stop(&service);
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

/// As the installer sets it up: the service runs as its own virtual
/// account, which can't change ACLs, in a folder made private beforehand.
/// The check of the installer's ACL is then the only gate, and a folder
/// that also lets Users in is refused (exit code 3).
#[test]
fn runs_as_its_virtual_account_in_the_installers_folder() {
    if !can_register() {
        return;
    }
    // Outside any user profile, which the virtual account can't enter.
    let root = Path::new(r"C:\").join(format!("lodestar-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("lodestar.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_lodestar"), &exe).unwrap();
    let program_data = root.join("pd");
    let data = program_data.join("Lodestar");
    std::fs::create_dir_all(&data).unwrap();

    let name = format!("LodestarVa{}", std::process::id());
    let account = format!("NT SERVICE\\{name}");
    let service = register(
        &name,
        &exe,
        Some(&account),
        &program_data,
        Some(root.clone()),
    );
    let sid = service_sid(&name);
    let icacls = |args: &[&str]| {
        let out = Command::new("icacls.exe")
            .arg(&data)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "icacls {args:?}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    };
    icacls(&[
        "/inheritance:r",
        "/grant:r",
        "*S-1-5-18:(OI)(CI)F",
        "*S-1-5-32-544:(OI)(CI)F",
        &format!("*{sid}:(OI)(CI)F"),
    ]);

    let log_path = data.join("logs").join("lodestar.log");
    start_and_check(&service, &log_path);
    let stopped = stop(&service);
    assert_eq!(
        field(&stopped, "SERVICE_EXIT_CODE").as_deref(),
        Some("0"),
        "{stopped}"
    );

    // Users get read access: refused before anything is read or written.
    icacls(&["/grant", "*S-1-5-32-545:(OI)(CI)R"]);
    let before = std::fs::read_to_string(&log_path).unwrap();
    sc(&["start", &service.name]);
    let stopped = wait_until_stopped(&service);
    assert_eq!(
        field(&stopped, "SERVICE_EXIT_CODE").as_deref(),
        Some("536870915"),
        "{stopped}"
    );
    assert_eq!(std::fs::read_to_string(&log_path).unwrap(), before);
    // The reason goes outside the folder, beside the installer's log.
    let root_dir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let reasons = std::fs::read_to_string(
        Path::new(&root_dir)
            .join("Temp")
            .join("lodestar-service.log"),
    )
    .unwrap_or_default();
    assert!(
        reasons.contains(&format!("{} isn't private", data.display())),
        "{reasons}"
    );
}

/// Without the installer, the virtual account creates the folder itself:
/// private from the start, owned by the service, and used.
#[test]
fn its_virtual_account_creates_a_missing_folder() {
    if !can_register() {
        return;
    }
    let root = Path::new(r"C:\").join(format!("lodestar-test-new-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("lodestar.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_lodestar"), &exe).unwrap();
    let program_data = root.join("pd");
    std::fs::create_dir_all(&program_data).unwrap();
    let data = program_data.join("Lodestar");

    let name = format!("LodestarVn{}", std::process::id());
    let account = format!("NT SERVICE\\{name}");
    let service = register(
        &name,
        &exe,
        Some(&account),
        &program_data,
        Some(root.clone()),
    );
    start_and_check(&service, &data.join("logs").join("lodestar.log"));
    let sid = service_sid(&name);
    let sd = sddl(&data);
    assert!(sd.starts_with(&format!("O:{sid}")), "{sd}");
    assert!(sd.contains("D:PAI") || sd.contains("D:P("), "{sd}");
    for other in [";BU)", ";AU)", ";LS)", ";WD)", "ID;"] {
        assert!(!sd.contains(other), "{other} in {sd}");
    }
    let stopped = stop(&service);
    assert_eq!(
        field(&stopped, "SERVICE_EXIT_CODE").as_deref(),
        Some("0"),
        "{stopped}"
    );
}

/// An HTTP/1.0 request to the server on `port`, with the body after it.
fn request(port: u16, head: &str, body: &str) -> Option<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
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

fn json_body(response: &str) -> serde_json::Value {
    let (_, body) = response.split_once("\r\n\r\n").expect("a body");
    serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {response}"))
}

/// Claims the instance with the setup token in `log`; returns the cookie.
fn sign_in(port: u16, log: &str) -> String {
    let token = log
        .lines()
        .rev()
        .find_map(|l| l.split("setup token ").nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("no setup token in:\n{log}"));
    let setup = request(
        port,
        "POST /api/v1/setup HTTP/1.0\r\nContent-Type: application/json",
        &format!(
            r#"{{"token":"{token}","password":"correct horse battery","notice_version":{v}}}"#,
            v = lodestar_api::legal::NOTICE_VERSION
        ),
    )
    .unwrap_or_default();
    assert!(setup.starts_with("HTTP/1.0 200"), "{setup}");
    setup
        .lines()
        .find_map(|l| l.strip_prefix("set-cookie: "))
        .and_then(|c| c.split(';').next())
        .expect("a session cookie")
        .to_string()
}

fn signed_in(port: u16, method: &str, path: &str, cookie: &str) -> String {
    request(
        port,
        &format!("{method} {path} HTTP/1.0\r\nCookie: {cookie}\r\nContent-Type: application/json"),
        "",
    )
    .unwrap_or_default()
}

/// `/system`'s `restart`.
fn restart_info(port: u16, cookie: &str) -> serde_json::Value {
    let info = signed_in(port, "GET", "/api/v1/system", cookie);
    assert!(info.starts_with("HTTP/1.0 200"), "{info}");
    json_body(&info)["restart"].clone()
}

/// The port of the `n`th `app.serving` line (from 1) in `log_path`.
fn nth_port(log_path: &Path, n: usize, limit: Duration) -> Option<u16> {
    wait_for(limit, || {
        let log = std::fs::read_to_string(log_path).unwrap_or_default();
        (log.matches("app.serving").count() >= n)
            .then(|| common::serving_port(&log))
            .flatten()
    })
}

/// Restart now as a service (PK-win-restart): the graceful shutdown, then
/// the dedicated service-specific code (`0x20000050`) for the SCM's
/// recovery action, and the next start, inside the 24 hours, doesn't offer
/// it again. A service registered with `sc.exe` has no recovery action, so
/// it stays stopped here; `packaging/windows/smoke.ps1` sees the
/// installed service come back.
#[test]
fn restart_now_exits_for_the_scm() {
    if !can_register() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let name = format!("LodestarRs{}", std::process::id());
    let exe = Path::new(env!("CARGO_BIN_EXE_lodestar"));
    let service = register(&name, exe, None, root.path(), None);
    let data = root.path().join("Lodestar");
    let log_path = data.join("logs").join("lodestar.log");
    let log = start_and_check(&service, &log_path);
    let port = port_in(&log_path, Duration::from_secs(10)).expect("a port");
    let cookie = sign_in(port, &log);

    let info = restart_info(port, &cookie);
    assert_eq!(info["method"], "service", "{info}");
    assert_eq!(info["available"], true, "{info}");
    assert!(info["estimated_seconds"].as_u64().unwrap() > 60, "{info}");

    let restart = signed_in(port, "POST", "/api/v1/system/restart", &cookie);
    assert!(restart.starts_with("HTTP/1.0 202"), "{restart}");
    let stopped = wait_until_stopped(&service);
    assert_eq!(
        field(&stopped, "SERVICE_EXIT_CODE").as_deref(),
        Some("536870992"),
        "{stopped}"
    );
    let log = std::fs::read_to_string(&log_path).unwrap();
    assert!(log.contains("restart requested"), "{log}");
    assert!(log.contains("shutting down"), "{log}");
    assert!(
        log.contains("service: exiting so Windows starts it again"),
        "{log}"
    );
    let record = std::fs::read_to_string(data.join("service-restarts.txt")).unwrap();
    assert!(record.contains("state stopped\nrestart "), "{record}");

    // Started again within the day: paused, with the hint.
    sc(&["start", &service.name]);
    let port = nth_port(&log_path, 2, Duration::from_secs(90)).expect("started again");
    wait_for(Duration::from_secs(90), || healthz(port)).expect("healthz after the restart");
    let info = restart_info(port, &cookie);
    assert_eq!(info["available"], false, "{info}");
    assert_eq!(info["method"], "manual", "{info}");
    assert!(
        info["hint"].as_str().unwrap().contains("Windows Services"),
        "{info}"
    );
    let log = std::fs::read_to_string(&log_path).unwrap();
    assert!(log.contains("Restart now is paused"), "{log}");
    let stopped = stop(&service);
    assert_eq!(
        field(&stopped, "SERVICE_EXIT_CODE").as_deref(),
        Some("0"),
        "{stopped}"
    );
}

/// Stops any `lodestar.exe` whose command line names `config`: the copy a
/// relaunch started, which the test didn't spawn itself.
struct KillByConfig(std::path::PathBuf);

impl Drop for KillByConfig {
    fn drop(&mut self) {
        let config = self.0.display().to_string().replace('\'', "''");
        let _ = Command::new("powershell.exe")
            .env_remove("PSModulePath")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!(
                "Get-CimInstance Win32_Process -Filter \"Name='lodestar.exe'\" | \
                 Where-Object {{ $_.CommandLine -like '*{config}*' }} | \
                 ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force }}"
            ))
            .output();
    }
}

/// The console (stderr, which the relaunched copy shares) and
/// `lodestar.jsonl`, for a failure message: a copy that died at start
/// says why on the console.
fn dump(console: &std::sync::Mutex<String>, jsonl: &Path) -> String {
    format!(
        "--- console ---\n{}\n--- lodestar.jsonl ---\n{}",
        console.lock().unwrap_or_else(|e| e.into_inner()),
        std::fs::read_to_string(jsonl).unwrap_or_else(|e| format!("(unreadable: {e})"))
    )
}

/// Restart now in the portable build, with HTTP on `bind`: the old process
/// shuts down and exits with 80 (a new copy took over), and a new copy
/// with the same arguments (the same config, so the same data and log)
/// serves again, signed in as before. Returns the two ports.
fn portable_restart(bind: &str) -> (u16, u16) {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "data_dir = '{}'\nlog = \"info\"\n[http]\nbind = \"{bind}\"\n\
             [dht]\nbind = [\"0.0.0.0:0\"]\n",
            dir.path().join("data").display(),
        ),
    )
    .unwrap();
    let _cleanup = KillByConfig(config.clone());
    let mut child = Command::new(env!("CARGO_BIN_EXE_lodestar"))
        .arg("--config")
        .arg(&config)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The setup token is on the console (stderr here), and so is anything
    // the relaunched copy prints: it shares the old process's stderr.
    let stderr = child.stderr.take().unwrap();
    let console = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    std::thread::spawn({
        let console = console.clone();
        move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
            {
                let mut c = console.lock().unwrap();
                c.push_str(&line);
                c.push('\n');
            }
        }
    });
    let jsonl = dir.path().join("data").join("logs").join("lodestar.jsonl");
    let first = nth_port(&jsonl, 1, Duration::from_secs(60))
        .unwrap_or_else(|| panic!("server didn't come up\n{}", dump(&console, &jsonl)));
    wait_for(Duration::from_secs(60), || healthz(first)).expect("healthz");
    let token_logged = wait_for(Duration::from_secs(30), || {
        let c = console.lock().unwrap().clone();
        c.contains("setup token ").then_some(c)
    })
    .expect("a setup token on the console");
    let cookie = sign_in(first, &token_logged);
    let info = restart_info(first, &cookie);
    assert_eq!(info["method"], "relaunch", "{info}");
    assert_eq!(info["available"], true, "{info}");

    let restart = signed_in(first, "POST", "/api/v1/system/restart", &cookie);
    assert!(restart.starts_with("HTTP/1.0 202"), "{restart}");
    let status = wait_for(Duration::from_secs(90), || child.try_wait().ok().flatten())
        .unwrap_or_else(|| panic!("the old process didn't exit\n{}", dump(&console, &jsonl)));
    assert_eq!(
        status.code(),
        Some(80),
        "the old process exited with {status}\n{}",
        dump(&console, &jsonl)
    );

    // The new copy: same config, so the same log, and the same store.
    let second = nth_port(&jsonl, 2, Duration::from_secs(90))
        .unwrap_or_else(|| panic!("no new copy served\n{}", dump(&console, &jsonl)));
    let health = wait_for(Duration::from_secs(60), || healthz(second))
        .unwrap_or_else(|| panic!("no healthz from the new copy\n{}", dump(&console, &jsonl)));
    assert!(health.starts_with("HTTP/1.0 200"), "{health}");
    let info = restart_info(second, &cookie);
    assert_eq!(info["method"], "relaunch", "{info}");
    let log = std::fs::read_to_string(&jsonl).unwrap();
    assert!(log.contains("app.relaunch"), "{log}");
    (first, second)
}

/// Restart now in the portable build, on any port.
#[test]
fn a_portable_restart_starts_a_new_copy() {
    portable_restart(common::ANY_PORT);
}

/// Restart now in the portable build on a fixed port (X-win-restart-
/// followups): the new copy waits until the old process has exited and
/// Windows has released the port, then binds the same one.
#[test]
fn a_portable_restart_serves_again_on_the_same_port() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let (first, second) = portable_restart(&format!("127.0.0.1:{port}"));
    assert_eq!((first, second), (port, port));
}

/// A run with a config that doesn't load, in a console of its own (so
/// `CONIN$` opens, as in `Start Lodestar.cmd`'s window), its stderr kept.
/// `relaunched`: started as Restart now starts a copy, with the token in
/// its environment and the hand-over on stdin (the token, then end of
/// file, as the old process gives once it has exited).
fn bad_config_run(
    dir: &Path,
    relaunched: bool,
) -> (
    std::process::Child,
    std::sync::Arc<std::sync::Mutex<String>>,
) {
    use std::os::windows::process::CommandExt;
    /// `CREATE_NEW_CONSOLE`.
    const NEW_CONSOLE: u32 = 0x0000_0010;
    const TOKEN: &str = "lodestar-test-token";
    let config = dir.join("config.toml");
    std::fs::write(&config, "this is not [ toml").unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lodestar"));
    cmd.arg("--config")
        .arg(&config)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(NEW_CONSOLE)
        .env_remove("LODESTAR_RELAUNCH_TOKEN");
    if relaunched {
        cmd.env("LODESTAR_RELAUNCH_TOKEN", TOKEN);
    }
    let mut child = cmd.spawn().unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        if relaunched {
            writeln!(stdin, "{TOKEN}").unwrap();
        }
        // Dropped: end of file.
    }
    let stderr = child.stderr.take().unwrap();
    let console = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    std::thread::spawn({
        let console = console.clone();
        move || {
            use std::io::BufRead;
            let mut reader = std::io::BufReader::new(stderr);
            let mut line = Vec::new();
            // Read to each newline or the end; the last prompt has none.
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => console
                        .lock()
                        .unwrap()
                        .push_str(&String::from_utf8_lossy(&line)),
                }
            }
        }
    });
    (child, console)
}

/// X-win-restart-followups-2: a copy started by Restart now whose config
/// doesn't load says where to look and waits for Enter on its console,
/// instead of its window closing unread.
#[test]
fn a_relaunched_copy_that_fails_at_start_waits_for_enter() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, console) = bad_config_run(dir.path(), true);
    let shown = wait_for(Duration::from_secs(30), || {
        let c = console.lock().unwrap().clone();
        c.contains("Press Enter to close this window.").then_some(c)
    });
    let still_running = child.try_wait().ok().flatten().is_none();
    let _ = child.kill();
    let _ = child.wait();
    let shown = shown.unwrap_or_else(|| {
        panic!(
            "no prompt; console:\n{}",
            console.lock().unwrap_or_else(|e| e.into_inner())
        )
    });
    assert!(shown.contains("config:"), "{shown}");
    assert!(
        shown.contains("couldn't start again after Restart now"),
        "{shown}"
    );
    assert!(still_running, "it should wait for Enter:\n{shown}");
}

/// The same failure in a run Restart now didn't start ends at once, as
/// before: no prompt, a failure exit.
#[test]
fn an_ordinary_run_that_fails_at_start_exits() {
    let dir = tempfile::tempdir().unwrap();
    let (mut child, console) = bad_config_run(dir.path(), false);
    let status = wait_for(Duration::from_secs(30), || child.try_wait().ok().flatten());
    if status.is_none() {
        let _ = child.kill();
    }
    let status = status.expect("it should exit");
    assert!(!status.success(), "{status}");
    // The reader has the whole stderr once the process has ended.
    let shown = wait_for(Duration::from_secs(10), || {
        let c = console.lock().unwrap().clone();
        c.contains("config:").then_some(c)
    })
    .expect("the config error on stderr");
    assert!(!shown.contains("Press Enter"), "{shown}");
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
