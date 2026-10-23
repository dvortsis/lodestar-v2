//! `<data>/bin/trust.json` (§12.2): the trust state the checks advance.
//!
//! - **Written** forward-only (`TrustState::merge`: the sequence and the
//!   marks never go down), by temporary file, fsync, rename and a folder
//!   fsync. An unchanged state isn't rewritten.
//! - **Missing:** rebuilt from the floor (the embedded statements and
//!   those of every verified overlay), with high-water marks from the
//!   newest verified overlay of each channel, so deleting it never brings
//!   back a revoked key or an older manifest.
//! - **Found:** raised to that floor too (statements and marks), so a file
//!   restored from a backup can't either.
//! - **Damaged** (unreadable, or not verifying against the embedded keys):
//!   it blocks installs (`trust_damaged`) and is never overwritten; the
//!   check still runs from the rebuilt floor so notices and releases show
//!   and an overlay's revocations hold.

use std::io::{self, Read};
use std::path::Path;

use lodestar_update::trust::{MAX_TRUST, OverlayMark};
use lodestar_update::{Embedded, TrustState};

pub const FILE: &str = "trust.json";

/// What reading `trust.json` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loaded {
    /// Read and verified.
    Found(TrustState),
    /// It didn't exist: rebuilt from the floor and the overlays.
    Rebuilt(TrustState),
    /// It exists and doesn't verify: installs stay blocked, and this
    /// floor-only state is used for the check alone.
    Damaged(TrustState),
}

impl Loaded {
    pub fn state(&self) -> &TrustState {
        match self {
            Loaded::Found(s) | Loaded::Rebuilt(s) | Loaded::Damaged(s) => s,
        }
    }

    pub fn damaged(&self) -> bool {
        matches!(self, Loaded::Damaged(_))
    }
}

/// Reads `<dir>/trust.json`, raised to the floor (§12.2): the embedded
/// statements and those of every installed overlay count whether or not
/// the file exists, so a stale `trust.json` (restored from a backup) can't
/// forget a revocation an overlay carries. Each statement is checked
/// against the recovery key as it's added, so an overlay's statements need
/// no trust in the overlay. `overlays` also gives a missing file its
/// high-water marks.
pub fn load(dir: &Path, embedded: &Embedded, overlays: &[OverlayMark]) -> Loaded {
    // A damaged file still takes the overlays' statements and marks, so a
    // revocation or a newer manifest an overlay carries isn't lost; only
    // when they disagree with the chain is it the embedded floor alone.
    let damaged = || {
        Loaded::Damaged(
            TrustState::rebuild(embedded, overlays)
                .or_else(|_| TrustState::rebuild(embedded, &[]))
                .unwrap_or_default(),
        )
    };
    match read_capped(&dir.join(FILE)) {
        // A rebuild that fails (an overlay's statements disagree with
        // the chain) is damage: nothing is written that could lower a
        // mark.
        Ok(None) => match TrustState::rebuild(embedded, overlays) {
            Ok(state) => Loaded::Rebuilt(state),
            Err(_) => damaged(),
        },
        Ok(Some(bytes)) => match TrustState::load(&bytes, embedded) {
            Ok(state) => match raised(state, embedded, overlays) {
                Some(state) => Loaded::Found(state),
                None => damaged(),
            },
            Err(_) => damaged(),
        },
        // Unreadable now is damaged for now: nothing is written over it.
        Err(_) => damaged(),
    }
}

/// `state` with the embedded and overlay statements it doesn't have yet,
/// and each channel's high-water mark at least its newest overlay's (as
/// a rebuild has), so a file restored from a backup can't re-accept an
/// older manifest; `None` when the statements disagree with it.
fn raised(state: TrustState, embedded: &Embedded, overlays: &[OverlayMark]) -> Option<TrustState> {
    let mut chain = state.chain(embedded).ok()?;
    chain.extend(&embedded.statements, None).ok()?;
    for o in overlays {
        chain.extend(&o.statements, None).ok()?;
    }
    let mut next = state.advance(&chain);
    for o in overlays {
        let m = next.channels.entry(o.channel).or_default();
        if o.timestamp > m.hwm_ts {
            m.hwm_ts = o.timestamp;
            m.hwm_sha256 = Some(o.sha256);
        }
    }
    Some(next)
}

