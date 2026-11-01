//! Roll back's cheap check ([`journal::kept`]), cached between
//! `GET /updates` polls so a polled status doesn't list the store's
//! folder or open the store each time.
//!
//! The answer depends on `<bin>/<version>/migration.json`, the snapshots
//! beside the store and the store's schema. It's reused only while a
//! probe of the same inputs (the marker's size and time, the store
//! folder's time, which any snapshot added, removed or restored changes,
//! whoever does it) still matches, for [`MAX_AGE`] at most, and never
//! when an input changed less than [`SETTLE`] ago: a coarse file clock
//! may not move again for a change right after it. The engine also
//! clears it on every `updates` event, and `POST /updates/rollback`
//! never uses it.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use super::journal::{self, Kept};
use super::layout;

/// A cached answer is trusted this long at most.
pub const MAX_AGE: Duration = Duration::from_secs(30);

/// Inputs changed more recently than this aren't cached.
pub const SETTLE: Duration = Duration::from_secs(3);

/// What the answer was computed from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Probe {
    version: String,
    /// `None` when there's no marker.
    marker: Option<(SystemTime, u64)>,
    folder: SystemTime,
}

impl Probe {
    /// `None` when something can't be read: then nothing is cached.
    fn of(ctx: &journal::Ctx, version: &str) -> Option<Probe> {
        let v = layout::parse_version(version)?;
        let marker = match std::fs::symlink_metadata(ctx.bin.overlay(&v).join("migration.json")) {
            Ok(m) => Some((m.modified().ok()?, m.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return None,
        };
        // The folder `snapshots::list` reads.
        let folder = match ctx.db.parent() {
            Some(d) if !d.as_os_str().is_empty() => d,
            _ => Path::new("."),
        };
        let folder = std::fs::metadata(folder).ok()?.modified().ok()?;
        Some(Probe {
            version: version.to_string(),
            marker,
            folder,
        })
    }

    /// Every input last changed at least [`SETTLE`] before `now`.
    fn settled(&self, now: SystemTime) -> bool {
        let Some(before) = now.checked_sub(SETTLE) else {
            return false;
        };
        self.folder <= before && self.marker.is_none_or(|(t, _)| t <= before)
    }
}

/// The last answer and when it was computed.
#[derive(Default)]
pub struct Cache(Mutex<Option<(Probe, Kept, Instant)>>);

impl Cache {
    /// [`journal::kept`], from the cache when its inputs haven't changed.
    pub fn kept(&self, ctx: &journal::Ctx, version: &str) -> Kept {
        self.kept_at(ctx, version, SystemTime::now(), Instant::now())
    }

    fn kept_at(&self, ctx: &journal::Ctx, version: &str, wall: SystemTime, now: Instant) -> Kept {
        // Probed before the answer: a change between the two leaves a
        // probe that no longer matches, so it's computed again next time.
        let probe = Probe::of(ctx, version);
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let (Some(p), Some((cached, kept, at))) = (&probe, &*slot)
            && p == cached
            && now.saturating_duration_since(*at) < MAX_AGE
        {
            return *kept;
        }
        let kept = journal::kept(ctx, version);
        *slot = probe.filter(|p| p.settled(wall)).map(|p| (p, kept, now));
        kept
    }

    /// Forgets the answer (an install, a rollback or a restart began).
    pub fn clear(&self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    #[cfg(test)]
    fn is_filled(&self) -> bool {
        self.0.lock().unwrap().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::layout::Bin;
    use crate::update::pending::Migration;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    struct Setup {
        _dir: tempfile::TempDir,
        ctx: journal::Ctx,
        later: SystemTime,
    }

    /// A store folder with `0.9.0`'s marker: it migrated the store from
    /// `latest - 1`, and its `pre-v<latest>` snapshot is there. `later` is far enough
    /// past every input's time for them to count as settled.
    fn setup() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(data.join("store")).unwrap();
        let bin = Bin::open(&data).unwrap();
        let db = data.join("store").join("lodestar.db");
        let latest = lodestar_store::latest_schema_version();
        let overlay = bin.overlay(&lodestar_update::semver::Version::new(0, 9, 0));
        std::fs::create_dir_all(&overlay).unwrap();
        let m = Migration {
            from: latest - 1,
            to: latest,
            started_at: 1,
            before: vec![],
        };
        std::fs::write(
            overlay.join("migration.json"),
            serde_json::to_vec(&m).unwrap(),
        )
        .unwrap();
        // The store, at the schema it was migrated to.
        drop(lodestar_store::Store::open(&db, lodestar_store::StoreConfig::default()).unwrap());
        std::fs::write(
            db.with_file_name(format!("lodestar.pre-v{latest}.db")),
            b"x",
        )
        .unwrap();
        let ctx = journal::Ctx {
            bin,
            db,
            index_root: data.join("index"),
            stop: Arc::new(AtomicBool::new(false)),
            image_version: "0.5.0".into(),
        };
        Setup {
            _dir: dir,
            ctx,
            later: SystemTime::now() + Duration::from_secs(3600),
        }
    }

    fn snapshot(s: &Setup) -> std::path::PathBuf {
        let latest = lodestar_store::latest_schema_version();
        s.ctx
            .db
            .with_file_name(format!("lodestar.pre-v{latest}.db"))
    }

    /// A settled answer is reused; removing the snapshot (the store
    /// folder changes) is seen on the next poll, never a stale "kept".
    #[test]
    fn a_removed_snapshot_is_seen_at_once() {
        let s = setup();
        let c = Cache::default();
        let t = Instant::now();
        assert_eq!(c.kept_at(&s.ctx, "0.9.0", s.later, t), Kept::RestoresStore);
        assert!(c.is_filled());
        assert_eq!(c.kept_at(&s.ctx, "0.9.0", s.later, t), Kept::RestoresStore);

        std::fs::remove_file(snapshot(&s)).unwrap();
        assert_eq!(c.kept_at(&s.ctx, "0.9.0", s.later, t), Kept::Missing);
    }

    /// The probe matches only for the same version and marker.
    #[test]
    fn a_changed_marker_or_version_is_computed_again() {
        let s = setup();
        let c = Cache::default();
        let t = Instant::now();
        assert_eq!(c.kept_at(&s.ctx, "0.9.0", s.later, t), Kept::RestoresStore);
        assert_eq!(c.kept_at(&s.ctx, "0.8.0", s.later, t), Kept::KeepsStore);
        assert_eq!(c.kept_at(&s.ctx, "0.9.0", s.later, t), Kept::RestoresStore);
        let marker = s
            .ctx
            .bin
            .overlay(&lodestar_update::semver::Version::new(0, 9, 0))
            .join("migration.json");
        std::fs::write(&marker, b"{").unwrap();
        assert_eq!(c.kept_at(&s.ctx, "0.9.0", s.later, t), Kept::Missing);
        assert_eq!(
            c.kept_at(&s.ctx, "not a version", s.later, t),
            Kept::Missing
        );
        assert!(!c.is_filled(), "an unreadable version isn't cached");
    }

    /// Inputs changed within [`SETTLE`] aren't cached; an answer older
    /// than [`MAX_AGE`] is computed again; `clear` forgets it.
    #[test]
    fn fresh_changes_age_and_clear_skip_the_cache() {
        let s = setup();
        let c = Cache::default();
        let t = Instant::now();
        assert_eq!(
            c.kept_at(&s.ctx, "0.9.0", SystemTime::now(), t),
            Kept::RestoresStore
        );
        assert!(!c.is_filled(), "just written: not settled");

        c.kept_at(&s.ctx, "0.9.0", s.later, t);
        assert!(c.is_filled());
        c.clear();
        assert!(!c.is_filled());

        c.kept_at(&s.ctx, "0.9.0", s.later, t);
        // Gone, with a probe that (as if the file clock missed it) still
        // matches: only the age limit catches that.
        std::fs::remove_file(snapshot(&s)).unwrap();
        let p = Probe::of(&s.ctx, "0.9.0").unwrap();
        *c.0.lock().unwrap() = Some((p.clone(), Kept::RestoresStore, t));
        assert_eq!(
            c.kept_at(&s.ctx, "0.9.0", s.later, t + MAX_AGE / 2),
            Kept::RestoresStore,
            "within the age limit, a matching probe is trusted"
        );
        *c.0.lock().unwrap() = Some((p, Kept::RestoresStore, t));
        let stale = c.kept_at(&s.ctx, "0.9.0", s.later, t + MAX_AGE);
        assert_eq!(stale, Kept::Missing, "past the age limit");
    }
}
