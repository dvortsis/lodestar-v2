//! Staging a downloaded version (§12.2 "Installing").
//!
//! `<data>/bin/<version>.tmp/` gets the binary (0700, moved from its
//! verified `.part` file), `manifest.json` and `manifest.json.minisig` as
//! fetched; each is fsynced, then the folder, then it's renamed to
//! `<version>/` and `<data>/bin` is fsynced. An overlay is inert until the
//! supervisor names it in `active`, so a crash at any point leaves at
//! worst a `.tmp` folder, removed by the next stage.
//!
//! Just before switching, the engine verifies the staged folder again as
//! the supervisor will ([`super::overlay::verify`]: the signature against
//! the embedded keys, the binary's hash and size) and the manifest again
//! against the current `trust.json`; then it sets the switch intent and
//! asks for the graceful shutdown, which `main.rs` turns into
//! `request.json` and `EXIT_SWITCH`.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

use lodestar_update::semver::Version;

use super::layout::{self, Bin};
use super::overlay::{BINARY, MANIFEST, SIGNATURE};

/// The points [`stage_with`] can be stopped at (tests' crash points).
#[cfg(test)]
pub const STEPS: usize = 7;

/// Stages `version` from its downloaded binary `part` and the signed
/// manifest it was pinned from. An existing `<version>/` (staged before,
/// never switched to) is replaced.
pub fn stage(
    bin: &Bin,
    version: &Version,
    part: &Path,
    manifest: &[u8],
    minisig: &str,
) -> io::Result<()> {
    stage_with(bin, version, part, manifest, minisig, &mut |_| Ok(()))
}

/// [`stage`], with `fault` called before each of its [`STEPS`] steps
/// (tests stop it there, as a crash would; production passes them all).
pub fn stage_with(
    bin: &Bin,
    version: &Version,
    part: &Path,
    manifest: &[u8],
    minisig: &str,
    fault: &mut dyn FnMut(usize) -> io::Result<()>,
) -> io::Result<()> {
    let name = version.to_string();
    // The folder's name is the version's canonical text, never a path.
    if layout::parse_version(&name).as_ref() != Some(version) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a version an overlay can be named by",
        ));
    }
    let tmp = bin.path(&format!("{name}.tmp"));
    remove_dir(&tmp)?;
    std::fs::create_dir(&tmp)?;
    let result = (|| {
        fault(0)?;
        write_new(&tmp.join(MANIFEST), manifest, 0o600)?;
        fault(1)?;
        write_new(&tmp.join(SIGNATURE), minisig.as_bytes(), 0o600)?;
        fault(2)?;
        let binary = tmp.join(BINARY);
        std::fs::rename(part, &binary)?;
        set_mode(&binary, 0o700)?;
        fault(3)?;
        File::open(&binary)?.sync_all()?;
        layout::sync_dir(&tmp);
        fault(4)?;
        bin.remove_overlay(version)?;
        fault(5)?;
        std::fs::rename(&tmp, bin.overlay(version))?;
        fault(6)?;
        layout::sync_dir(bin.dir());
        Ok(())
    })();
    if result.is_err() {
        let _ = remove_dir(&tmp);
    }
    result
}

fn write_new(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(layout::nofollow())
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

fn remove_dir(path: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn a_staged_version_has_its_three_files_and_no_tmp() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        let part = bin.path("0.6.0.part");
        std::fs::write(&part, b"binary").unwrap();
        let v = Version::new(0, 6, 0);
        stage(&bin, &v, &part, b"{manifest}", "sig").unwrap();
        let dir = bin.overlay(&v);
        assert_eq!(std::fs::read(dir.join(BINARY)).unwrap(), b"binary");
        assert_eq!(std::fs::read(dir.join(MANIFEST)).unwrap(), b"{manifest}");
        assert_eq!(std::fs::read(dir.join(SIGNATURE)).unwrap(), b"sig");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir.join(BINARY)), 0o700);
        assert_eq!(mode(&dir.join(MANIFEST)), 0o600);
        assert!(!part.exists(), "moved, not copied");
        assert!(!bin.path("0.6.0.tmp").exists());
        assert_eq!(bin.overlays().unwrap(), [v]);
    }

    /// A `.tmp` left by a crash, and a version staged before, are
    /// replaced; a failure leaves no `.tmp` behind.
    #[test]
    fn leftovers_are_replaced_and_a_failure_cleans_up() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        let v = Version::new(0, 6, 0);
        std::fs::create_dir(bin.path("0.6.0.tmp")).unwrap();
        std::fs::write(bin.path("0.6.0.tmp/junk"), b"x").unwrap();
        let part = bin.path("0.6.0.part");
        std::fs::write(&part, b"first").unwrap();
        stage(&bin, &v, &part, b"m", "s").unwrap();
        std::fs::write(&part, b"second").unwrap();
        stage(&bin, &v, &part, b"m2", "s2").unwrap();
        assert_eq!(
            std::fs::read(bin.overlay(&v).join(BINARY)).unwrap(),
            b"second"
        );
        assert!(!bin.overlay(&v).join("junk").exists());
        // No part file: it fails, and leaves no .tmp behind.
        assert!(stage(&bin, &v, &part, b"m3", "s3").is_err());
        assert!(!bin.path("0.6.0.tmp").exists());
    }

    /// A crash before any step leaves nothing the supervisor would run
    /// (only `active` names an overlay, and staging never writes it), and
    /// staging again from a fresh download finishes it.
    #[test]
    fn a_crash_at_every_step_is_finished_by_staging_again() {
        for step in 0..STEPS {
            let data = tempfile::tempdir().unwrap();
            let bin = Bin::open(data.path()).unwrap();
            let v = Version::new(0, 6, 0);
            // An older staging of the same version, never switched to.
            let part = bin.path("0.6.0.part");
            std::fs::write(&part, b"older").unwrap();
            stage(&bin, &v, &part, b"m0", "s0").unwrap();

            std::fs::write(&part, b"binary").unwrap();
            // A crash: the step fails and nothing after it runs (the
            // cleanup a live process does is skipped by leaking it).
            let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                stage_with(&bin, &v, &part, b"m", "s", &mut |n| {
                    if n == step {
                        panic!("crash at {n}");
                    }
                    Ok(())
                })
            }));
            assert!(crashed.is_err(), "step {step}");
            assert!(!bin.path("active").exists(), "step {step}");

            // The restart downloads again (a moved .part is gone) and stages.
            if !part.exists() {
                std::fs::write(&part, b"binary").unwrap();
            }
            stage(&bin, &v, &part, b"m", "s").unwrap();
            let dir = bin.overlay(&v);
            assert_eq!(
                std::fs::read(dir.join(BINARY)).unwrap(),
                b"binary",
                "{step}"
            );
            assert_eq!(std::fs::read(dir.join(MANIFEST)).unwrap(), b"m", "{step}");
            assert!(!bin.path("0.6.0.tmp").exists(), "step {step}");
            assert_eq!(bin.overlays().unwrap(), [v], "step {step}");
        }
    }

    #[test]
    fn a_version_with_build_metadata_is_never_a_folder_name() {
        let data = tempfile::tempdir().unwrap();
        let bin = Bin::open(data.path()).unwrap();
        let part = bin.path("x.part");
        std::fs::write(&part, b"binary").unwrap();
        let v = Version::parse("0.6.0+evil").unwrap();
        assert!(stage(&bin, &v, &part, b"m", "s").is_err());
        assert!(!bin.path("0.6.0+evil").exists());
        assert!(!bin.path("0.6.0+evil.tmp").exists());
    }
}
