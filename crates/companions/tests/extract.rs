//! CMP-2: crafted archives for each refusal, in both formats where the
//! format can carry it; a property test over entry names; and the install
//! layout's steps, including a crash between each.

use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lodestar_companions::catalogue::{ArchiveKind, RelPath};
use lodestar_companions::extract::{
    self, ExtractError, Limits, MAX_ENTRIES, Refusal, extract_limited,
};
use lodestar_companions::layout::{Install, LayoutError, POINTER, Pointer};
use lodestar_companions::{CompanionId, Version};
use proptest::prelude::*;
use tar::{EntryType, Header};
use zip::write::SimpleFileOptions;

const EXE: &str = "app/bin/app";

// ─── Helpers ────────────────────────────────────────────────────────────

/// A temporary folder, removed when dropped.
struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "lodestar-cmp2-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(fs::canonicalize(&p).unwrap())
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn exe() -> RelPath {
    RelPath::parse(EXE).unwrap()
}

/// What an entry is.
enum E<'a> {
    File(&'a [u8]),
    Dir,
}

/// A zip of `entries`, deflated.
fn zip_of(entries: &[(&str, E<'_>)]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let o = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    for (name, e) in entries {
        match e {
            E::File(data) => {
                w.start_file(*name, o).unwrap();
                w.write_all(data).unwrap();
            }
            E::Dir => w.add_directory(*name, o).unwrap(),
        }
    }
    w.finish().unwrap().into_inner()
}

/// A zip with `name` stored (not deflated), for patching sizes.
fn stored_zip(name: &str, data: &[u8]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let o = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o644);
    w.start_file(name, o).unwrap();
    w.write_all(data).unwrap();
    w.finish().unwrap().into_inner()
}

/// Offsets of the central-directory record and local header for `name`.
fn zip_records(zip: &[u8], name: &str) -> (usize, usize) {
    let find = |sig: &[u8; 4], name_at: usize| {
        (0..zip.len() - 4)
            .find(|&i| {
                &zip[i..i + 4] == sig
                    && zip.get(i + name_at..i + name_at + name.len()) == Some(name.as_bytes())
            })
            .unwrap()
    };
    (find(b"PK\x01\x02", 46), find(b"PK\x03\x04", 30))
}

/// Sets `name`'s Unix mode (file type and permission bits) in the central
/// directory, which the zip writer masks to 0o777.
fn patch_mode(mut zip: Vec<u8>, name: &str, mode: u32) -> Vec<u8> {
    let (cd, _) = zip_records(&zip, name);
    zip[cd + 5] = 3; // made by: Unix
    zip[cd + 38..cd + 42].copy_from_slice(&(mode << 16).to_le_bytes());
    zip
}

/// Sets `name`'s claimed uncompressed size, in both headers.
fn patch_size(mut zip: Vec<u8>, name: &str, size: u32) -> Vec<u8> {
    let (cd, local) = zip_records(&zip, name);
    zip[cd + 24..cd + 28].copy_from_slice(&size.to_le_bytes());
    zip[local + 22..local + 26].copy_from_slice(&size.to_le_bytes());
    zip
}

/// One raw tar entry: a GNU header with `name` written byte for byte (the
/// tar writer refuses the names these tests need), then the data.
fn tar_entry(name: &[u8], ty: EntryType, mode: u32, data: &[u8]) -> Vec<u8> {
    let mut h = Header::new_gnu();
    h.as_mut_bytes()[..name.len()].copy_from_slice(name);
    h.set_entry_type(ty);
    h.set_mode(mode);
    h.set_size(data.len() as u64);
    if matches!(ty, EntryType::Symlink | EntryType::Link) {
        h.as_mut_bytes()[157..163].copy_from_slice(b"/etc/x");
    }
    h.set_cksum();
    let mut out = h.as_bytes().to_vec();
    out.extend_from_slice(data);
    out.resize(out.len().div_ceil(512) * 512, 0);
    out
}

fn tgz(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut tar: Vec<u8> = parts.concat();
    tar.extend_from_slice(&[0; 1024]);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&tar).unwrap();
    gz.finish().unwrap()
}

fn tfile(name: &str, data: &[u8]) -> Vec<u8> {
    tar_entry(name.as_bytes(), EntryType::Regular, 0o644, data)
}

fn tdir(name: &str) -> Vec<u8> {
    tar_entry(name.as_bytes(), EntryType::Directory, 0o755, b"")
}

/// The executable, so a test reaches the rule it's about.
fn exe_tar() -> Vec<u8> {
    tfile(EXE, b"#!")
}

/// Runs an extraction into `<tmp>/s.staging` and checks the invariant:
/// on failure the staging folder is gone and nothing else was written.
fn run_with(
    limits: Limits,
    kind: ArchiveKind,
    bytes: &[u8],
) -> (Result<extract::Extracted, ExtractError>, Tmp) {
    let tmp = Tmp::new();
    let staging = tmp.0.join("s.staging");
    let r = extract_limited(limits, kind, &mut Cursor::new(bytes), &exe(), &staging);
    if r.is_err() {
        assert!(!staging.exists(), "staging left behind: {r:?}");
        assert_eq!(fs::read_dir(&tmp.0).unwrap().count(), 0, "{r:?}");
    }
    (r, tmp)
}

fn run(kind: ArchiveKind, bytes: &[u8]) -> (Result<extract::Extracted, ExtractError>, Tmp) {
    run_with(Limits::Q16, kind, bytes)
}

fn refusal(kind: ArchiveKind, bytes: &[u8]) -> Option<Refusal> {
    let (r, _tmp) = run(kind, bytes);
    r.expect_err("should be refused").refusal()
}

fn zip_refusal(name: &str) -> Option<Refusal> {
    refusal(
        ArchiveKind::Zip,
        &zip_of(&[(EXE, E::File(b"#!")), (name, E::File(b"x"))]),
    )
}

fn tar_refusal(name: &[u8]) -> Option<Refusal> {
    refusal(
        ArchiveKind::TarGz,
        &tgz(&[exe_tar(), tar_entry(name, EntryType::Regular, 0o644, b"x")]),
    )
}

// ─── What's accepted ────────────────────────────────────────────────────

