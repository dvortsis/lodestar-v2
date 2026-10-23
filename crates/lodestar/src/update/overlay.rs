//! Verifying an overlay before each start (§12.2): its manifest and
//! signature, then the binary's SHA-256 and size, from the built-in
//! recovery key alone (`lodestar_update::overlay`). No expiry check, so an
//! installed version keeps starting offline after it migrated the store.
//!
//! On Linux the binary is copied into a sealed memfd (no writes, no
//! growing or shrinking, no further seals), the copy is what's hashed, and
//! the copy is what runs (`/proc/self/fd/<n>`): no write to `<data>/bin`
//! between the check and the start can change what starts. Where a memfd
//! isn't available (an old kernel, a seccomp policy) the descriptor opened
//! with `O_NOFOLLOW` is hashed and run instead, which a writer holding the
//! same file open could still change; the supervisor says so. Elsewhere
//! the path runs. Either residual needs write access to `<data>`, which is
//! control of the install already (see the decisions file).
//!
//! A definitive failure (missing, tampered, not a regular file) is a
//! [`Refusal::Bad`]; an I/O error that may pass is [`Refusal::Unavailable`]
//! and leaves the overlay in place.

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use lodestar_update::semver::Version;
use lodestar_update::{Overlay, Sha256};
use sha2::Digest as _;

use super::layout::{self, Bin};

pub const BINARY: &str = "lodestar";
pub const MANIFEST: &str = "manifest.json";
pub const SIGNATURE: &str = "manifest.json.minisig";

/// A verified overlay, ready to run.
#[derive(Debug)]
pub struct Verified {
    pub overlay: Overlay,
    /// Holds the verified binary (the sealed copy) open until the child
    /// has started.
    pub file: File,
    /// What to run: the descriptor on Linux, else the path.
    pub program: PathBuf,
    /// Why no sealed copy was made, when it wasn't (Linux only).
    pub unsealed: Option<String>,
}

/// Why an overlay can't run.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Its key is revoked by this build's own statements.
    Revoked,
    /// Definitive: missing, tampered, the wrong kind of file.
    Bad(String),
    /// An I/O error that may pass: the overlay stays for the next start.
    Unavailable(String),
}

/// A definitive refusal for what can't change by itself, else a
/// transient one.
fn io_refusal(what: &str, e: &std::io::Error) -> Refusal {
    use std::io::ErrorKind::*;
    let definitive = matches!(e.kind(), NotFound | InvalidData | InvalidInput)
        || e.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error());
    let why = format!("{what}: {e}");
    if definitive {
        Refusal::Bad(why)
    } else {
        Refusal::Unavailable(why)
    }
}

/// Verifies `<bin>/<version>/` for this build's target.
pub fn verify(bin: &Bin, version: &Version) -> Result<Verified, Refusal> {
    let dir = bin.overlay(version);
    let bad = |what: &str, e: &dyn std::fmt::Display| Refusal::Bad(format!("{what}: {e}"));
    if super::testing::fault_set("overlay:io") {
        return Err(Refusal::Unavailable("test fault".into()));
    }
    let manifest = read_capped(
        &dir.join(MANIFEST),
        lodestar_update::doc::MAX_MANIFEST as u64,
    )
    .map_err(|e| io_refusal("reading its manifest", &e))?;
    let minisig = read_capped(&dir.join(SIGNATURE), 1024)
        .map_err(|e| io_refusal("reading its signature", &e))?;
    let minisig = String::from_utf8(minisig).map_err(|e| bad("its signature", &e))?;
    let path = dir.join(BINARY);
    let src = layout::open_nofollow(&path).map_err(|e| io_refusal("opening its binary", &e))?;
    if !is_executable(&src) {
        return Err(Refusal::Bad("its binary isn't executable".into()));
    }
    let (mut file, unsealed) = sealed_copy(src)?;
    let (sha256, len) = hash(&mut file).map_err(|e| io_refusal("reading its binary", &e))?;
    if len > max_binary() {
        return Err(Refusal::Bad("its binary is too large".into()));
    }
    let target = super::testing::target()
        .or_else(|| lodestar_update::Target::from_triple(env!("LODESTAR_BUILD_TARGET")))
        // This build's problems, not the overlay's: it's kept.
        .ok_or_else(|| {
            Refusal::Unavailable(format!(
                "this build's target ({}) isn't shipped",
                env!("LODESTAR_BUILD_TARGET")
            ))
        })?;
    let embedded =
        super::trust_root().map_err(|e| Refusal::Unavailable(format!("the built-in keys: {e}")))?;
    let overlay =
        lodestar_update::overlay(&manifest, &minisig, sha256, len, version, target, &embedded)
            .map_err(|e| match e {
                lodestar_update::Error::RevokedKey => Refusal::Revoked,
                e => bad("verification", &e),
            })?;
    let program = program(&file, path);
    Ok(Verified {
        overlay,
        file,
        program,
        unsealed,
    })
}