/// Writes `next` merged over `loaded` (never over a damaged file).
/// Returns the state now on disk, or `None` when nothing was written
/// because it was damaged or unchanged.
pub fn save(dir: &Path, loaded: &Loaded, next: TrustState) -> io::Result<Option<TrustState>> {
    let merged = match loaded {
        Loaded::Damaged(_) => return Ok(None),
        Loaded::Found(old) => {
            let merged = TrustState::merge(old, next);
            if merged == *old {
                return Ok(None);
            }
            merged
        }
        // Nothing on disk yet: the rebuilt state is the floor to merge on.
        Loaded::Rebuilt(base) => TrustState::merge(base, next),
    };
    let bytes = merged.to_bytes().map_err(|e| io::Error::other(e.code()))?;
    write_atomic(dir, FILE, &bytes)?;
    Ok(Some(merged))
}

/// At most [`MAX_TRUST`] + 1 bytes (a larger file fails to load);
/// `None` when it doesn't exist.
fn read_capped(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let file = match super::layout::open_nofollow(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        other => other?,
    };
    let mut out = Vec::new();
    file.take(MAX_TRUST as u64 + 1).read_to_end(&mut out)?;
    Ok(Some(out))
}

fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    super::layout::write_atomic(dir, name, bytes)
}

#[cfg(test)]
mod tests {
    use lodestar_update::testing::{TestKey, TestTrust, bytes, envelope, manifest, statement};
    use lodestar_update::{Channel, Error, Role, Sha256, check};

    use super::*;

    fn trust() -> (TestTrust, Embedded) {
        let t = TestTrust::default();
        let e = t.embedded(vec![]);
        (t, e)
    }

    #[test]
    fn a_missing_file_is_rebuilt_and_a_damaged_one_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let (_t, e) = trust();
        let loaded = load(dir.path(), &e, &[]);
        assert!(matches!(loaded, Loaded::Rebuilt(_)));
        // The first write creates it.
        let mut next = loaded.state().clone();
        next.channels.entry(Channel::Stable).or_default().hwm_ts = 100;
        assert!(save(dir.path(), &loaded, next).unwrap().is_some());
        let found = load(dir.path(), &e, &[]);
        assert!(matches!(found, Loaded::Found(_)));
        assert_eq!(found.state().channels[&Channel::Stable].hwm_ts, 100);

