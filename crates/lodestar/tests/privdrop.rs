//! PUID/PGID on the real binary (src/privdrop.rs). These need root, as in
//! the container; run as anyone else they skip and say so.
#![cfg(target_os = "linux")]

use std::os::unix::fs::{MetadataExt, symlink};
use std::path::Path;
use std::process::{Command, Output, Stdio};
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

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn lodestar(data: &Path, port: u16) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_lodestar"));
    c.env_clear()
        .env("LODESTAR_DATA_DIR", data)
        .env("LODESTAR_HTTP_BIND", format!("127.0.0.1:{port}"))
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

/// Runs a command that should fail before serving.
fn refused(mut c: Command) -> String {
    let Output { status, stderr, .. } = c.stdout(Stdio::null()).output().unwrap();
    assert!(!status.success());
    String::from_utf8_lossy(&stderr).into_owned()
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
    std::fs::set_permissions(
        tmp.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();

    let port = free_port();
    let mut child = lodestar(&data, port)
        .env("PUID", "4242")
        .env("PGID", "4343")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let stderr = child.stderr.take().unwrap();
    let log = std::thread::spawn(move || std::io::read_to_string(stderr).unwrap_or_default());

    let start = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "server didn't come up"
        );
        assert!(child.try_wait().unwrap().is_none(), "lodestar exited");
        std::thread::sleep(Duration::from_millis(100));
    }

    // Every thread, the runtime's included, runs as 4242:4343 with no
    // groups or capabilities, so setuid(0) can't succeed.
    let tasks: Vec<_> = std::fs::read_dir(format!("/proc/{pid}/task"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert!(tasks.len() > 1, "expected the runtime's threads");
    for task in &tasks {
        let status = std::fs::read_to_string(task.join("status")).unwrap();
        assert_eq!(field(&status, "Uid"), "4242\t4242\t4242\t4242", "{task:?}");
        assert_eq!(field(&status, "Gid"), "4343\t4343\t4343\t4343", "{task:?}");
        assert_eq!(field(&status, "Groups"), "", "{task:?}");
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
    let check = lodestar(&data, port)
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
    assert!(log.contains("dropped root: running as 4242:4343"), "{log}");
    assert!(log.contains("was owned by 0:0"), "{log}");
}

#[test]
fn bad_ids_and_symlinked_data_dirs_stop_start_up() {
    if !root() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let port = free_port();
    for (puid, pgid, says) in [
        ("0", "100", "LODESTAR_ALLOW_ROOT=1"),
        ("99", "0", "LODESTAR_ALLOW_ROOT=1"),
        ("nobody", "100", "not a number"),
        ("-1", "100", "not a number"),
        ("4294967295", "100", "reserved"),
    ] {
        let mut c = lodestar(&data, port);
        c.env("PUID", puid).env("PGID", pgid);
        let e = refused(c);
        assert!(e.contains(says), "{puid}:{pgid}: {e}");
        // Nothing was created or changed before the refusal.
        assert!(!data.exists());
    }

    let real = tmp.path().join("real");
    std::fs::create_dir(&real).unwrap();
    symlink(&real, &data).unwrap();
    let e = refused(lodestar(&data, port));
    assert!(e.contains("symlink"), "{e}");
    assert_eq!(owner(&real), (0, 0));
}
