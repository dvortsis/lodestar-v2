//! The companion launcher hook (CMP-4b, wired by CMP-5): `lodestar
//! __lodestar-companion-launcher` reaches `launcher_main()` before
//! anything parses arguments, reads config or starts logging. Without a
//! plan it fails with the launcher's own code (120, `launcher_bad_plan`),
//! not clap's usage error (2). Linux only: elsewhere the hook is absent.
#![cfg(target_os = "linux")]

use std::process::Command;

#[test]
fn the_sentinel_argument_reaches_the_launcher() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_lodestar"))
        .arg("__lodestar-companion-launcher")
        .env_remove("LODESTAR_COMPANION_PLAN")
        // A config that would fail to load, to show none is read.
        .env("LODESTAR_DATA_DIR", dir.path().join("missing"))
        .env("LODESTAR_HTTP_BIND", "not an address")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(120), "{stderr}");
    assert_eq!(
        stderr.trim(),
        "lodestar companion launcher: launcher_bad_plan"
    );
    assert!(
        !dir.path().join("missing").exists(),
        "the data folder was touched"
    );
}
