//! The core re-judge (§20.5): once the core changes (its
//! [`CORE_VERSION`] goes up), every stored torrent judged by an older one
//! is judged again by today's, in bounded batches that resume where they
//! stopped. So are torrents stored above the store's high-water id by a
//! build that doesn't keep the marker. It runs in two phases behind the
//! one marker: first the torrents [`rejudge_first`] picks, which the
//! earlier core may have judged differently, then every other. What the
//! core now holds is held as ingest would hold it. A torrent that can't be
//! judged (its row or file list won't read or is bigger than the core
//! reads, or judging it fails) is held as too large to judge, never left
//! shown. Only stored torrents are read: a quarantine row is never judged
//! again from its sealed name here.
//!
//! Known limits: a row an older build rewrote in place while downgraded
//! isn't seen by the high-water id; a batch's totals are written after
//! its holds commit, so a restart between the two undercounts that batch
//! (its held torrents are gone, so they aren't counted again).

use std::time::{Duration, Instant};

use lodestar_classify::{ContentType, FileInfo, file_infos};
use lodestar_filter::reclassify::{Refiling, rejudge_core};
use lodestar_filter::{CORE_VERSION, Decision, Engine, Handle, Item, MAX_FILES, rejudge_first};
use lodestar_store::{CorePass, Error, FilesBound, Store, TorrentRow};

use crate::enrich::Backfill;

/// How long one batch may hold the store: the writer's other work waits
/// meanwhile. The batch stops after the torrent that passes it.
pub const BATCH_TIME: Duration = Duration::from_millis(250);

/// The biggest stored file list the re-judge reads: past it, the torrent
/// is held as too large to judge without decoding the list. Files as the
/// core reads them, and metainfo's own totals for path bytes and
/// segments.
pub const FILES_BOUND: FilesBound = FilesBound {
    files: MAX_FILES,
    path_bytes: 32 << 20,
    segments: 2_000_000,
    raw_bytes: 64 << 20,
};

/// A batch in which more than half the torrents, and more than this many,
/// won't read stops instead of holding them: that's the store failing
/// (a missing dictionary, a damaged page), not the torrents.
pub const BREAKER_ROWS: usize = 16;

/// What [`rejudge_stored`] fails with when the breaker stops a batch.
pub const BREAKER: &str = "too many stored torrents in one batch won't read";

/// One batch of the core re-judge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoreRejudged {
    /// Torrents judged again.
    pub judged: usize,
    /// Torrents left for the other phase.
    pub left: usize,
    /// Swarm hashes of the torrents it held back (dropped or quarantined):
    /// intake must refuse them now.
    pub held: Vec<[u8; 20]>,
    /// The classification rules changed during the batch, so nothing was
    /// written: run it again from the same place.
    pub rejected: bool,
    /// The pass's totals before this batch, and after it.
    pub before: CorePass,
    pub after: CorePass,
    /// The batch ended its phase (`before.phase`).
    pub phase_done: bool,
    /// The pass reached the end: the store is now judged by today's core.
    pub finished: bool,
}

/// Marks an empty store as judged by today's core: what this build stores
/// next, it judges (a fresh `lodestar import`, a fresh server).
pub fn seed_if_empty(store: &mut Store) -> Result<(), Error> {
    store.seed_core_marker(CORE_VERSION)
}

/// Records that this build judged every torrent stored now (the store's
/// high-water id), when no re-judge is under way. Only for a caller that
/// knows nothing was due before it stored ([`is_current`]): the writer
/// once its pass is done, an import that started on a current store.
pub fn note_judged(store: &mut Store) -> Result<(), Error> {
    store.advance_core_seen(CORE_VERSION)
}

/// Whether every stored torrent was judged by today's core: no re-judge
/// is due (one judged by an older core, or stored by an older build).
pub fn is_current(store: &mut Store) -> Result<bool, Error> {
    Ok(store.core_rejudge_due(CORE_VERSION)?.is_none())
}

/// Whether a store error is about one torrent's own rows (they won't
/// decode, or are bigger than [`FILES_BOUND`]), so that torrent can't be
/// judged and is held, rather than the store failing (the batch is tried
/// again). A missing compression dictionary is the store's fault.
fn row_level(e: &Error) -> bool {
    use lodestar_store::rusqlite::Error as Sql;
    match e {
        Error::Compression(e) => e.kind() != std::io::ErrorKind::NotFound,
        Error::Codec(_) | Error::Corrupt(_) | Error::Invalid(_) => true,
        Error::Sqlite(e) => matches!(
            e,
            Sql::FromSqlConversionFailure(..)
                | Sql::InvalidColumnType(..)
                | Sql::IntegralValueOutOfRange(..)
                | Sql::Utf8Error(..)
        ),
        _ => false,
    }
}

/// How one stored torrent's judgment turned out.
enum Row {
    /// No longer stored.
    Gone,
    /// Judged in the other phase.
    Left,
    /// Judged again: what to apply, if anything, and whether it was held
    /// because it couldn't be judged at all.
    Judged(Option<Refiling>, bool),
}