        std::fs::write(dir.path().join(FILE), b"{ not trust").unwrap();
        let damaged = load(dir.path(), &e, &[]);
        assert!(damaged.damaged());
        let mut next = damaged.state().clone();
        next.channels.entry(Channel::Stable).or_default().hwm_ts = 200;
        assert_eq!(save(dir.path(), &damaged, next).unwrap(), None);
        assert_eq!(
            std::fs::read(dir.path().join(FILE)).unwrap(),
            b"{ not trust",
            "never overwritten"
        );
    }

    #[test]
    fn marks_only_go_forward_and_an_unchanged_state_isnt_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let (_t, e) = trust();
        let first = load(dir.path(), &e, &[]);
        let mut at = first.state().clone();
        at.channels.entry(Channel::Stable).or_default().hwm_ts = 500;
        save(dir.path(), &first, at.clone()).unwrap();
        let found = load(dir.path(), &e, &[]);
        let mut lower = found.state().clone();
        lower.channels.entry(Channel::Stable).or_default().hwm_ts = 400;
        assert_eq!(save(dir.path(), &found, lower).unwrap(), None);
        assert_eq!(
            load(dir.path(), &e, &[]).state().channels[&Channel::Stable].hwm_ts,
            500
        );
    }

    /// A stale `trust.json` (restored from a backup) still honours a
    /// rotation an installed overlay carries.
    #[test]
    fn a_stale_file_is_raised_to_the_overlays() {
        let dir = tempfile::tempdir().unwrap();
        let (t, e) = trust();
        let first = load(dir.path(), &e, &[]);
        save(dir.path(), &first, first.state().clone()).unwrap();
        let mut stale = first.state().clone();
        stale.channels.entry(Channel::Stable).or_default().hwm_ts = 50;
        std::fs::write(dir.path().join(FILE), stale.to_bytes().unwrap()).unwrap();
        let rotation = statement(&t.recovery, 1, Role::Release, &TestKey::new(9).public(), 1);
        let overlay = OverlayMark {
            channel: Channel::Stable,
            timestamp: 1_000,
            sha256: lodestar_update::Sha256::of(b"body"),
            statements: vec![rotation],
        };
        let found = load(dir.path(), &e, std::slice::from_ref(&overlay));
        assert!(matches!(found, Loaded::Found(_)));
        assert_eq!(found.state().seq, 1, "the overlay's rotation counts");
        // A forged statement (not the recovery key's) makes it damaged,
        // not silently ignored.
        let forged = statement(
            &TestKey::new(7),
            1,
            Role::Release,
            &TestKey::new(9).public(),
            1,
        );
        let bad = OverlayMark {
            statements: vec![forged],
            ..overlay
        };
        assert!(load(dir.path(), &e, std::slice::from_ref(&bad)).damaged());
        std::fs::remove_file(dir.path().join(FILE)).unwrap();
        assert!(
            load(dir.path(), &e, &[bad]).damaged(),
            "a failed rebuild is damage"
        );
    }

    /// Deleting `trust.json` doesn't forget a revocation an overlay
    /// carries: the rebuild takes the overlays' statements.
    #[test]
    fn a_rebuild_keeps_what_the_overlays_carry() {
        let dir = tempfile::tempdir().unwrap();
        let (t, e) = trust();
        let rotation = statement(&t.recovery, 1, Role::Release, &TestKey::new(9).public(), 1);
        let overlay = OverlayMark {
            channel: Channel::Stable,
            timestamp: 1_000,
            sha256: lodestar_update::Sha256::of(b"body"),
            statements: vec![rotation],
        };
        let rebuilt = load(dir.path(), &e, std::slice::from_ref(&overlay));
        assert_eq!(rebuilt.state().seq, 1);
        assert!(rebuilt.state().channels[&Channel::Stable].hwm_ts >= 1_000);
    }

    const T: u64 = 1_800_000_000;

    /// An envelope with a stable manifest at `ts`, signed by `key`.
    fn signed_at(key: &TestKey, ts: u64) -> Vec<u8> {
        let body = bytes(&manifest(Channel::Stable, ts, "0.5.4", b"binary"));
        envelope(&key.signed(&body), None)
    }

    /// A damaged `trust.json` still honours a revocation an installed
    /// overlay carries: the old release key's manifest is refused.
    #[test]
    fn a_damaged_file_keeps_the_overlays_revocations() {
        let dir = tempfile::tempdir().unwrap();
        let (t, e) = trust();
        let rotation = statement(&t.recovery, 1, Role::Release, &TestKey::new(9).public(), T);
        let overlay = OverlayMark {
            channel: Channel::Stable,
            timestamp: T + 10,
            sha256: Sha256::of(b"body"),
            statements: vec![rotation],
        };
        std::fs::write(dir.path().join(FILE), b"{ not trust").unwrap();
        let damaged = load(dir.path(), &e, std::slice::from_ref(&overlay));
        assert!(damaged.damaged());
        assert_eq!(damaged.state().seq, 1);
        assert_eq!(
            check(
                &signed_at(&t.release, T + 100),
                Channel::Stable,
                T + 200,
                &e,
                damaged.state()
            )
            .unwrap_err(),
            Error::RevokedKey
        );
    }

    /// A `trust.json` restored from a backup, with marks older than an
    /// installed overlay's, refuses a manifest older than the overlay's.
    #[test]
    fn a_stale_file_is_raised_to_the_overlays_marks() {
        let dir = tempfile::tempdir().unwrap();
        let (t, e) = trust();
        let first = load(dir.path(), &e, &[]);
        let mut stale = first.state().clone();
        stale.channels.entry(Channel::Stable).or_default().hwm_ts = T;
        std::fs::write(dir.path().join(FILE), stale.to_bytes().unwrap()).unwrap();
        let overlay = OverlayMark {
            channel: Channel::Stable,
            timestamp: T + 500,
            sha256: Sha256::of(b"body"),
            statements: vec![],
        };
        let found = load(dir.path(), &e, std::slice::from_ref(&overlay));
        assert!(matches!(found, Loaded::Found(_)));
        assert_eq!(found.state().channels[&Channel::Stable].hwm_ts, T + 500);
        let older = signed_at(&t.release, T + 100);
        assert_eq!(
            check(&older, Channel::Stable, T + 600, &e, found.state()).unwrap_err(),
            Error::Older
        );
        // Without the overlay, the stale mark alone lets it through.
        let alone = load(dir.path(), &e, &[]);
        assert!(check(&older, Channel::Stable, T + 600, &e, alone.state()).is_ok());
    }
}
