//! PUID/PGID on the real binary (src/privdrop.rs). These need root, as in
//! the container; run as anyone else they skip and say so.
#![cfg(target_os = "linux")]

use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn root() -> bool {
    if rustix::process::geteuid().is_root() {
        true
    } else {
        eprintln!(
            "skipping: the PUID/PGID drop only happens when started as root; \
             run `sudo -E cargo test -p lodestar --test privdrop` to cover it"
        );
        false
    }
}

mod common;

fn lodestar(data: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_lodestar"));
    c.env_clear()
        .env("LODESTAR_DATA_DIR", data)
        .env("LODESTAR_HTTP_BIND", common::ANY_PORT)
        .env("LODESTAR_DHT_PORT", "0")
        .env("LODESTAR_LOG", "info");
    c
}

fn owner(p: &Path) -> (u32, u32) {
    let m = std::fs::symlink_metadata(p).unwrap();
    (m.uid(), m.gid())
}

fn field<'a>(status: &'a str, key: &str) -> &'a str {
    status
        .lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
        .unwrap_or_else(|| panic!("no {key} in {status}"))
        .trim()
}

/// Runs a command that should fail before serving; one that starts
/// serving instead is killed, so the test fails rather than hangs.
fn refused(mut c: Command) -> String {
    let mut child = c
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let log = std::thread::spawn(move || std::io::read_to_string(stderr).unwrap_or_default());
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "expected a refusal, but it kept running: {}",
                log.join().unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(!status.success());
    log.join().unwrap()
}

#[test]
fn drops_to_puid_and_pgid_before_anything_runs() {
    if !root() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(data.join("index")).unwrap();
    std::fs::write(data.join("index/old"), b"").unwrap();
    let outside = tmp.path().join("outside");
    std::fs::write(&outside, b"").unwrap();
    symlink(&outside, data.join("link")).unwrap();
    // The data directory sits under the tempdir, which must be traversable.
    std::fs::set_permissions(tmp.path(), PermissionsExt::from_mode(0o755)).unwrap();

    let mut child = lodestar(&data)
        .env("PUID", "4242")
        .env("PGID", "4343")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let stderr = child.stderr.take().unwrap();
    let (port_tx, port_rx) = std::sync::mpsc::channel();
    let log = std::thread::spawn(move || {
        use std::io::BufRead;
        let mut all = String::new();
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            if let Some(port) = common::serving_port(&line) {
                let _ = port_tx.send(port);
            }
            all.push_str(&line);
            all.push('\n');
        }
        all
    });

    let start = Instant::now();
    let mut port = 0;
    while port == 0 || std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        if port == 0 {
            port = port_rx.try_recv().unwrap_or(0);
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "server didn't come up"
        );
        assert!(child.try_wait().unwrap().is_none(), "lodestar exited");
        std::thread::sleep(Duration::from_millis(100));
    }

    // Every thread, the runtime's included, runs as 4242:4343 with no
    // groups or capabilities, so setuid(0) can't succeed: the
    // supervisor's (one thread) and its child's, which does the work
    // (B-update, §12.2).
    let threads = |pid: &str| -> Vec<std::path::PathBuf> {
        std::fs::read_dir(format!("/proc/{pid}/task"))
            .map(|r| r.map(|e| e.unwrap().path()).collect())
            .unwrap_or_default()
    };
    let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap();
    let child_pid = children
        .split_whitespace()
        .next()
        .expect("the supervisor's child");
    let mut tasks = threads(&pid.to_string());
    let child_tasks = threads(child_pid);
    assert!(
        child_tasks.len() > 1,
        "expected the child's runtime threads"
    );
    tasks.extend(child_tasks);
    for task in &tasks {
        let status = std::fs::read_to_string(task.join("status")).unwrap();
        assert_eq!(field(&status, "Uid"), "4242\t4242\t4242\t4242", "{task:?}");
        assert_eq!(field(&status, "Gid"), "4343\t4343\t4343\t4343", "{task:?}");
        assert_eq!(field(&status, "Groups"), "", "{task:?}");
        assert_eq!(field(&status, "NoNewPrivs"), "1", "{task:?}");
        for cap in ["CapPrm", "CapEff", "CapAmb"] {
            assert!(
                field(&status, cap).bytes().all(|b| b == b'0'),
                "{task:?} {cap}"
            );
        }
    }

    // The data directory and what was in it are the new account's; the
    // file behind the symlink isn't.
    for p in ["", "index", "index/old", "link", "lodestar.db"] {
        assert_eq!(owner(&data.join(p)), (4242, 4343), "{p}");
    }
    assert_eq!(owner(&outside), (0, 0));

    // The health check drops too, and doesn't need to write anything.
    let check = lodestar(&data)
        .env("LODESTAR_HTTP_BIND", format!("127.0.0.1:{port}"))
        .env("PUID", "4242")
        .env("PGID", "4343")
        .arg("healthcheck")
        .status()
        .unwrap();
    assert!(check.success());

    assert!(
        Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    let status = child.wait().unwrap();
    let log = log.join().unwrap();
    assert!(status.success(), "{log}");
    assert!(
        log.contains("dropped root: running as the target account"),
        "{log}"
    );
    assert!(log.contains("uid=4242 gid=4343"), "{log}");
    assert!(
        log.contains("was owned by another account") && log.contains("from_uid=0 from_gid=0"),
        "{log}"
    );
}