/// Today's core on one stored torrent, as ingest judges it.
pub(crate) type Judge<'a> =
    dyn Fn(&TorrentRow, &[FileInfo], bool, &Engine, i64) -> Option<(Decision, bool)> + 'a;

/// Judges again, by today's core, the stored torrents after where the
/// re-judge to [`CORE_VERSION`] stopped, in its phase: up to `limit`, or
/// as many as [`BATCH_TIME`] allows (at least one). Records the place and
/// totals. At the end of the first phase it starts the second; at the end
/// of the second it records the store as judged by today's core. `None`
/// when there's nothing to do (an empty store is marked so first). `mark`
/// is called with the hashes the batch will hold just before its write,
/// as the backfill does. Fails with [`BREAKER`] rather than hold a batch
/// that mostly won't read.
pub fn rejudge_stored(
    store: &mut Store,
    filter: &Handle,
    limit: usize,
    ctx: &Backfill<'_>,
    mark: &dyn Fn(&Store, &[[u8; 20]]),
) -> Result<Option<CoreRejudged>, Error> {
    rejudge_with(store, filter, limit, ctx, mark, &rejudge_core)
}

pub(crate) fn rejudge_with(
    store: &mut Store,
    filter: &Handle,
    limit: usize,
    ctx: &Backfill<'_>,
    mark: &dyn Fn(&Store, &[[u8; 20]]),
    judge: &Judge<'_>,
) -> Result<Option<CoreRejudged>, Error> {
    seed_if_empty(store)?;
    let Some(before) = store.core_rejudge_due(CORE_VERSION)? else {
        return Ok(None);
    };
    // The generation first, then the rules, as the backfill reads them.
    let generation = store.class_gen()?;
    let engine = filter.current();
    let ids = store.ids_after(before.after, limit.max(1))?;
    if ids.is_empty() {
        let finished = before.phase >= 2;
        if finished {
            store.finish_core_rejudge(CORE_VERSION)?;
        } else {
            // The first phase is done: the second goes over the same ids.
            let next = CorePass {
                phase: 2,
                after: before.start,
                ..before
            };
            store.set_core_rejudge(CORE_VERSION, &next)?;
        }
        return Ok(Some(CoreRejudged {
            before,
            after: before,
            phase_done: true,
            finished,
            ..CoreRejudged::default()
        }));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let started = Instant::now();
    let mut refilings = Vec::new();
    let (mut judged, mut left, mut unjudged) = (0, 0, 0);
    let mut last = before.after;
    for &id in &ids {
        if last != before.after && started.elapsed() >= BATCH_TIME {
            break;
        }
        // The whole of one torrent's judgment, so a panic anywhere in it
        // holds that torrent and the batch goes on.
        let row = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            judge_row(store, id, before.phase, &engine, ctx, now, judge)
        }));
        let row = match row {
            Ok(Ok(row)) => row,
            Ok(Err(e)) if !row_level(&e) => return Err(e),
            // Its rows won't read, or judging it panicked: held under
            // CS-0a by the hashes as stored, and deleted. A row whose
            // hashes won't read either is deleted with no hold, counted
            // as held (it's no longer shown).
            Ok(Err(_)) | Err(_) => {
                let hashes = match store.raw_hashes(id) {
                    Ok(h) => h,
                    Err(e) if row_level(&e) => Vec::new(),
                    Err(e) => return Err(e),
                };
                Row::Judged(
                    Some(Refiling::unread(id, hashes, &engine.unjudgeable())),
                    true,
                )
            }
        };
        last = id;
        match row {
            Row::Gone => {}
            Row::Left => left += 1,
            Row::Judged(refiling, unread) => {
                judged += 1;
                unjudged += usize::from(unread);
                refilings.extend(refiling);
            }
        }
    }
    // Most of a batch unreadable is the store failing, not its torrents:
    // stop, hold nothing, and try again later.
    if unjudged > BREAKER_ROWS && unjudged * 2 > judged + left {
        return Err(Error::Invalid(BREAKER));
    }
    let refiled: Vec<_> = refilings
        .iter()
        .map(|r| r.refiled(now, now + ctx.quarantine_secs))
        .collect();
    let held_back = refilings.iter().filter(|r| r.held.is_some()).count();
    let held: Vec<[u8; 20]> = refilings
        .iter()
        .filter(|r| r.held.is_some())
        .flat_map(|r| r.hashes.iter().copied())
        .collect();
    if !held.is_empty() {
        mark(store, &held);
    }
    // One transaction: a held torrent is deleted with its hold written.
    if store
        .set_enrichments_at(generation, &[], &refiled)?
        .is_none()
    {
        return Ok(Some(CoreRejudged {
            rejected: true,
            before,
            after: before,
            ..CoreRejudged::default()
        }));
    }
    // After the holds commit: a restart between the two judges this batch
    // again, which holds nothing twice (a held torrent is no longer
    // stored).
    let after = CorePass {
        after: last,
        found: before.found + judged as i64,
        held: before.held + held_back as i64,
        unreadable: before.unreadable + unjudged as i64,
        ..before
    };
    store.set_core_rejudge(CORE_VERSION, &after)?;
    Ok(Some(CoreRejudged {
        judged,
        left,
        held,
        rejected: false,
        before,
        after,
        phase_done: false,
        finished: false,
    }))
}