#[test]
fn both_formats_extract_with_the_modes_the_plan_sets() {
    let z = zip_of(&[
        ("app/", E::Dir),
        (EXE, E::File(b"#!/bin/sh\n")),
        ("app/lib/a.dll", E::File(b"aaaa")),
        ("app/README", E::File(b"")),
    ]);
    let t = tgz(&[
        tdir("./"),
        tdir("./app/"),
        tfile("./app/bin/app", b"#!/bin/sh\n"),
        tfile("./app/lib/a.dll", b"aaaa"),
        tfile("./app/README", b""),
    ]);
    for (kind, bytes) in [(ArchiveKind::Zip, z), (ArchiveKind::TarGz, t)] {
        let (r, tmp) = run(kind, &bytes);
        let done = r.unwrap();
        assert_eq!((done.files, done.bytes), (3, 14), "{kind:?}");
        let s = tmp.0.join("s.staging");
        assert_eq!(fs::read(s.join("app/bin/app")).unwrap(), b"#!/bin/sh\n");
        assert_eq!(fs::read(s.join("app/lib/a.dll")).unwrap(), b"aaaa");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &str| fs::metadata(s.join(p)).unwrap().permissions().mode() & 0o7777;
            assert_eq!(mode("app/bin/app"), 0o755);
            assert_eq!(mode("app/lib/a.dll"), 0o644);
            assert_eq!(mode("app/README"), 0o644);
            assert_eq!(mode("app/lib"), 0o755);
            assert_eq!(mode("app"), 0o755);
            assert_eq!(mode(""), 0o755);
        }
    }
}

#[test]
fn a_directory_listed_after_its_files_is_fine() {
    let z = zip_of(&[(EXE, E::File(b"#!")), ("app/bin/", E::Dir)]);
    let t = tgz(&[exe_tar(), tdir("app/bin/")]);
    assert!(run(ArchiveKind::Zip, &z).0.is_ok());
    assert!(run(ArchiveKind::TarGz, &t).0.is_ok());
}

#[test]
fn only_archive_kinds_extract() {
    let (r, _tmp) = run(ArchiveKind::Binary, b"\x7fELF");
    assert!(matches!(r, Err(ExtractError::NotAnArchive(_))));
}

// ─── Names ──────────────────────────────────────────────────────────────

#[test]
fn zip_slip_and_tar_dot_dot_are_refused() {
    for name in ["../evil", "app/../../evil", "app/.."] {
        assert_eq!(zip_refusal(name), Some(Refusal::Traversal), "{name}");
        assert_eq!(
            tar_refusal(name.as_bytes()),
            Some(Refusal::Traversal),
            "{name}"
        );
    }
}

#[test]
fn absolute_names_and_drive_letters_are_refused() {
    for name in ["/etc/evil", "C:/evil", "c:evil", "//server/share/evil"] {
        assert_eq!(zip_refusal(name), Some(Refusal::Absolute), "{name}");
        assert_eq!(
            tar_refusal(name.as_bytes()),
            Some(Refusal::Absolute),
            "{name}"
        );
    }
    for name in ["..\\evil", "app\\evil"] {
        assert_eq!(zip_refusal(name), Some(Refusal::Backslash), "{name}");
        assert_eq!(
            tar_refusal(name.as_bytes()),
            Some(Refusal::Backslash),
            "{name}"
        );
    }
}

#[test]
fn names_windows_cannot_hold_are_refused() {
    let cases = [
        ("app/a:stream", Refusal::Colon),
        ("app/CON", Refusal::ReservedName),
        ("app/nul.txt", Refusal::ReservedName),
        ("app/Com1.dll", Refusal::ReservedName),
        ("app/x.", Refusal::TrailingDotOrSpace),
        ("app /x", Refusal::TrailingDotOrSpace),
        ("app//x", Refusal::EmptyComponent),
        ("app/./x", Refusal::DotComponent),
        ("app/a*b", Refusal::Character),
    ];
    for (name, want) in cases {
        assert_eq!(zip_refusal(name), Some(want), "{name}");
        assert_eq!(tar_refusal(name.as_bytes()), Some(want), "{name}");
    }
    assert_eq!(tar_refusal(b"app/a\x01b"), Some(Refusal::Character));
    assert_eq!(tar_refusal(b"app/caf\xc3\xa9"), Some(Refusal::Character));
}

#[test]
fn an_error_shows_a_bounded_sanitised_name() {
    let (r, _tmp) = run(
        ArchiveKind::TarGz,
        &tgz(&[tar_entry(b"../\x1b[2Jevil", EntryType::Regular, 0o644, b"")]),
    );
    match r {
        Err(ExtractError::Refused {
            refusal: Refusal::Character,
            entry: Some(e),
        }) => assert_eq!(e.as_str(), "../?[2Jevil"),
        other => panic!("{other:?}"),
    }
}

// ─── Kinds and modes ────────────────────────────────────────────────────

#[test]
fn symlinks_and_hard_links_are_refused() {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    w.add_symlink("app/link", "/etc/passwd", SimpleFileOptions::default())
        .unwrap();
    let z = w.finish().unwrap().into_inner();
    assert_eq!(refusal(ArchiveKind::Zip, &z), Some(Refusal::Link));
    for ty in [EntryType::Symlink, EntryType::Link] {
        let t = tgz(&[exe_tar(), tar_entry(b"app/link", ty, 0o777, b"")]);
        assert_eq!(
            refusal(ArchiveKind::TarGz, &t),
            Some(Refusal::Link),
            "{ty:?}"
        );
    }
}

#[test]
fn devices_fifos_and_sockets_are_refused() {
    for ty in [
        EntryType::Char,
        EntryType::Block,
        EntryType::Fifo,
        EntryType::Continuous,
        EntryType::new(b'V'),
    ] {
        let t = tgz(&[exe_tar(), tar_entry(b"app/dev", ty, 0o644, b"")]);
        assert_eq!(
            refusal(ArchiveKind::TarGz, &t),
            Some(Refusal::Special),
            "{ty:?}"
        );
    }
    for mode in [0o020_644, 0o060_644, 0o010_644, 0o140_644] {
        let z = patch_mode(
            zip_of(&[(EXE, E::File(b"#!")), ("app/dev", E::File(b""))]),
            "app/dev",
            mode,
        );
        assert_eq!(
            refusal(ArchiveKind::Zip, &z),
            Some(Refusal::Special),
            "{mode:o}"
        );
    }
}

