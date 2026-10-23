//! The real binary's `/system` and restart (B2c): signed in, `GET /system`
//! reports the build and installation with nothing secret in it, and
//! `POST /system/restart` answers, shuts down gracefully and comes back
//! under the same process id with its data intact and nothing left
//! waiting for a restart.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

const PASSWORD: &str = "correct horse battery";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

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

fn json_request(port: u16, method: &str, path: &str, cookie: &str, body: &str) -> String {
    request(
        port,
        &format!("{method} {path} HTTP/1.0\r\nCookie: {cookie}\r\nContent-Type: application/json"),
        body,
    )
    .unwrap_or_default()
}

fn body(response: &str) -> Value {
    let (_, body) = response.split_once("\r\n\r\n").expect("a body");
    serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {response}"))
}

fn healthy(port: u16) -> bool {
    request(port, "GET /healthz HTTP/1.0", "").is_some_and(|r| r.starts_with("HTTP/1.0 200"))
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(60), "{what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Every string in `v`, keys included.
fn strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|v| strings(v, out)),
        Value::Object(o) => {
            for (k, v) in o {
                out.push(k.clone());
                strings(v, out);
            }
        }
        _ => {}
    }
}

struct Server {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn system_reports_and_restarts_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lodestar"))
        .env("LODESTAR_DATA_DIR", dir.path())
        .env("LODESTAR_HTTP_BIND", format!("127.0.0.1:{port}"))
        .env("LODESTAR_DHT_ENABLED", "false")
        .env("LODESTAR_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, lines) = mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            let _ = tx.send(line);
        }
    });
    let mut server = Server { child, lines };
    let pid = server.child.id();
    wait_until("the server didn't come up", || healthy(port));

    let token = loop {
        let line = server
            .lines
            .recv_timeout(Duration::from_secs(10))
            .expect("setup token logged");
        if let Some(rest) = line.split("setup token ").nth(1) {
            break rest.split_whitespace().next().unwrap().to_string();
        }
    };
    let setup = request(
        port,
        "POST /api/v1/setup HTTP/1.0\r\nContent-Type: application/json",
        &format!(r#"{{"token":"{token}","password":"{PASSWORD}"}}"#),
    )
    .unwrap();
    assert!(setup.starts_with("HTTP/1.0 200"), "{setup}");
    let cookie = setup
        .lines()
        .find_map(|l| l.strip_prefix("set-cookie: "))
        .and_then(|c| c.split(';').next())
        .unwrap()
        .to_string();

    // Signed out: nothing.
    let anon = request(port, "GET /api/v1/system HTTP/1.0", "").unwrap();
    assert!(anon.starts_with("HTTP/1.0 401"), "{anon}");

    // A restart-only change waits for the restart.
    let saved = json_request(
        port,
        "PATCH",
        "/api/v1/settings/crawler",
        &cookie,
        r#"{"timeout_secs":17}"#,
    );
    assert!(saved.starts_with("HTTP/1.0 200"), "{saved}");

    let first = json_request(port, "GET", "/api/v1/system", &cookie, "");
    assert!(first.starts_with("HTTP/1.0 200"), "{first}");
    let info = body(&first);
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(info["repository"], env!("CARGO_PKG_REPOSITORY"));
    assert_eq!(info["restart"]["available"], true);
    assert_eq!(info["restart"]["method"], "reexec");
    assert!(info["restart"]["estimated_seconds"].as_u64().unwrap() >= 1);
    assert_eq!(
        info["restart_required"],
        serde_json::json!(["crawler.timeout_secs"])
    );
    assert!(info["data_formats"]["store_schema"].as_i64().unwrap() > 0);
    assert!(
        info["data_formats"]["index"]
            .as_str()
            .unwrap()
            .starts_with('v')
    );
    assert_eq!(info["data"]["dir"], dir.path().display().to_string());
    assert!(info["data"]["database_bytes"].as_u64().unwrap() > 0);
    // No cold store yet: unknown, not 0. The log file is on by default
    // and already holds the start-up lines (B-log1).
    assert_eq!(info["data"]["cold_bytes"], Value::Null);
    assert!(info["data"]["logs_bytes"].as_u64().unwrap() > 0);
    assert_eq!(info["address_change"], Value::Null);
    assert_eq!(info["connections"][0]["kind"], "dht");
    assert_eq!(info["connections"][0]["state"], "off");
    assert_eq!(
        info["build"]["target"].as_str().map(str::is_empty),
        Some(false)
    );
    // Settings by name and source only.
    let changed = &info["diagnostics"]["changed"];
    assert!(
        changed
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "crawler.timeout_secs" && c["source"] == "db"),
        "{changed}"
    );
    // Nothing secret anywhere in it.
    let mut all = Vec::new();
    strings(&info, &mut all);
    let cookie_value = cookie.split_once('=').unwrap().1;
    for secret in [PASSWORD, token.as_str(), cookie_value] {
        assert!(
            !all.iter().any(|s| s.contains(secret)),
            "{secret:?} in /system: {first}"
        );
    }
    let auth_file = std::fs::read_to_string(dir.path().join("auth.json")).unwrap();
    let raw = first.split_once("\r\n\r\n").unwrap().1;
    for value in serde_json::from_str::<Value>(&auth_file)
        .map(|v| {
            let mut s = Vec::new();
            strings(&v, &mut s);
            s
        })
        .unwrap_or_default()
    {
        if value.len() >= 16 {
            assert!(!raw.contains(&value), "auth.json's {value:?} in /system");
        }
    }
    let started_at = info["started_at"].as_u64().unwrap();

    // Cross-origin and non-JSON restarts are refused.
    let not_json = request(
        port,
        &format!("POST /api/v1/system/restart HTTP/1.0\r\nCookie: {cookie}"),
        "",
    )
    .unwrap();
    assert!(not_json.starts_with("HTTP/1.0 415"), "{not_json}");

    let restart = json_request(port, "POST", "/api/v1/system/restart", &cookie, "");
    assert!(restart.starts_with("HTTP/1.0 202"), "{restart}");
    assert_eq!(body(&restart)["restarting"], true);
    // It goes away, then comes back as the same process.
    wait_until("it never went away", || !healthy(port));
    wait_until("it didn't come back", || healthy(port));
    assert!(
        server.child.try_wait().unwrap().is_none(),
        "the process exited instead of re-executing"
    );
    assert_eq!(server.child.id(), pid);

    // The session may not survive; the password does.
    let login = request(
        port,
        "POST /api/v1/login HTTP/1.0\r\nContent-Type: application/json",
        &format!(r#"{{"password":"{PASSWORD}"}}"#),
    )
    .unwrap();
    assert!(login.starts_with("HTTP/1.0 200"), "{login}");
    let cookie = login
        .lines()
        .find_map(|l| l.strip_prefix("set-cookie: "))
        .and_then(|c| c.split(';').next())
        .unwrap()
        .to_string();
    let after = body(&json_request(port, "GET", "/api/v1/system", &cookie, ""));
    assert_eq!(after["restart_required"], serde_json::json!([]));
    assert!(after["started_at"].as_u64().unwrap() >= started_at);
    assert_eq!(after["data_formats"], info["data_formats"]);
    let crawler = body(&json_request(
        port,
        "GET",
        "/api/v1/settings/crawler",
        &cookie,
        "",
    ));
    assert_eq!(crawler["fields"]["timeout_secs"]["value"], 17);

    // The shutdown before the restart was the graceful one.
    let mut log = String::new();
    while let Ok(line) = server.lines.try_recv() {
        log.push_str(&line);
        log.push('\n');
    }
    assert!(log.contains("restart requested"), "{log}");
    assert!(log.contains("stopped cleanly"), "{log}");

    // A signal still stops it for good.
    let killed = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let exit = server.child.wait().unwrap();
    assert!(exit.success(), "exit {exit:?}");
}