#[test]
fn bad_ids_and_symlinked_data_dirs_stop_start_up() {
    if !root() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    for (puid, pgid, says) in [
        ("0", "100", "LODESTAR_ALLOW_ROOT=1"),
        ("99", "0", "LODESTAR_ALLOW_ROOT=1"),
        ("nobody", "100", "not a number"),
        ("-1", "100", "not a number"),
        ("4294967295", "100", "reserved"),
    ] {
        let mut c = lodestar(&data);
        c.env("PUID", puid).env("PGID", pgid);
        let e = refused(c);
        assert!(e.contains(says), "{puid}:{pgid}: {e}");
        // Nothing was created or changed before the refusal.
        assert!(!data.exists());
    }

    // A folder of someone else's that isn't Lodestar's is left alone.
    let foreign = tmp.path().join("foreign");
    std::fs::create_dir(&foreign).unwrap();
    std::fs::write(foreign.join("keep"), b"").unwrap();
    let e = refused(lodestar(&foreign));
    assert!(e.contains("doesn't look like Lodestar's"), "{e}");
    assert_eq!(owner(&foreign.join("keep")), (0, 0));

    // The new account can't get through a 0700 root-owned parent.
    let locked = tmp.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, PermissionsExt::from_mode(0o700)).unwrap();
    let e = refused(lodestar(&locked.join("data")));
    assert!(e.contains("a parent folder doesn't let it in"), "{e}");

    let real = tmp.path().join("real");
    std::fs::create_dir(&real).unwrap();
    symlink(&real, &data).unwrap();
    let e = refused(lodestar(&data));
    assert!(e.contains("symlink"), "{e}");
    assert_eq!(owner(&real), (0, 0));
}

/// Root without CAP_SETUID/CAP_SETGID (compose `cap_drop: ALL`): refused
/// before anything is changed, with what to do instead.
#[test]
fn root_without_setuid_stops_before_any_change() {
    if !root() {
        return;
    }
    if Command::new("setpriv").arg("--version").output().is_err() {
        eprintln!("skipping: needs setpriv (util-linux) to drop capabilities");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let mut c = Command::new("setpriv");
    c.args(["--bounding-set", "-setuid,-setgid", "--inh-caps", "-all"])
        .arg(env!("CARGO_BIN_EXE_lodestar"))
        .env_clear()
        .env("LODESTAR_DATA_DIR", &data)
        .env("LODESTAR_HTTP_BIND", common::ANY_PORT)
        .env("LODESTAR_DHT_PORT", "0");
    let e = refused(c);
    assert!(e.contains("without the SETUID/SETGID capabilities"), "{e}");
    assert!(e.contains("--user"), "{e}");
    assert_eq!(owner(&data), (0, 0));
}