#[test]
fn setuid_setgid_and_sticky_bits_are_refused() {
    for bits in [0o4000, 0o2000, 0o1000] {
        let t = tgz(&[tar_entry(
            EXE.as_bytes(),
            EntryType::Regular,
            0o755 | bits,
            b"#!",
        )]);
        assert_eq!(refusal(ArchiveKind::TarGz, &t), Some(Refusal::ModeBits));
        let d = tgz(&[
            exe_tar(),
            tar_entry(b"app/d/", EntryType::Directory, 0o755 | bits, b""),
        ]);
        assert_eq!(refusal(ArchiveKind::TarGz, &d), Some(Refusal::ModeBits));
        let z = patch_mode(zip_of(&[(EXE, E::File(b"#!"))]), EXE, 0o100_755 | bits);
        assert_eq!(refusal(ArchiveKind::Zip, &z), Some(Refusal::ModeBits));
    }
}

// ─── Duplicates ─────────────────────────────────────────────────────────

#[test]
fn names_that_collide_after_case_folding_are_refused() {
    let z = zip_of(&[
        (EXE, E::File(b"#!")),
        ("app/Readme", E::File(b"1")),
        ("app/README", E::File(b"2")),
    ]);
    assert_eq!(refusal(ArchiveKind::Zip, &z), Some(Refusal::Duplicate));
    let z = zip_of(&[(EXE, E::File(b"#!")), ("APP/x", E::File(b"1"))]);
    assert!(
        run(ArchiveKind::Zip, &z).0.is_ok(),
        "a folded parent is the same folder"
    );
    let t = tgz(&[
        exe_tar(),
        tfile("app/Readme", b"1"),
        tfile("app/README", b"2"),
    ]);
    assert_eq!(refusal(ArchiveKind::TarGz, &t), Some(Refusal::Duplicate));
    let t = tgz(&[exe_tar(), tfile("app/x", b"1"), tfile("./app/x", b"2")]);
    assert_eq!(refusal(ArchiveKind::TarGz, &t), Some(Refusal::Duplicate));
    let t = tgz(&[exe_tar(), tdir("app/d/"), tdir("app/D/")]);
    assert_eq!(refusal(ArchiveKind::TarGz, &t), Some(Refusal::Duplicate));
    let t = tgz(&[exe_tar(), tfile("app/d", b""), tdir("app/d/")]);
    assert_eq!(refusal(ArchiveKind::TarGz, &t), Some(Refusal::Duplicate));
}

#[test]
fn a_file_used_as_a_folder_is_refused() {
    let t = tgz(&[exe_tar(), tfile("app/f", b""), tfile("app/f/g", b"")]);
    assert_eq!(
        refusal(ArchiveKind::TarGz, &t),
        Some(Refusal::NotADirectory)
    );
    let z = zip_of(&[
        (EXE, E::File(b"#!")),
        ("app/f", E::File(b"")),
        ("app/F/g", E::File(b"")),
    ]);
    assert_eq!(refusal(ArchiveKind::Zip, &z), Some(Refusal::NotADirectory));
}

// ─── Caps ───────────────────────────────────────────────────────────────

#[test]
fn the_entry_cap_is_q16s() {
    assert_eq!(MAX_ENTRIES, 20_000);
    // Refused from the index alone, before anything is written.
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let o = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for i in 0..=MAX_ENTRIES {
        w.start_file(format!("f{i}"), o).unwrap();
    }
    let z = w.finish().unwrap().into_inner();
    assert_eq!(refusal(ArchiveKind::Zip, &z), Some(Refusal::TooManyEntries));
}