/// Whether the error is the breaker's ([`BREAKER`]).
pub fn is_breaker(e: &Error) -> bool {
    matches!(e, Error::Invalid(m) if *m == BREAKER)
}

/// One stored torrent judged again in `phase`, or left for the other: an
/// error from its own rows is the caller's to hold.
fn judge_row(
    store: &mut Store,
    id: i64,
    phase: i64,
    engine: &Engine,
    ctx: &Backfill<'_>,
    now: i64,
    judge: &Judge<'_>,
) -> Result<Row, Error> {
    let Some(row) = store.torrent(id)? else {
        return Ok(Row::Gone);
    };
    let files = match store.files_bounded(id, &FILES_BOUND)? {
        Some(tree) => file_infos(&tree.files, tree.encoding),
        // No file list: nothing to judge it by.
        None => return Err(Error::Corrupt("no file list")),
    };
    let item = Item {
        hashes: &[],
        name: &row.name,
        files: &files,
        content_type: ContentType::Other,
    };
    // The first phase judges what the earlier core may have judged
    // differently, the second every other torrent.
    if rejudge_first(&item) != (phase == 1) {
        return Ok(Row::Left);
    }
    let own = store
        .correction(id)?
        .map(crate::enrich::correction)
        .unwrap_or_default();
    // Your Is adult correction, or an always-adult source: read as Adult
    // too. A Not adult one changes nothing the core sees.
    let own = crate::source_adult::correction(own, row.sources, ctx.adult);
    let also_adult = own.adult == Some(true);
    let refiling = judge(&row, &files, also_adult, engine, now).map(|(d, adult_hold)| Refiling {
        adult_hold,
        ..Refiling::new(&row, &d, ctx.sealer)
    });
    Ok(Row::Judged(refiling, false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_filter::{Core, CsList, Lists, Sealer, Switches, TermSet};
    use lodestar_store::StoreConfig;

    fn add(store: &mut Store, name: &str) -> (i64, [u8; 20]) {
        use lodestar_bencode::{OwnedValue, to_vec};
        let raw = to_vec(&OwnedValue::Dict(
            [
                (b"length".to_vec(), OwnedValue::Int(1 << 30)),
                (
                    b"name".to_vec(),
                    OwnedValue::Bytes(name.as_bytes().to_vec()),
                ),
                (b"piece length".to_vec(), OwnedValue::Int(1 << 20)),
                (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20])),
            ]
            .into_iter()
            .collect(),
        ));
        let info = lodestar_metainfo::Info::parse(&raw).unwrap();
        let id = store
            .insert(&[lodestar_store::NewTorrent {
                info: &info,
                raw_info: Some(&raw),
                discovered_at: 1,
                metadata_at: 1,
                extra_flags: 0,
                source: lodestar_store::SourceId::new(0).unwrap(),
            }])
            .unwrap()[0]
            .id();
        (id, info.primary_hash().0)
    }

    /// Judging one torrent panics: that torrent is held under CS-0a and
    /// deleted, and the batch goes on to the next, which is judged.
    #[test]
    fn a_panic_judging_one_torrent_holds_it_and_the_batch_goes_on() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let long = |n: &str| format!("{}{n}", "word ".repeat(600));
        let (bad, bad_hash) = add(&mut store, &long("panics here"));
        let (good, _) = add(&mut store, &long("plain film"));
        let filter = Handle::new(Engine::new(Core::new(
            Lists {
                sexual: TermSet::of(&["lexa"]),
                child_roles: TermSet::of(&["rolea"]),
                codes: TermSet::of(&["codex"]),
            },
            CsList::default(),
            Switches::default(),
        )));
        let sealer = Sealer::new(&[9; 32]);
        let ctx = Backfill {
            filter: Some(&filter),
            sealer: Some(&sealer),
            quarantine_secs: 86_400,
            adult: Default::default(),
        };
        let judge = |row: &TorrentRow, files: &[FileInfo], adult, e: &Engine, now| {
            assert!(!row.name.contains("panics"), "judging failed");
            rejudge_core(row, files, adult, e, now)
        };
        let b = rejudge_with(&mut store, &filter, 10, &ctx, &|_, _| {}, &judge)
            .unwrap()
            .unwrap();
        assert_eq!(
            (b.judged, b.held.clone(), b.after.unreadable),
            (2, vec![bad_hash], 1)
        );
        assert!(store.torrent(bad).unwrap().is_none());
        assert!(store.torrent(good).unwrap().is_some());
        assert_eq!(
            store.quarantine_rule(&bad_hash).unwrap().as_deref(),
            Some("CS-0a")
        );
    }
}
