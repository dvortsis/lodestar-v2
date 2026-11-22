//! The fixture-server helper must not leave its child running when a test
//! panics between `spawn` and `stop`.
#![cfg(all(unix, feature = "fixture"))]
#![allow(dead_code)] // the helper modules serve other test binaries too

mod common;
mod fixture_server;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[test]
fn a_panicking_test_leaves_no_server_process() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("fx").to_string_lossy().into_owned();
    let mut pid = 0;
    let caught = catch_unwind(AssertUnwindSafe(|| {
        let server = fixture_server::spawn(&["--fixture", &dir], &[]);
        pid = server.pid();
        assert!(alive(pid), "the child should be running");
        panic!("a test failing before stop()");
    }));
    let payload = caught.unwrap_err();
    assert_eq!(
        payload.downcast_ref::<&str>(),
        Some(&"a test failing before stop()"),
        "the closure must fail only at the deliberate panic"
    );
    assert_ne!(pid, 0);
    let start = Instant::now();
    while alive(pid) {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "server {pid} outlived its panicking test"
        );
        std::thread::yield_now();
    }
}