#[test]
fn one_entry_past_the_cap_is_refused() {
    let limits = Limits {
        entries: 6,
        ..Limits::Q16
    };
    let mut parts = vec![exe_tar()];
    let mut entries = vec![(EXE.to_owned(), b"#!".to_vec())];
    for i in 0..3 {
        parts.push(tfile(&format!("app/f{i}"), b""));
        entries.push((format!("app/f{i}"), Vec::new()));
    }
    // exe + 3 files + the two folders the names create (`app`, `app/bin`)
    // = 6: directories nobody listed count too.
    assert!(run_with(limits, ArchiveKind::TarGz, &tgz(&parts)).0.is_ok());
    parts.push(tfile("app/f3", b""));
    let r = run_with(limits, ArchiveKind::TarGz, &tgz(&parts)).0;
    assert_eq!(r.unwrap_err().refusal(), Some(Refusal::TooManyEntries));

    let zip = |n: usize| {
        let v: Vec<(&str, E<'_>)> = entries[..n]
            .iter()
            .map(|(name, d)| (name.as_str(), E::File(d)))
            .collect();
        zip_of(&v)
    };
    assert!(run_with(limits, ArchiveKind::Zip, &zip(4)).0.is_ok());
    let mut z = entries.clone();
    z.push(("app/f9".into(), Vec::new()));
    let v: Vec<(&str, E<'_>)> = z.iter().map(|(n, d)| (n.as_str(), E::File(d))).collect();
    let r = run_with(limits, ArchiveKind::Zip, &zip_of(&v)).0;
    assert_eq!(r.unwrap_err().refusal(), Some(Refusal::TooManyEntries));
}

#[test]
fn the_size_cap_counts_bytes_written_not_headers() {
    let data = vec![7u8; 2000];
    let at = |total| Limits {
        total,
        ..Limits::Q16
    };
    // A header that claims more than the cap isn't refused for the
    // claim: the 2,000 bytes really there pass a 3,000-byte cap, and the
    // lie is what refuses it.
    for z in [
        patch_size(stored_zip(EXE, &data), EXE, 4000),
        patch_size(zip_of(&[(EXE, E::File(&data))]), EXE, 4000),
    ] {
        let r = run_with(at(3000), ArchiveKind::Zip, &z).0;
        assert_eq!(r.unwrap_err().refusal(), Some(Refusal::SizeMismatch));
        // Under a cap below the real size, the first byte past it stops it.
        let r = run_with(at(1999), ArchiveKind::Zip, &z).0;
        assert_eq!(r.unwrap_err().refusal(), Some(Refusal::TooLarge));
    }
    // A header that claims less: the zip reader stops at the claim, and
    // the checksum then fails. Refused, and nothing is left.
    let z = patch_size(zip_of(&[(EXE, E::File(&data))]), EXE, 10);
    let r = run_with(at(5000), ArchiveKind::Zip, &z).0;
    assert!(matches!(r, Err(ExtractError::Corrupt)), "{r:?}");

    // Exactly the cap passes; one byte past refuses.
    let z = stored_zip(EXE, &data);
    let t = tgz(&[tfile(EXE, &data)]);
    for (kind, bytes) in [(ArchiveKind::Zip, &z), (ArchiveKind::TarGz, &t)] {
        assert!(run_with(at(2000), kind, bytes).0.is_ok(), "{kind:?}");
        let r = run_with(at(1999), kind, bytes).0;
        assert_eq!(
            r.unwrap_err().refusal(),
            Some(Refusal::TooLarge),
            "{kind:?}"
        );
    }
}

/// A gzip of one raw tar header claiming `size` bytes of type `ty`,
/// followed by `size` zeros, streamed so the test never holds them.
fn header_bomb(ty: u8, size: u64) -> Vec<u8> {
    let mut h = Header::new_gnu();
    h.as_mut_bytes()[..13].copy_from_slice(b"././@LongLink");
    h.set_entry_type(EntryType::new(ty));
    h.set_mode(0o644);
    h.set_size(size);
    h.set_cksum();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(h.as_bytes()).unwrap();
    let zeros = vec![0u8; 1 << 20];
    let mut left = size;
    while left > 0 {
        let n = left.min(zeros.len() as u64) as usize;
        gz.write_all(&zeros[..n]).unwrap();
        left -= n as u64;
    }
    gz.finish().unwrap()
}

#[test]
fn a_long_name_or_pax_header_cannot_inflate_past_the_caps() {
    // Each header's body is read by the tar parser itself, before any
    // entry reaches the writer: GNU long names (`L`, `K`), pax records
    // (`x`) and a pax global header (`g`), which is skipped.
    let limits = Limits {
        total: 1024 * 1024,
        overhead: 1024 * 1024,
        ..Limits::Q16
    };
    for ty in *b"LKxg" {
        let bomb = header_bomb(ty, 64 * 1024 * 1024);
        let r = run_with(limits, ArchiveKind::TarGz, &bomb).0;
        let refusal = r.unwrap_err().refusal();
        assert!(
            matches!(refusal, Some(Refusal::Ratio | Refusal::TooLarge)),
            "{}: {refusal:?}",
            ty as char
        );
    }
    // Q16's own caps: a 256 MiB long name of zeros is stopped by the
    // ratio after a few megabytes.
    let r = run(ArchiveKind::TarGz, &header_bomb(b'L', 256 * 1024 * 1024)).0;
    assert_eq!(r.unwrap_err().refusal(), Some(Refusal::Ratio));
}

/// A tgz of `prefix` (raw tar bytes), then one raw `ty` header claiming
/// `size` bytes, its body `chunk` repeated, streamed so the test never
/// holds it.
fn streamed_header(prefix: &[u8], ty: u8, size: u64, chunk: &[u8]) -> Vec<u8> {
    let mut h = Header::new_gnu();
    h.as_mut_bytes()[..13].copy_from_slice(b"././@LongLink");
    h.set_entry_type(EntryType::new(ty));
    h.set_mode(0o644);
    h.set_size(size);
    h.set_cksum();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(prefix).unwrap();
    gz.write_all(h.as_bytes()).unwrap();
    let mut left = size;
    while left > 0 {
        let n = left.min(chunk.len() as u64) as usize;
        gz.write_all(&chunk[..n]).unwrap();
        left -= n as u64;
    }
    gz.finish().unwrap()
}

/// `n` bytes from a fixed LCG: `every`th byte pseudo-random, the rest
/// zero (`every` = 1 is incompressible).
fn noise(n: usize, every: usize) -> Vec<u8> {
    let mut x = 0x2545_f491_u32;
    (0..n)
        .map(|i| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            if i % every == 0 { (x >> 24) as u8 } else { 0 }
        })
        .collect()
}

#[test]
fn header_bodies_are_capped_on_their_own_not_by_the_total() {
    // The tar parser holds a long name or pax record whole in memory, so
    // the overhead cap applies to them by itself, whatever the 2 GiB
    // total and the ratio would still allow.
    let limits = Limits {
        total: 256 * 1024 * 1024,
        overhead: 256 * 1024,
        ..Limits::Q16
    };
    // A long-name body that compresses at well under 100:1, so the ratio
    // never trips.
    let body = noise(1 << 20, 40);
    let r = run_with(
        limits,
        ArchiveKind::TarGz,
        &streamed_header(b"", b'L', 8 << 20, &body),
    )
    .0;
    assert_eq!(r.unwrap_err().refusal(), Some(Refusal::TooLarge));
    // An incompressible file first raises the ratio's denominator; the
    // header after it still can't pass the overhead cap.
    let file = tar_entry(b"app/noise", EntryType::Regular, 0o644, &noise(2 << 20, 1));
    for ty in *b"LKx" {
        let bomb = streamed_header(&file, ty, 8 << 20, &[0u8; 1 << 16]);
        let r = run_with(limits, ArchiveKind::TarGz, &bomb).0;
        assert_eq!(
            r.unwrap_err().refusal(),
            Some(Refusal::TooLarge),
            "{}",
            ty as char
        );
    }
}

#[test]
fn folders_a_deep_name_creates_count_toward_the_entry_cap() {
    let limits = Limits {
        entries: 10,
        ..Limits::Q16
    };
    let deep = format!("{}f", "d/".repeat(20));
    let t = tgz(&[exe_tar(), tfile(&deep, b"")]);
    let r = run_with(limits, ArchiveKind::TarGz, &t).0;
    assert_eq!(r.unwrap_err().refusal(), Some(Refusal::TooManyEntries));
    let z = zip_of(&[(EXE, E::File(b"#!")), (deep.as_str(), E::File(b""))]);
    let r = run_with(limits, ArchiveKind::Zip, &z).0;
    assert_eq!(r.unwrap_err().refusal(), Some(Refusal::TooManyEntries));
}

#[test]
fn a_zip_index_is_read_under_the_overhead_budget() {
    // The zip crate allocates for each index record as it opens the
    // archive, so opening reads at most `overhead` bytes, however it finds
    // the index. Scaled down from a 4-million-record index.
    let limits = Limits {
        overhead: 64 * 1024,
        ..Limits::Q16
    };
    let many = |n: usize| {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let o = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        w.start_file(EXE, o).unwrap();
        for i in 0..n {
            w.start_file(format!("f{i:07}"), o).unwrap();
        }
        w.finish().unwrap().into_inner()
    };
    let z = many(5_000);
    let r = run_with(limits, ArchiveKind::Zip, &z).0;
    assert_eq!(r.unwrap_err().refusal(), Some(Refusal::TooLarge));

    // Over 65,535 entries the writer uses a zip64 index, which the crate
    // finds through other fields; the budget doesn't care.
    let z64 = many(70_000);
    let r = run_with(limits, ArchiveKind::Zip, &z64).0;
    assert_eq!(r.unwrap_err().refusal(), Some(Refusal::TooLarge));
    // Under Q16's budget it opens, and the entry cap refuses it.
    assert_eq!(
        refusal(ArchiveKind::Zip, &z64),
        Some(Refusal::TooManyEntries)
    );

    // An end record pushed further from the end than a comment allows
    // (trailing bytes the crate tolerates) is no way round it either.
    let mut far = z.clone();
    far.extend(std::iter::repeat_n(0u8, 65_560));
    let r = run_with(limits, ArchiveKind::Zip, &far).0;
    let refusal = r.unwrap_err().refusal();
    assert!(
        matches!(refusal, Some(Refusal::TooLarge) | None),
        "{refusal:?}"
    );
}

#[test]
fn an_encrypted_zip_entry_is_refused() {
    let mut z = zip_of(&[(EXE, E::File(b"#!"))]);
    let (cd, local) = zip_records(&z, EXE);
    z[cd + 8] |= 1;
    z[local + 6] |= 1;
    assert_eq!(refusal(ArchiveKind::Zip, &z), Some(Refusal::Encrypted));
}

#[test]
fn a_zip_bomb_over_the_ratio_is_refused() {
    let zeros = vec![0u8; 8 * 1024 * 1024];
    let z = zip_of(&[(EXE, E::File(b"#!")), ("app/bomb", E::File(&zeros))]);
    assert_eq!(refusal(ArchiveKind::Zip, &z), Some(Refusal::Ratio));
    let t = tgz(&[exe_tar(), tfile("app/bomb", &zeros)]);
    assert_eq!(refusal(ArchiveKind::TarGz, &t), Some(Refusal::Ratio));
}

#[test]
fn the_ratio_holds_for_the_whole_archive_too() {
    // Each entry stays under the floor; together they're far over 100:1.
    let chunk = vec![0u8; 60 * 1024];
    let names: Vec<String> = (0..40).map(|i| format!("app/z{i}")).collect();
    let mut v = vec![(EXE, E::File(b"#!"))];
    v.extend(names.iter().map(|n| (n.as_str(), E::File(&chunk))));
    let z = zip_of(&v);
    assert_eq!(refusal(ArchiveKind::Zip, &z), Some(Refusal::Ratio));
    let mut parts = vec![exe_tar()];
    parts.extend(names.iter().map(|n| tfile(n, &chunk)));
    assert_eq!(
        refusal(ArchiveKind::TarGz, &tgz(&parts)),
        Some(Refusal::Ratio)
    );
}

// ─── The executable ─────────────────────────────────────────────────────

#[test]
fn the_catalogues_executable_must_be_a_file_in_the_archive() {
    let z = zip_of(&[("app/bin/other", E::File(b"#!"))]);
    assert_eq!(
        refusal(ArchiveKind::Zip, &z),
        Some(Refusal::MissingExecutable)
    );
    let t = tgz(&[tfile("app/bin/APP", b"#!")]);
    assert_eq!(
        refusal(ArchiveKind::TarGz, &t),
        Some(Refusal::MissingExecutable)
    );
    let t = tgz(&[tdir("app/bin/app/")]);
    assert_eq!(
        refusal(ArchiveKind::TarGz, &t),
        Some(Refusal::ExecutableNotFile)
    );
    let z = zip_of(&[("app/bin/app/", E::Dir)]);
    assert_eq!(
        refusal(ArchiveKind::Zip, &z),
        Some(Refusal::ExecutableNotFile)
    );
}

// ─── Damage ─────────────────────────────────────────────────────────────

fn corrupt(kind: ArchiveKind, bytes: &[u8]) {
    let (r, _tmp) = run(kind, bytes);
    assert!(matches!(r, Err(ExtractError::Corrupt)), "{kind:?}: {r:?}");
}

#[test]
fn a_truncated_or_corrupt_stream_is_refused() {
    let data: Vec<u8> = (0..50_000u32)
        .flat_map(|i| i.wrapping_mul(2_654_435_761).to_le_bytes())
        .collect();
    let z = zip_of(&[(EXE, E::File(&data))]);
    let t = tgz(&[tfile(EXE, &data)]);

    corrupt(ArchiveKind::Zip, &z[..z.len() / 2]);
    corrupt(ArchiveKind::Zip, b"PK\x05\x06 not a zip");
    corrupt(ArchiveKind::Zip, b"");
    let mut flipped = z.clone();
    flipped[200] ^= 0xff;
    corrupt(ArchiveKind::Zip, &flipped);

    corrupt(ArchiveKind::TarGz, &t[..t.len() / 2]);
    corrupt(ArchiveKind::TarGz, &t[..t.len() - 4]);
    corrupt(ArchiveKind::TarGz, b"\x1f\x8b not gzip");
    let mut flipped = t.clone();
    let n = flipped.len();
    flipped[n - 6] ^= 0xff; // the gzip CRC, past the tar's end
    corrupt(ArchiveKind::TarGz, &flipped);
}

// ─── On disk ────────────────────────────────────────────────────────────

#[test]
fn an_existing_staging_folder_is_never_reused() {
    let tmp = Tmp::new();
    let staging = tmp.0.join("s.staging");
    fs::create_dir(&staging).unwrap();
    let t = tgz(&[exe_tar()]);
    let r = extract::extract(ArchiveKind::TarGz, &mut Cursor::new(&t), &exe(), &staging);
    assert!(matches!(r, Err(ExtractError::StagingInTheWay)), "{r:?}");
    assert!(staging.is_dir(), "someone else's folder is left alone");
}

#[cfg(unix)]
#[test]
fn a_symlink_planted_before_extraction_is_never_followed() {
    use std::os::unix::fs::symlink;
    let tmp = Tmp::new();
    let outside = tmp.0.join("outside");
    fs::create_dir(&outside).unwrap();
    let t = tgz(&[exe_tar()]);

    // The staging path itself.
    let staging = tmp.0.join("s.staging");
    symlink(&outside, &staging).unwrap();
    let r = extract::extract(ArchiveKind::TarGz, &mut Cursor::new(&t), &exe(), &staging);
    assert!(matches!(r, Err(ExtractError::StagingInTheWay)), "{r:?}");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);

    // A stale staging folder holding a link out: cleaned as a link.
    let data = tmp.0.join("data");
    fs::create_dir(&data).unwrap();
    let mut install = Install::open(&data, &id()).unwrap();
    let stale = install.root().join("1.0.staging");
    fs::create_dir(&stale).unwrap();
    symlink(&outside, stale.join("app")).unwrap();
    fs::write(outside.join("keep"), b"k").unwrap();
    install
        .stage(&v("1.0"), ArchiveKind::TarGz, &mut Cursor::new(&t), &exe())
        .unwrap();
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"k");
    assert!(!outside.join("bin").exists());
    assert!(install.root().join("1.0/app/bin/app").is_file());

    // A staging path that's a link: refused, the link and its target kept.
    symlink(&outside, install.root().join("2.0.staging")).unwrap();
    let r = install.stage(&v("2.0"), ArchiveKind::TarGz, &mut Cursor::new(&t), &exe());
    assert!(
        matches!(r, Err(LayoutError::Extract(ExtractError::StagingInTheWay))),
        "{r:?}"
    );
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"k");

    // A version folder that's a link: never extracted into.
    symlink(&outside, install.root().join("3.0")).unwrap();
    let r = install.stage(&v("3.0"), ArchiveKind::TarGz, &mut Cursor::new(&t), &exe());
    assert!(matches!(r, Err(LayoutError::AlreadyStaged)), "{r:?}");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
}