/// The largest binary read: anything past a release artifact's limit is
/// refused before it's hashed or copied (smaller under the debug-only
/// `max_binary` override).
fn max_binary() -> u64 {
    super::testing::max_binary().unwrap_or(lodestar_update::doc::MAX_ARTIFACT_SIZE)
}

/// The binary copied into a sealed memfd, rewound; or, where that isn't
/// available, the file itself and why.
#[cfg(target_os = "linux")]
fn sealed_copy(src: File) -> Result<(File, Option<String>), Refusal> {
    use rustix::fs::{MemfdFlags, SealFlags};
    use std::io::{Seek, SeekFrom};
    let flags = MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING;
    // MFD_EXEC (Linux 6.3+) keeps it executable under `vm.memfd_noexec`;
    // older kernels refuse the flag.
    let fd = match rustix::fs::memfd_create("lodestar", flags | MemfdFlags::EXEC) {
        Err(rustix::io::Errno::INVAL) => rustix::fs::memfd_create("lodestar", flags),
        other => other,
    };
    let fd = match fd {
        Ok(fd) => fd,
        Err(e) => return Ok((src, Some(format!("no memfd: {e}")))),
    };
    let mut copy = File::from(fd);
    let copied = std::io::copy(&mut src.take(max_binary() + 1), &mut copy)
        .map_err(|e| io_refusal("copying its binary", &e))?;
    if copied > max_binary() {
        return Err(Refusal::Bad("its binary is too large".into()));
    }
    let seals = SealFlags::SEAL | SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE;
    rustix::fs::fcntl_add_seals(&copy, seals)
        .map_err(|e| Refusal::Unavailable(format!("sealing its copy: {e}")))?;
    copy.seek(SeekFrom::Start(0))
        .map_err(|e| io_refusal("rewinding its copy", &e))?;
    Ok((copy, None))
}

#[cfg(not(target_os = "linux"))]
fn sealed_copy(src: File) -> Result<(File, Option<String>), Refusal> {
    Ok((src, None))
}

fn read_capped(path: &std::path::Path, cap: u64) -> std::io::Result<Vec<u8>> {
    let f = layout::open_nofollow(path)?;
    let mut out = Vec::new();
    f.take(cap + 1).read_to_end(&mut out)?;
    if out.len() as u64 > cap {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "too large",
        ));
    }
    Ok(out)
}

/// The SHA-256 and length of at most [`max_binary`] + 1 bytes (so the
/// size check refuses a larger file).
fn hash(file: &mut File) -> std::io::Result<(Sha256, u64)> {
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut len = 0u64;
    let mut capped = file.take(max_binary() + 1);
    loop {
        let n = capped.read(&mut buf)?;
        if n == 0 {
            break;
        }
        len += n as u64;
        h.update(&buf[..n]);
    }
    Ok((Sha256(h.finalize().into()), len))
}

fn is_executable(file: &File) -> bool {
    use std::os::unix::fs::PermissionsExt;
    file.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o100 != 0)
}

#[cfg(target_os = "linux")]
fn program(file: &File, _path: PathBuf) -> PathBuf {
    use std::os::fd::AsRawFd;
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

/// Elsewhere the path is run (see the module's residuals).
#[cfg(not(target_os = "linux"))]
fn program(_file: &File, path: PathBuf) -> PathBuf {
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind};

    /// What can't change by itself sets the overlay aside; what may pass
    /// keeps it.
    #[test]
    fn only_definitive_errors_are_bad() {
        let bad = |e: Error| matches!(io_refusal("x", &e), Refusal::Bad(_));
        assert!(bad(Error::from(ErrorKind::NotFound)));
        assert!(bad(Error::new(ErrorKind::InvalidData, "too large")));
        assert!(bad(Error::from_raw_os_error(
            rustix::io::Errno::LOOP.raw_os_error()
        )));
        for transient in [
            Error::from(ErrorKind::PermissionDenied),
            Error::from_raw_os_error(rustix::io::Errno::IO.raw_os_error()),
            Error::from_raw_os_error(rustix::io::Errno::MFILE.raw_os_error()),
        ] {
            assert!(!bad(transient));
        }
    }
}
