//! Verifying an overlay before each start (§12.2): its manifest and
//! signature, then the binary's SHA-256 and size, from the built-in
//! recovery key alone (`lodestar_update::overlay`). No expiry check, so an
//! installed version keeps starting offline after it migrated the store.
//!
//! The binary is hashed through a descriptor opened with `O_NOFOLLOW`, and
//! on Linux that same descriptor is what runs (`/proc/self/fd/<n>`), so a
//! file swapped in after the check can't be started instead.

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
    /// Holds the verified binary open until the child has started.
    pub file: File,
    /// What to run: the descriptor on Linux, else the path.
    pub program: PathBuf,
}

/// Why an overlay can't run.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Its key is revoked by this build's own statements.
    Revoked,
    /// Anything else: missing, unreadable, tampered.
    Bad(String),
}

/// Verifies `<bin>/<version>/` for this build's target.
pub fn verify(bin: &Bin, version: &Version) -> Result<Verified, Refusal> {
    let dir = bin.overlay(version);
    let bad = |what: &str, e: &dyn std::fmt::Display| Refusal::Bad(format!("{what}: {e}"));
    let manifest = read_capped(
        &dir.join(MANIFEST),
        lodestar_update::doc::MAX_MANIFEST as u64,
    )
    .map_err(|e| bad("reading its manifest", &e))?;
    let minisig =
        read_capped(&dir.join(SIGNATURE), 1024).map_err(|e| bad("reading its signature", &e))?;
    let minisig = String::from_utf8(minisig).map_err(|e| bad("its signature", &e))?;
    let path = dir.join(BINARY);
    let mut file = layout::open_nofollow(&path).map_err(|e| bad("opening its binary", &e))?;
    let (sha256, len) = hash(&mut file).map_err(|e| bad("reading its binary", &e))?;
    let target = super::testing::target()
        .or_else(|| lodestar_update::Target::from_triple(env!("LODESTAR_BUILD_TARGET")))
        .ok_or_else(|| {
            Refusal::Bad(format!(
                "this build's target ({}) isn't shipped",
                env!("LODESTAR_BUILD_TARGET")
            ))
        })?;
    let embedded = super::trust_root().map_err(|e| bad("the built-in keys", &e))?;
    let overlay =
        lodestar_update::overlay(&manifest, &minisig, sha256, len, version, target, &embedded)
            .map_err(|e| match e {
                lodestar_update::Error::RevokedKey => Refusal::Revoked,
                e => bad("verification", &e),
            })?;
    if !is_executable(&file) {
        return Err(Refusal::Bad("its binary isn't executable".into()));
    }
    let program = program(&file, path);
    Ok(Verified {
        overlay,
        file,
        program,
    })
}

fn read_capped(path: &std::path::Path, cap: u64) -> std::io::Result<Vec<u8>> {
    let f = layout::open_nofollow(path)?;
    let mut out = Vec::new();
    f.take(cap + 1).read_to_end(&mut out)?;
    if out.len() as u64 > cap {
        return Err(std::io::Error::other("too large"));
    }
    Ok(out)
}

fn hash(file: &mut File) -> std::io::Result<(Sha256, u64)> {
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut len = 0u64;
    loop {
        let n = file.read(&mut buf)?;
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

/// Elsewhere the path is run; the residual (a swap between the check and
/// the start needs write access to `<data>`, which is full control
/// already) is recorded in the decisions file.
#[cfg(not(target_os = "linux"))]
fn program(_file: &File, path: PathBuf) -> PathBuf {
    path
}