// ─── Property: names either refuse or stay inside ───────────────────────

fn component() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("..".to_owned()),
        Just(".".to_owned()),
        Just("".to_owned()),
        Just("app".to_owned()),
        Just("APP".to_owned()),
        Just("CON".to_owned()),
        Just("x.".to_owned()),
        Just("a:b".to_owned()),
        Just("C:".to_owned()),
        Just("a\\..".to_owned()),
        "[a-zA-Z0-9._ -]{1,8}",
    ]
}

fn name() -> impl Strategy<Value = String> {
    (
        any::<bool>(),
        prop::collection::vec(component(), 1..5),
        any::<bool>(),
    )
        .prop_map(|(lead, parts, dir)| {
            let mut s = if lead { "/".to_owned() } else { String::new() };
            s.push_str(&parts.join("/"));
            if dir {
                s.push('/');
            }
            s
        })
}

/// Every path under `root`, which must be only files and folders.
fn walk(root: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(root).unwrap() {
        let e = e.unwrap();
        let ft = e.file_type().unwrap();
        assert!(!ft.is_symlink());
        out.push(e.path());
        if ft.is_dir() {
            walk(&e.path(), out);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn arbitrary_names_refuse_or_extract_strictly_inside(
        names in prop::collection::vec(name(), 1..8),
        zip_format in any::<bool>(),
    ) {
        let bytes = if zip_format {
            let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
            let o = SimpleFileOptions::default();
            w.start_file(EXE, o).unwrap();
            for n in &names {
                // The writer refuses a few names outright (an exact
                // duplicate); those aren't what this property is about.
                if n.ends_with('/') {
                    let _ = w.add_directory(n.as_str(), o);
                } else if w.start_file(n.as_str(), o).is_ok() {
                    w.write_all(b"x").unwrap();
                }
            }
            w.finish().unwrap().into_inner()
        } else {
            let mut parts = vec![exe_tar()];
            for n in &names {
                let b = n.as_bytes();
                if b.len() < 100 {
                    parts.push(if n.ends_with('/') {
                        tar_entry(b, EntryType::Directory, 0o755, b"")
                    } else {
                        tar_entry(b, EntryType::Regular, 0o644, b"x")
                    });
                }
            }
            tgz(&parts)
        };
        let kind = if zip_format { ArchiveKind::Zip } else { ArchiveKind::TarGz };
        let (r, tmp) = run(kind, &bytes);
        let mut all = Vec::new();
        walk(&tmp.0, &mut all);
        let staging = tmp.0.join("s.staging");
        if r.is_ok() {
            for p in &all {
                prop_assert!(p.starts_with(&staging), "{p:?}");
                let rel = p.strip_prefix(&staging).unwrap();
                prop_assert!(rel.components().all(|c| matches!(c, std::path::Component::Normal(_))));
            }
        } else {
            prop_assert!(all.is_empty(), "{all:?}");
        }
    }
}

// ─── Layout ─────────────────────────────────────────────────────────────

fn id() -> CompanionId {
    CompanionId::parse("prowlarr").unwrap()
}

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

fn stage(install: &mut Install, version: &str, body: &[u8]) {
    let t = tgz(&[tfile(EXE, body)]);
    install
        .stage(
            &v(version),
            ArchiveKind::TarGz,
            &mut Cursor::new(&t),
            &exe(),
        )
        .unwrap();
}

fn names(install: &Install) -> Vec<String> {
    let mut n: Vec<String> = fs::read_dir(install.root())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    n.sort();
    n
}

fn pointer(current: &str, previous: Option<&str>) -> Option<Pointer> {
    Some(Pointer {
        current: v(current),
        previous: previous.map(v),
    })
}

#[test]
fn stage_commit_prune_and_rollback() {
    let tmp = Tmp::new();
    let mut install = Install::open(&tmp.0, &id()).unwrap();
    assert_eq!(install.root(), tmp.0.join("companions").join("prowlarr"));
    assert_eq!(install.pointer().unwrap(), None);
    assert!(matches!(install.rollback(), Err(LayoutError::NoCurrent)));
    assert!(matches!(
        install.commit(&v("1.0"), &exe()),
        Err(LayoutError::NotStaged)
    ));

    stage(&mut install, "1.0", b"one");
    assert!(matches!(
        install.stage(&v("1.0"), ArchiveKind::TarGz, &mut Cursor::new(b""), &exe()),
        Err(LayoutError::AlreadyStaged)
    ));
    assert_eq!(install.pointer().unwrap(), None, "staging doesn't switch");
    install.commit(&v("1.0"), &exe()).unwrap();
    assert_eq!(install.pointer().unwrap(), pointer("1.0", None));
    assert!(matches!(
        install.prune_previous(),
        Err(LayoutError::NoPrevious)
    ));
    assert!(matches!(install.rollback(), Err(LayoutError::NoPrevious)));

    // An update keeps the old version until it's pruned.
    stage(&mut install, "2.0", b"two");
    install.commit(&v("2.0"), &exe()).unwrap();
    assert_eq!(install.pointer().unwrap(), pointer("2.0", Some("1.0")));
    assert_eq!(names(&install), ["1.0", "2.0", POINTER]);
    let exe_path = install.executable(&v("2.0"), &exe());
    assert_eq!(fs::read(exe_path).unwrap(), b"two");
    install.prune_previous().unwrap();
    assert_eq!(install.pointer().unwrap(), pointer("2.0", None));
    assert_eq!(names(&install), ["2.0", POINTER]);

    // A failed update rolls back and the bad version goes.
    stage(&mut install, "3.0", b"three");
    install.commit(&v("3.0"), &exe()).unwrap();
    install.rollback().unwrap();
    assert_eq!(install.pointer().unwrap(), pointer("2.0", None));
    assert_eq!(names(&install), ["2.0", POINTER]);

    // Two updates without a prune: the oldest goes, one previous is kept.
    stage(&mut install, "4.0", b"4");
    install.commit(&v("4.0"), &exe()).unwrap();
    stage(&mut install, "5.0", b"5");
    install.commit(&v("5.0"), &exe()).unwrap();
    assert_eq!(install.pointer().unwrap(), pointer("5.0", Some("4.0")));
    assert_eq!(names(&install), ["4.0", "5.0", POINTER]);
}

#[test]
fn a_crash_between_any_two_steps_recovers() {
    let tmp = Tmp::new();
    let reopen = || Install::open(&tmp.0, &id()).unwrap();
    let mut install = reopen();
    stage(&mut install, "1.0", b"one");
    install.commit(&v("1.0"), &exe()).unwrap();

    // Crashed mid-extraction: a staging folder is left.
    let root = install.root().to_path_buf();
    fs::create_dir_all(root.join("2.0.staging/app")).unwrap();
    fs::write(root.join("2.0.staging/app/half"), b"h").unwrap();
    let mut install = reopen();
    assert_eq!(install.sweep().unwrap(), ["2.0.staging"]);
    assert_eq!(install.pointer().unwrap(), pointer("1.0", None));

    // A stale staging folder is also cleaned by the next stage.
    fs::create_dir_all(root.join("2.0.staging/app")).unwrap();
    stage(&mut install, "2.0", b"two");

    // Crashed after staging, before commit: the pointer is unchanged and
    // the version can be committed, or swept.
    let mut install = reopen();
    assert_eq!(install.pointer().unwrap(), pointer("1.0", None));
    assert_eq!(install.sweep().unwrap(), ["2.0"]);
    stage(&mut install, "2.0", b"two");
    install.commit(&v("2.0"), &exe()).unwrap();

    // Crashed while writing the pointer: the temporary file is ignored
    // and swept, and the old pointer stands.
    fs::write(root.join("current.tmp"), b"{\"format\":1,\"curr").unwrap();
    let mut install = reopen();
    assert_eq!(install.pointer().unwrap(), pointer("2.0", Some("1.0")));
    assert_eq!(install.sweep().unwrap(), ["current.tmp"]);

    // Crashed after commit, before prune or rollback: both still work.
    let mut install = reopen();
    assert_eq!(install.sweep().unwrap(), Vec::<String>::new());
    install.rollback().unwrap();
    assert_eq!(install.pointer().unwrap(), pointer("1.0", None));
    assert_eq!(names(&install), ["1.0", POINTER]);

    // Other files in the folder are never swept.
    fs::create_dir(root.join("config")).unwrap();
    fs::write(root.join("notes"), b"n").unwrap();
    assert_eq!(reopen().sweep().unwrap(), Vec::<String>::new());
}

#[test]
fn a_damaged_pointer_is_reported_not_guessed() {
    let tmp = Tmp::new();
    let install = Install::open(&tmp.0, &id()).unwrap();
    for bad in [
        &b"not json"[..],
        b"{\"format\":2,\"current\":\"1.0\"}",
        b"{\"format\":1,\"current\":\"../x\"}",
        b"{\"format\":1,\"current\":\"01.0\"}",
        b"{\"format\":1,\"current\":\"1.0\",\"extra\":1}",
    ] {
        fs::write(install.root().join(POINTER), bad).unwrap();
        assert!(matches!(install.pointer(), Err(LayoutError::BadPointer)));
    }
}

#[test]
fn remove_deletes_the_companion_folder_only() {
    let tmp = Tmp::new();
    fs::write(tmp.0.join("keep"), b"k").unwrap();
    let mut install = Install::open(&tmp.0, &id()).unwrap();
    stage(&mut install, "1.0", b"one");
    install.commit(&v("1.0"), &exe()).unwrap();
    let other = Install::open(&tmp.0, &CompanionId::parse("qbittorrent").unwrap()).unwrap();
    install.remove().unwrap();
    assert!(!tmp.0.join("companions/prowlarr").exists());
    assert!(other.root().is_dir());
    assert_eq!(fs::read(tmp.0.join("keep")).unwrap(), b"k");
    assert!(Install::existing(&tmp.0, &id()).unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn removal_never_follows_a_link() {
    use std::os::unix::fs::symlink;
    let tmp = Tmp::new();
    let outside = tmp.0.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), b"k").unwrap();
    let data = tmp.0.join("data");
    fs::create_dir(&data).unwrap();

    // Links inside are removed as links.
    let mut install = Install::open(&data, &id()).unwrap();
    stage(&mut install, "1.0", b"one");
    symlink(&outside, install.root().join("1.0/app/out")).unwrap();
    symlink(outside.join("keep"), install.root().join("link")).unwrap();
    install.remove().unwrap();
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"k");

    // A companion folder that's a link to elsewhere is refused.
    symlink(&outside, data.join("companions/prowlarr")).unwrap();
    assert!(matches!(
        Install::open(&data, &id()),
        Err(LayoutError::OutsideRoot)
    ));
    assert!(matches!(
        Install::existing(&data, &id()),
        Err(LayoutError::OutsideRoot)
    ));
    fs::remove_file(data.join("companions/prowlarr")).unwrap();

    // So is `companions` itself.
    let data2 = tmp.0.join("data2");
    fs::create_dir(&data2).unwrap();
    symlink(&data, data2.join("companions")).unwrap();
    assert!(matches!(
        Install::open(&data2, &id()),
        Err(LayoutError::OutsideRoot)
    ));
    // Nothing was created inside the link's target first.
    assert!(!data.join("prowlarr").exists());

    // A sweep through a folder swapped for a link deletes nothing there.
    let mut install = Install::open(&data, &id()).unwrap();
    let decoy = tmp.0.join("decoy");
    fs::create_dir_all(decoy.join("9.0/app")).unwrap();
    fs::create_dir_all(decoy.join("9.0.staging")).unwrap();
    fs::remove_dir_all(install.root()).unwrap();
    symlink(&decoy, install.root()).unwrap();
    assert!(matches!(install.sweep(), Err(LayoutError::OutsideRoot)));
    assert!(decoy.join("9.0/app").is_dir() && decoy.join("9.0.staging").is_dir());
    fs::remove_file(install.root()).unwrap();

    // And a folder swapped for a link after it was opened.
    let install = Install::open(&data, &id()).unwrap();
    fs::remove_dir(install.root()).unwrap();
    symlink(&outside, install.root()).unwrap();
    assert!(matches!(install.remove(), Err(LayoutError::OutsideRoot)));
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"k");
}

