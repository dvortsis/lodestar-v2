//! Updates, part 2 (architecture §12.2, Unix): `lodestar serve` as the
//! supervisor of an overlay child, probation with journaled rollback, and
//! the child's side of it. Part 3 adds the check, the install and the API;
//! until then nothing stages an overlay outside tests, so this runs the
//! image's own code as its child.
//!
//! `<data>/bin`'s files and who writes them are in [`layout`]; the exit
//! codes and the request file in [`control`]; probation's accounting in
//! [`pending`]; the rollback steps in [`journal`].

//! Only [`log_events`] exists off Unix, so the log catalogue (and its
//! golden) is the same everywhere.

#[cfg(unix)]
#[allow(dead_code)] // The update engine's (part 4).
pub mod check;
#[cfg(unix)]
pub mod control;
#[cfg(unix)]
pub mod history;
#[cfg(unix)]
pub mod journal;
#[cfg(unix)]
pub mod layout;
pub mod log_events;
#[cfg(unix)]
pub mod overlay;
#[cfg(unix)]
pub mod pending;
#[cfg(unix)]
#[allow(dead_code)] // The update engine's (part 4).
pub mod policy;
#[cfg(unix)]
pub mod probation;
#[cfg(unix)]
pub mod supervisor;
#[cfg(unix)]
pub mod testing;
#[cfg(unix)]
#[allow(dead_code)] // The update engine's (part 4).
pub mod trust_file;
#[allow(dead_code)] // The update engine's (part 4); `Window::parse` checks settings now.
pub mod window;

#[cfg(unix)]
use lodestar_update::semver::Version;
#[cfg(unix)]
use lodestar_update::{Embedded, PublicKey};

/// This build's version (`LODESTAR_BUILD_VERSION`), or the debug-only
/// pretend one.
#[cfg(unix)]
pub fn build_version() -> Version {
    testing::version()
        .and_then(layout::parse_version)
        .or_else(|| Version::parse(env!("LODESTAR_BUILD_VERSION")).ok())
        .unwrap_or_else(|| Version::new(0, 0, 0))
}

/// The keys overlays are verified with: the embedded ones, or the
/// debug-only test root.
#[cfg(unix)]
pub fn trust_root() -> Result<Embedded, lodestar_update::Error> {
    let Some(t) = testing::trust_root() else {
        return Embedded::builtin();
    };
    let key = |k: &Option<String>| k.as_deref().map(PublicKey::from_base64).transpose();
    Ok(Embedded {
        recovery: PublicKey::from_base64(&t.recovery)?,
        release: key(&t.release)?,
        nightly: key(&t.nightly)?,
        freshness: key(&t.freshness)?,
        statements: t.statements.clone(),
    })
}
