//! The portable build's update steps in the real binary on Windows
//! (PK-win-portable-upd): a copy of `lodestar.exe` in a folder laid out as
//! the zip unpacks it (`Start Lodestar.cmd` beside it, data in `data`).
//!
//! - A rollback left half done is finished at the next start, **before
//!   anything opens the store**: the store the failed version migrated is
//!   replaced by its snapshot, the version is marked `failed`, and the
//!   journal goes. Both the store and the snapshot are unreadable, so the
//!   run stops at the store's open: had it opened the store first, the
//!   snapshot wouldn't be in place.
//! - A start with no `<data>\bin` creates it with a protected DACL.
//!
//! The run ends by itself (the store can't be opened); no test waits on a
//! timer. The test binary's version is the crate's (`LODESTAR_VERSION`
//! unset in tests).
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::Digest;

/// The portable folder: `app\lodestar.exe`, the launcher, `app\data`.
fn portable() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let app = tmp.path().join("app");
    std::fs::create_dir_all(app.join("data")).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_lodestar"), app.join("lodestar.exe")).unwrap();
    std::fs::write(app.join("Start Lodestar.cmd"), "@echo off\r\n").unwrap();
    (tmp, app)
}

/// `lodestar.exe serve` as the launcher starts it, until it exits.
fn serve(app: &Path) -> (std::process::ExitStatus, String) {
    let out = Command::new(app.join("lodestar.exe"))
        .arg("serve")
        .env("LODESTAR_DATA_DIR", app.join("data"))
        .env_remove("LODESTAR_RELAUNCH_TOKEN")
        .env_remove("LODESTAR_PORTABLE_WINDOW")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    (
        out.status,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn sha256(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap();
    sha2::Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn the_old_version_finishes_a_rollback_before_it_opens_the_store() {
    let (_tmp, app) = portable();
    let data = app.join("data");
    let bin = data.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // The failed version migrated: its snapshot, and the store it left.
    std::fs::write(data.join("lodestar.db"), b"the failed version's store").unwrap();
    std::fs::write(data.join("lodestar.pre-v999.db"), b"the snapshot").unwrap();
    std::fs::write(bin.join("failed.exe"), b"the failed program").unwrap();
    let me = env!("CARGO_PKG_VERSION");
    let journal = serde_json::json!({
        "format": 1,
        "from": me,
        "to": "99.0.0",
        "old_sha256": sha256(&app.join("lodestar.exe")),
        "new_sha256": sha256(&bin.join("failed.exe")),
        "snapshots_before": [],
        "attempts": 1,
        "state": "rolling_back",
    });
    std::fs::write(bin.join("portable.json"), journal.to_string()).unwrap();

    let (status, stderr) = serve(&app);
    assert!(
        !status.success(),
        "an unreadable store can't serve: {stderr}"
    );
    assert_eq!(
        std::fs::read(data.join("lodestar.db")).unwrap(),
        b"the snapshot",
        "{stderr}"
    );
    assert!(!data.join("lodestar.pre-v999.db").exists());
    assert!(!bin.join("portable.json").exists(), "{stderr}");
    assert!(!bin.join("failed.exe").exists());
    let skipped = std::fs::read_to_string(bin.join("skipped")).unwrap();
    assert!(
        skipped.contains("\"99.0.0\"") && skipped.contains("\"failed\""),
        "{skipped}"
    );
    // The program in place is the old one, untouched.
    assert_eq!(
        sha256(&app.join("lodestar.exe")),
        journal["old_sha256"].as_str().unwrap()
    );
}

#[test]
fn a_new_update_folder_is_created_private() {
    let (_tmp, app) = portable();
    let data = app.join("data");
    std::fs::write(data.join("lodestar.db"), b"not a store").unwrap();
    let (_, stderr) = serve(&app);
    let bin = data.join("bin");
    assert!(bin.is_dir(), "{stderr}");
    let root = std::env::var_os("SystemRoot").unwrap();
    let out =
        Command::new(Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe"))
            .env_remove("PSModulePath")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "(Get-Acl -LiteralPath $env:P).Sddl",
            ])
            .env("P", &bin)
            .output()
            .unwrap();
    let sddl = String::from_utf8_lossy(&out.stdout);
    // Protected (nothing inherited), and no Users, Authenticated Users or
    // Everyone.
    assert!(sddl.contains("D:P"), "{sddl}");
    for shared in [";BU)", ";AU)", ";WD)", "S-1-5-32-545", "S-1-5-11)"] {
        assert!(!sddl.contains(shared), "{shared} in {sddl}");
    }
}

/// What the update relies on: Windows lets the running program be
/// renamed aside into `data\bin`, and a new file take its name, while it
/// runs. The copy here waits for a relaunch token on stdin, so it stays
/// up until the pipe closes; nothing waits on a timer.
#[test]
fn the_running_program_can_be_renamed_aside_and_back() {
    let (_tmp, app) = portable();
    let bin = app.join("data").join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let program = app.join("lodestar.exe");
    let mut child = Command::new(&program)
        .arg("serve")
        .env("LODESTAR_DATA_DIR", app.join("data"))
        .env("LODESTAR_RELAUNCH_TOKEN", "test-token")
        .env_remove("LODESTAR_PORTABLE_WINDOW")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Still running (waiting for the token) when the renames happen.
    assert!(child.try_wait().unwrap().is_none());
    std::fs::rename(&program, bin.join("previous.exe")).unwrap();
    std::fs::write(&program, b"the new program").unwrap();
    assert!(child.try_wait().unwrap().is_none());
    // And back, as a rollback does.
    std::fs::rename(&program, bin.join("failed.exe")).unwrap();
    std::fs::rename(bin.join("previous.exe"), &program).unwrap();
    // Closing the pipe without the token ends it.
    drop(child.stdin.take());
    let status = child.wait().unwrap();
    assert!(status.success(), "{status:?}");
    assert!(program.is_file());
}
