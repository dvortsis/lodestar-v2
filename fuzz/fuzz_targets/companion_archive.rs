//! Companion archive extraction (CMP-2, `lodestar_companions::extract`):
//! zip and tar.gz through the same entry point, into a temporary root.
//! The first byte picks the format (bit 0: zip, else tar.gz) and the caps
//! (bit 1: tight caps that the fuzzer can reach, else plan Q16's); the
//! rest is the archive. Nothing may panic; nothing is ever written
//! outside the staging folder; a failure leaves no staging folder; and a
//! success leaves only regular files and folders, the executable among
//! them.
#![no_main]

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use lodestar_companions::catalogue::{ArchiveKind, RelPath};
use lodestar_companions::extract::{Limits, extract_limited};

const EXE: &str = "app/bin/app";

fn base() -> &'static PathBuf {
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    BASE.get_or_init(|| {
        let p =
            std::env::temp_dir().join(format!("lodestar-fuzz-companion-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).expect("temp root");
        fs::canonicalize(&p).expect("temp root")
    })
}

/// Every path under `dir`; panics on a link.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).expect("readable") {
        let e = e.expect("entry");
        let ft = e.file_type().expect("type");
        assert!(
            ft.is_dir() || ft.is_file(),
            "not a file or folder: {:?}",
            e.path()
        );
        out.push(e.path());
        if ft.is_dir() {
            walk(&e.path(), out);
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&pick, archive)) = data.split_first() else {
        return;
    };
    let kind = if pick & 1 == 1 {
        ArchiveKind::Zip
    } else {
        ArchiveKind::TarGz
    };
    let limits = if pick & 2 == 2 {
        Limits {
            entries: 16,
            total: 64 * 1024,
            ratio: 100,
            ratio_floor: 1024,
            overhead: 64 * 1024,
        }
    } else {
        Limits::Q16
    };

    let work = base().join("w");
    let _ = fs::remove_dir_all(&work);
    let outside = work.join("outside");
    fs::create_dir_all(&outside).expect("work");
    fs::write(outside.join("keep"), b"k").expect("sentinel");
    let staging = work.join("root").join("1.0.staging");
    fs::create_dir(work.join("root")).expect("root");

    let exe = RelPath::parse(EXE).expect("valid");
    let r = extract_limited(limits, kind, &mut Cursor::new(archive), &exe, &staging);

    let mut all = Vec::new();
    walk(&work, &mut all);
    for p in &all {
        let allowed = p == &outside
            || p == &outside.join("keep")
            || p == &work.join("root")
            || (r.is_ok() && p.starts_with(&staging));
        assert!(allowed, "{p:?} written ({r:?})");
    }
    assert_eq!(fs::read(outside.join("keep")).expect("sentinel"), b"k");
    match &r {
        Ok(done) => {
            assert!(staging.join("app/bin/app").is_file());
            assert!(done.bytes <= limits.total && done.entries <= limits.entries);
        }
        Err(_) => assert!(!staging.exists(), "staging left: {r:?}"),
    }
    let _ = fs::remove_dir_all(&work);
});