// ─── Windows: junctions need no privilege, so they're tested there ──────

#[cfg(windows)]
fn junction(link: &Path, target: &Path) {
    let ok = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "mklink /J {link:?}");
}

#[cfg(windows)]
#[test]
fn junctions_are_never_followed_on_windows() {
    let tmp = Tmp::new();
    let outside = tmp.0.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), b"k").unwrap();
    let t = tgz(&[exe_tar()]);

    // A staging path that's a junction is refused, not extracted into.
    let staging = tmp.0.join("s.staging");
    junction(&staging, &outside);
    let r = extract::extract(ArchiveKind::TarGz, &mut Cursor::new(&t), &exe(), &staging);
    assert!(matches!(r, Err(ExtractError::StagingInTheWay)), "{r:?}");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);

    let data = tmp.0.join("data");
    fs::create_dir(&data).unwrap();
    let mut install = Install::open(&data, &id()).unwrap();
    // A stale staging folder holding a junction out: cleaned as a link.
    let stale = install.root().join("1.0.staging");
    fs::create_dir(&stale).unwrap();
    junction(&stale.join("app"), &outside);
    install
        .stage(&v("1.0"), ArchiveKind::TarGz, &mut Cursor::new(&t), &exe())
        .unwrap();
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"k");
    install.commit(&v("1.0"), &exe()).unwrap();
    // A staging or version path that's a junction refuses.
    junction(&install.root().join("2.0.staging"), &outside);
    let r = install.stage(&v("2.0"), ArchiveKind::TarGz, &mut Cursor::new(&t), &exe());
    assert!(
        matches!(r, Err(LayoutError::Extract(ExtractError::StagingInTheWay))),
        "{r:?}"
    );
    junction(&install.root().join("3.0"), &outside);
    let r = install.stage(&v("3.0"), ArchiveKind::TarGz, &mut Cursor::new(&t), &exe());
    assert!(matches!(r, Err(LayoutError::AlreadyStaged)), "{r:?}");
    // Sweeping and removing take junctions out as links.
    assert_eq!(install.sweep().unwrap(), ["2.0.staging", "3.0"]);
    junction(&install.root().join("1.0").join("out"), &outside);
    install.remove().unwrap();
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"k");

    // A companion folder that's a junction is refused.
    junction(&data.join("companions").join("prowlarr"), &outside);
    assert!(matches!(
        Install::open(&data, &id()),
        Err(LayoutError::OutsideRoot)
    ));
    assert!(matches!(
        Install::existing(&data, &id()),
        Err(LayoutError::OutsideRoot)
    ));
}

