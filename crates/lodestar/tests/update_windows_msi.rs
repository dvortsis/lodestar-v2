//! `lodestar update-helper` from the real binary (PK-win-upd-2): it reads
//! no config, and installs nothing unless it's the installed program,
//! `%ProgramFiles%\Lodestar\lodestar.exe`. A test build runs from
//! `target\`, so it's refused before it looks at any data folder. The
//! helper's steps (copy then verify, a swapped file, a replayed manifest,
//! the staging folder's ACL, `.lock`) are tested in
//! `update::windows::helper` and `update::windows::msi`, on this job too.
#![cfg(windows)]

use std::process::Command;

fn helper(args: &[&str]) -> (Option<i32>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_lodestar"))
        .args(args)
        // Never consulted: the helper always uses the default data folder.
        .env("LODESTAR_DATA_DIR", r"C:\nowhere-lodestar-test")
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn outside_program_files_it_refuses() {
    let (code, err) = helper(&["update-helper"]);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("not_installed"), "{err}");
}

#[test]
fn registering_the_task_needs_the_installed_program() {
    let (code, err) = helper(&["update-helper", "--register-task"]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("not_installed"), "{err}");
}

/// The copy's own entry (`--run-msi`) is refused outside staging, before
/// it waits for anything or looks at a journal.
#[test]
fn the_copy_runs_only_from_staging() {
    let (code, err) = helper(&["update-helper", "--run-msi"]);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("not_installed"), "{err}");
}

#[test]
fn it_takes_no_config() {
    let (code, err) = helper(&["--config", r"C:\x\config.toml", "update-helper"]);
    assert_eq!(code, Some(2), "{err}");
}

#[test]
fn it_isnt_listed_in_help() {
    let out = Command::new(env!("CARGO_BIN_EXE_lodestar"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&out.stdout).contains("update-helper"));
}