#[test]
fn versions_compare_by_their_text() {
    let tmp = Tmp::new();
    let mut install = Install::open(&tmp.0, &id()).unwrap();
    stage(&mut install, "1.0", b"a");
    install.commit(&v("1.0"), &exe()).unwrap();
    stage(&mut install, "1", b"b");
    let done = install.commit(&v("1"), &exe()).unwrap();
    assert!(done.leftover.is_none());
    assert_eq!(done.pointer.current.as_str(), "1");
    assert_eq!(
        done.pointer.previous.as_ref().map(Version::as_str),
        Some("1.0")
    );
    // `1.0.0` is a different folder; nothing names it, so it's swept.
    fs::create_dir(install.root().join("1.0.0")).unwrap();
    assert_eq!(install.sweep().unwrap(), ["1.0.0"]);
    assert_eq!(names(&install), ["1", "1.0", POINTER]);
}

#[test]
fn a_partial_version_is_never_committed_and_tombstones_are_swept() {
    let tmp = Tmp::new();
    let mut install = Install::open(&tmp.0, &id()).unwrap();
    // A folder without the executable, or with a folder in its place,
    // isn't staged.
    fs::create_dir_all(install.root().join("3.0/app/bin")).unwrap();
    assert!(matches!(
        install.commit(&v("3.0"), &exe()),
        Err(LayoutError::NotStaged)
    ));
    fs::create_dir(install.root().join("3.0/app/bin/app")).unwrap();
    assert!(matches!(
        install.commit(&v("3.0"), &exe()),
        Err(LayoutError::NotStaged)
    ));
    // A crash partway through a delete leaves a tombstone; sweep takes it.
    fs::create_dir_all(install.root().join("2.0.deleting/app")).unwrap();
    fs::write(install.root().join("2.0.deleting/app/x"), b"x").unwrap();
    assert_eq!(install.sweep().unwrap(), ["2.0.deleting", "3.0"]);
    assert!(names(&install).is_empty());
}

#[test]
fn sweep_takes_the_current_versions_staging_folder_too() {
    let tmp = Tmp::new();
    let mut install = Install::open(&tmp.0, &id()).unwrap();
    stage(&mut install, "1.0", b"a");
    install.commit(&v("1.0"), &exe()).unwrap();
    fs::create_dir_all(install.root().join("1.0.staging/app")).unwrap();
    assert_eq!(install.sweep().unwrap(), ["1.0.staging"]);
    assert_eq!(names(&install), ["1.0", POINTER]);
}
