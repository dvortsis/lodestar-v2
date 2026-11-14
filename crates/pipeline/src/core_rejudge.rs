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
//! A batch that mostly won't read (the breaker, [`BREAKER_ROWS`]) holds
//! nothing: its range is kept in the store ([`Stall`]) and the pass goes
//! on past it. Once both phases are done, the kept ranges are judged
//! again, with a wait between tries, and the pass stays unfinished, with
//! the store not marked as judged, until every one has been judged.
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
/// (a damaged dictionary, a damaged page), not the torrents. Its range is
/// kept ([`Stall`]) and the pass moves past it.
pub const BREAKER_ROWS: usize = 16;

/// What [`rejudge_stored`] fails with when the breaker stops a stalled
/// range judged again ([`RETRY_PHASE`]): the caller waits before the next
/// try.
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
    /// Both phases are done and only stalled ranges are left: they're
    /// judged again from time to time, and the pass stays unfinished.
    pub waiting: bool,
    /// The breaker stopped this batch, in a range of its own: it's kept to
    /// judge again, and the pass moved past it. `None` when the batch only
    /// extended a kept range (the pass still moved past it).
    pub stall: Option<Stall>,
    /// A stalled range judged through in this batch, and removed.
    pub cleared: Option<Stall>,
    /// The stalled ranges still to judge after this batch.
    pub stalled: Stalled,
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
/// as the backfill does. A batch that mostly won't read holds nothing:
/// its range is kept ([`Stall`]) and the pass moves on. Once both phases
/// are done, the kept ranges are judged again ([`RETRY_PHASE`]); one the
/// breaker stops again fails with [`BREAKER`]. The pass finishes only
/// once none is left.
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
    let Some(mut before) = store.core_rejudge_due(CORE_VERSION)? else {
        return Ok(None);
    };
    // A pass about to start on an empty store has nothing to judge: mark
    // it instead (only then, not on every batch).
    if before == CorePass::starting(before.start) {
        seed_if_empty(store)?;
        match store.core_rejudge_due(CORE_VERSION)? {
            Some(pass) => before = pass,
            None => return Ok(None),
        }
    }
    let mut stalls = load_stalls(store, before.start)?;
    if before.phase >= RETRY_PHASE {
        return retry_stall(store, filter, limit, ctx, mark, judge, before, stalls);
    }
    // The generation first, then the rules, as the backfill reads them.
    let generation = store.class_gen()?;
    let engine = filter.current();
    let ids = store.ids_after(before.after, limit.max(1))?;
    if ids.is_empty() {
        let next = if before.phase >= 2 {
            // The end of the second phase: done, unless ranges the breaker
            // stopped are still to judge.
            if stalls.is_empty() {
                finish(store)?;
                return Ok(Some(CoreRejudged {
                    before,
                    after: before,
                    phase_done: true,
                    finished: true,
                    ..CoreRejudged::default()
                }));
            }
            CorePass {
                phase: RETRY_PHASE,
                ..before
            }
        } else {
            // The first phase is done: the second goes over the same ids.
            CorePass {
                phase: 2,
                after: before.start,
                ..before
            }
        };
        store.set_core_rejudge(CORE_VERSION, &next)?;
        return Ok(Some(CoreRejudged {
            before,
            after: next,
            phase_done: true,
            waiting: next.phase >= RETRY_PHASE,
            stalled: totals(&stalls),
            ..CoreRejudged::default()
        }));
    }
    let b = judge_ids(store, &ids, before.after, before.phase, &engine, ctx, judge)?;
    if b.tripped() {
        // Most of the batch wouldn't read: hold nothing, and note the range
        // to judge again later, before the pass moves past it, so a restart
        // between the two only notes it again.
        let stall = Stall {
            phase: before.phase,
            after: before.after,
            to: b.last,
            rows: b.rows,
        };
        let new = add_stall(&mut stalls, stall);
        save_stalls(store, &stalls)?;
        let after = CorePass {
            after: b.last,
            ..before
        };
        store.set_core_rejudge(CORE_VERSION, &after)?;
        return Ok(Some(CoreRejudged {
            before,
            after,
            stall: new.then_some(stall),
            stalled: totals(&stalls),
            ..CoreRejudged::default()
        }));
    }
    let Some(applied) = apply(store, generation, &b, ctx, mark)? else {
        return Ok(Some(CoreRejudged {
            rejected: true,
            before,
            after: before,
            stalled: totals(&stalls),
            ..CoreRejudged::default()
        }));
    };
    // After the holds commit: a restart between the two judges this batch
    // again, which holds nothing twice (a held torrent is no longer
    // stored).
    let after = CorePass {
        after: b.last,
        ..counted(before, &b, applied.held_back)
    };
    store.set_core_rejudge(CORE_VERSION, &after)?;
    Ok(Some(CoreRejudged {
        judged: b.judged,
        left: b.left,
        held: applied.held,
        before,
        after,
        stalled: totals(&stalls),
        ..CoreRejudged::default()
    }))
}

/// The pass's phase once both phases are done and only ranges the breaker
/// stopped are left ([`Stall`]): it's judged again from them, the first
/// first, and the pass stays unfinished until none is left.
pub const RETRY_PHASE: i64 = 3;

/// The settings key the ranges the breaker stopped are kept under, with
/// the core version they're for: `v<version>;<phase>:<after>:<to>:<rows>;…`.
/// Ids and counts only. Not one of the store's pass keys, so it's written
/// here and removed before the pass finishes.
pub const STALLS_KEY: &str = "filter.core.stalled";

/// At most this many ranges are kept. Past it, a new range widens the
/// last of its phase to cover it: a wider range only judges again
/// torrents judged already, which changes nothing.
pub const MAX_STALLS: usize = 1024;

/// A range of stored torrents the breaker stopped: the ids after `after`,
/// up to and including `to`, in `phase`. Nothing in it was held or
/// written; it's judged again until it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stall {
    pub phase: i64,
    pub after: i64,
    pub to: i64,
    /// The stored torrents it covered when it stopped (a range that
    /// shrinks as it's judged again loses those judged).
    pub rows: u64,
}

/// Ranges the breaker stopped that are still to judge, and their rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stalled {
    pub ranges: u64,
    pub rows: u64,
}

fn totals(stalls: &[Stall]) -> Stalled {
    Stalled {
        ranges: stalls.len() as u64,
        rows: stalls.iter().fold(0, |sum, s| sum.saturating_add(s.rows)),
    }
}

/// The ranges still to judge again ([`Stall`]), and their rows: for a
/// status read outside the writer.
pub fn stalled(store: &mut Store) -> Result<Stalled, Error> {
    let start = match store.core_rejudge_due(CORE_VERSION)? {
        Some(pass) => pass.start,
        None => return Ok(Stalled::default()),
    };
    Ok(totals(&load_stalls(store, start)?))
}

/// The stalled ranges kept for today's core. Ranges kept for another core
/// are for a pass that's gone (a new one judges every torrent again). A
/// value that won't read is taken as every torrent of the pass, in both
/// phases, so nothing it named is ever left unjudged.
fn load_stalls(store: &mut Store, start: i64) -> Result<Vec<Stall>, Error> {
    let text = match store.setting_text(STALLS_KEY) {
        Ok(Some(text)) => text,
        Ok(None) => return Ok(Vec::new()),
        Err(e) if row_level(&e) => return Ok(everything(start)),
        Err(e) => return Err(e),
    };
    match parse_stalls(&text) {
        Some((version, stalls)) if version == CORE_VERSION => Ok(stalls),
        Some(_) => Ok(Vec::new()),
        None => Ok(everything(start)),
    }
}

fn everything(start: i64) -> Vec<Stall> {
    (1..=2)
        .map(|phase| Stall {
            phase,
            after: start,
            to: i64::MAX,
            rows: 0,
        })
        .collect()
}

fn parse_stalls(text: &str) -> Option<(i64, Vec<Stall>)> {
    let mut parts = text.split(';');
    let version = parts.next()?.strip_prefix('v')?.parse().ok()?;
    let stalls = parts
        .map(|part| {
            let mut f = part.split(':');
            let stall = Stall {
                phase: f.next()?.parse().ok()?,
                after: f.next()?.parse().ok()?,
                to: f.next()?.parse().ok()?,
                rows: f.next()?.parse().ok()?,
            };
            (f.next().is_none() && (1..=2).contains(&stall.phase) && stall.after < stall.to)
                .then_some(stall)
        })
        .collect::<Option<Vec<_>>>()?;
    Some((version, stalls))
}

fn format_stalls(stalls: &[Stall]) -> String {
    let mut text = format!("v{CORE_VERSION}");
    for s in stalls {
        text.push_str(&format!(";{}:{}:{}:{}", s.phase, s.after, s.to, s.rows));
    }
    text
}

fn save_stalls(store: &mut Store, stalls: &[Stall]) -> Result<(), Error> {
    if stalls.is_empty() {
        store.update_settings_text(&[], &[STALLS_KEY])
    } else {
        store.update_settings_text(&[(STALLS_KEY, &format_stalls(stalls))], &[])
    }
}

/// Adds `new` to the ranges: the same range again (a restart before the
/// pass moved past it) is kept once, one that follows a range of its
/// phase extends it, and past [`MAX_STALLS`] it widens the last of its
/// phase. Returns whether it's a range of its own, so a run of batches
/// the breaker stops is logged once, not once a batch.
fn add_stall(stalls: &mut Vec<Stall>, new: Stall) -> bool {
    let same_phase = |s: &&mut Stall| s.phase == new.phase;
    if stalls
        .iter()
        .any(|s| s.phase == new.phase && s.after <= new.after && new.to <= s.to)
    {
        return false;
    }
    if let Some(s) = stalls
        .iter_mut()
        .filter(same_phase)
        .find(|s| s.to == new.after)
    {
        s.to = new.to;
        s.rows = s.rows.saturating_add(new.rows);
        return false;
    }
    if stalls.len() >= MAX_STALLS
        && let Some(s) = stalls.iter_mut().rev().find(|s| s.phase == new.phase)
    {
        s.after = s.after.min(new.after);
        s.to = s.to.max(new.to);
        s.rows = s.rows.saturating_add(new.rows);
        return false;
    }
    stalls.push(new);
    true
}

/// Records the store as judged by today's core, the stalled ranges' key
/// first: a restart between the two finds no range left and finishes.
fn finish(store: &mut Store) -> Result<(), Error> {
    store.update_settings_text(&[], &[STALLS_KEY])?;
    store.finish_core_rejudge(CORE_VERSION)
}

/// One batch from the first stalled range, in that range's phase. The
/// breaker stops it again: the range goes to the back of the list, so the
/// next try starts with another, and this fails with [`BREAKER`] for the
/// caller to wait. Once the range is judged through, it's removed; once
/// none is left, the pass finishes.
#[allow(
    clippy::too_many_arguments,
    reason = "the batch's inputs, as rejudge_with's"
)]
fn retry_stall(
    store: &mut Store,
    filter: &Handle,
    limit: usize,
    ctx: &Backfill<'_>,
    mark: &dyn Fn(&Store, &[[u8; 20]]),
    judge: &Judge<'_>,
    before: CorePass,
    mut stalls: Vec<Stall>,
) -> Result<Option<CoreRejudged>, Error> {
    let Some(&stall) = stalls.first() else {
        finish(store)?;
        return Ok(Some(CoreRejudged {
            before,
            after: before,
            phase_done: true,
            finished: true,
            ..CoreRejudged::default()
        }));
    };
    let generation = store.class_gen()?;
    let engine = filter.current();
    let limit = limit.max(1);
    let listed = store.ids_after(stall.after, limit)?;
    let ids: Vec<i64> = listed
        .iter()
        .copied()
        .filter(|&id| id <= stall.to)
        .collect();
    let rest = |stalls: &mut Vec<Stall>| stalls.remove(0);
    if ids.is_empty() {
        // Nothing left in it (judged, or no longer stored).
        rest(&mut stalls);
        save_stalls(store, &stalls)?;
        return Ok(Some(CoreRejudged {
            before,
            after: before,
            waiting: true,
            cleared: Some(stall),
            stalled: totals(&stalls),
            ..CoreRejudged::default()
        }));
    }
    let b = judge_ids(store, &ids, stall.after, stall.phase, &engine, ctx, judge)?;
    if b.tripped() {
        let s = rest(&mut stalls);
        stalls.push(s);
        save_stalls(store, &stalls)?;
        return Err(Error::Invalid(BREAKER));
    }
    let Some(applied) = apply(store, generation, &b, ctx, mark)? else {
        return Ok(Some(CoreRejudged {
            rejected: true,
            before,
            after: before,
            waiting: true,
            stalled: totals(&stalls),
            ..CoreRejudged::default()
        }));
    };
    // Judged through when the batch reached the range's end: its last id,
    // or every id listed and none past them inside it.
    let through = b.last >= stall.to
        || (b.last == *ids.last().expect("not empty")
            && (listed.len() < limit || listed.len() > ids.len()));
    let cleared = if through {
        rest(&mut stalls);
        Some(stall)
    } else {
        let s = &mut stalls[0];
        s.after = b.last;
        s.rows = s.rows.saturating_sub(b.rows);
        None
    };
    save_stalls(store, &stalls)?;
    let after = counted(before, &b, applied.held_back);
    store.set_core_rejudge(CORE_VERSION, &after)?;
    Ok(Some(CoreRejudged {
        judged: b.judged,
        left: b.left,
        held: applied.held,
        before,
        after,
        waiting: true,
        cleared,
        stalled: totals(&stalls),
        ..CoreRejudged::default()
    }))
}

/// One batch's judgments, before anything is written.
struct Batch {
    refilings: Vec<Refiling>,
    judged: usize,
    left: usize,
    unjudged: usize,
    /// The last id it reached, and how many ids it reached.
    last: i64,
    rows: u64,
}

impl Batch {
    /// Most of the batch wouldn't read: the store failing, not its
    /// torrents.
    fn tripped(&self) -> bool {
        self.unjudged > BREAKER_ROWS && self.unjudged * 2 > self.judged + self.left
    }
}

/// Judges `ids` (after `from`) in `phase`, as many as [`BATCH_TIME`]
/// allows (at least one).
fn judge_ids(
    store: &mut Store,
    ids: &[i64],
    from: i64,
    phase: i64,
    engine: &Engine,
    ctx: &Backfill<'_>,
    judge: &Judge<'_>,
) -> Result<Batch, Error> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let started = Instant::now();
    let mut b = Batch {
        refilings: Vec::new(),
        judged: 0,
        left: 0,
        unjudged: 0,
        last: from,
        rows: 0,
    };
    for &id in ids {
        if b.rows > 0 && started.elapsed() >= BATCH_TIME {
            break;
        }
        // The whole of one torrent's judgment, so a panic anywhere in it
        // holds that torrent and the batch goes on.
        let row = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            judge_row(store, id, phase, engine, ctx, now, judge)
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
        b.last = id;
        b.rows += 1;
        match row {
            Row::Gone => {}
            Row::Left => b.left += 1,
            Row::Judged(refiling, unread) => {
                b.judged += 1;
                b.unjudged += usize::from(unread);
                b.refilings.extend(refiling);
            }
        }
    }
    Ok(b)
}

/// What a batch's write held.
struct Applied {
    held: Vec<[u8; 20]>,
    held_back: usize,
}

/// Writes a batch's refilings in one transaction (a held torrent is
/// deleted with its hold written), marking the hashes it holds for intake
/// first. `None` when the rules changed under it, so nothing was written.
fn apply(
    store: &mut Store,
    generation: i64,
    b: &Batch,
    ctx: &Backfill<'_>,
    mark: &dyn Fn(&Store, &[[u8; 20]]),
) -> Result<Option<Applied>, Error> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let refiled: Vec<_> = b
        .refilings
        .iter()
        .map(|r| r.refiled(now, now + ctx.quarantine_secs))
        .collect();
    let held_back = b.refilings.iter().filter(|r| r.held.is_some()).count();
    let held: Vec<[u8; 20]> = b
        .refilings
        .iter()
        .filter(|r| r.held.is_some())
        .flat_map(|r| r.hashes.iter().copied())
        .collect();
    if !held.is_empty() {
        mark(store, &held);
    }
    if store
        .set_enrichments_at(generation, &[], &refiled)?
        .is_none()
    {
        return Ok(None);
    }
    Ok(Some(Applied { held, held_back }))
}

/// The pass's totals with a written batch counted.
fn counted(before: CorePass, b: &Batch, held_back: usize) -> CorePass {
    CorePass {
        found: before.found + b.judged as i64,
        held: before.held + held_back as i64,
        unreadable: before.unreadable + b.unjudged as i64,
        ..before
    }
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

    fn stall(phase: i64, after: i64, to: i64, rows: u64) -> Stall {
        Stall {
            phase,
            after,
            to,
            rows,
        }
    }

    #[test]
    fn stalled_ranges_round_trip_and_bad_text_is_refused() {
        let stalls = vec![stall(1, 0, 32, 32), stall(2, 40, 90, 17)];
        let text = format_stalls(&stalls);
        assert_eq!(parse_stalls(&text), Some((CORE_VERSION, stalls)));
        assert_eq!(
            parse_stalls(&format_stalls(&[])),
            Some((CORE_VERSION, vec![]))
        );
        for bad in [
            "",
            "x1;1:0:2:2",
            "v1;1:0:2",
            "v1;1:0:2:2:9",
            "v1;3:0:2:2",
            "v1;1:5:5:0",
            "v1;1:a:2:2",
            "v1;;",
        ] {
            assert_eq!(parse_stalls(bad), None, "{bad}");
        }
    }

    /// Text that won't read is taken as every torrent of the pass, in
    /// both phases; ranges kept for another core are for a pass that's
    /// gone.
    #[test]
    fn unreadable_or_other_cores_stalls() {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        store
            .update_settings_text(&[(STALLS_KEY, "not ranges")], &[])
            .unwrap();
        assert_eq!(
            load_stalls(&mut store, 7).unwrap(),
            [stall(1, 7, i64::MAX, 0), stall(2, 7, i64::MAX, 0)]
        );
        let other = format!("v{};1:0:9:9", CORE_VERSION + 1);
        store
            .update_settings_text(&[(STALLS_KEY, &other)], &[])
            .unwrap();
        assert_eq!(load_stalls(&mut store, 7).unwrap(), []);
        save_stalls(&mut store, &[stall(1, 0, 9, 9)]).unwrap();
        assert_eq!(load_stalls(&mut store, 7).unwrap(), [stall(1, 0, 9, 9)]);
        save_stalls(&mut store, &[]).unwrap();
        assert_eq!(store.setting_text(STALLS_KEY).unwrap(), None);
    }

    /// The same range again is kept once, one that follows a range of its
    /// phase extends it, and past the cap a new range widens the last of
    /// its phase.
    #[test]
    fn stalled_ranges_merge() {
        let mut stalls = Vec::new();
        assert!(add_stall(&mut stalls, stall(1, 0, 32, 32)), "new");
        assert!(!add_stall(&mut stalls, stall(1, 0, 32, 32)), "the same");
        assert!(!add_stall(&mut stalls, stall(1, 8, 16, 8)), "inside");
        assert_eq!(stalls, [stall(1, 0, 32, 32)]);
        assert!(!add_stall(&mut stalls, stall(1, 32, 64, 30)), "extends");
        assert_eq!(stalls, [stall(1, 0, 64, 62)]);
        assert!(add_stall(&mut stalls, stall(2, 32, 64, 30)), "other phase");
        assert!(add_stall(&mut stalls, stall(1, 100, 120, 5)), "apart");
        // Rows never overflow, whatever a kept value says.
        stalls[0].rows = u64::MAX;
        assert!(!add_stall(&mut stalls, stall(1, 64, 70, 6)));
        assert_eq!(stalls[0].rows, u64::MAX);
        assert_eq!(totals(&stalls).rows, u64::MAX);
        stalls[0].rows = 62;
        assert_eq!(stalls.len(), 3);
        let mut full: Vec<Stall> = (0..MAX_STALLS as i64)
            .map(|i| stall(1 + i % 2, i * 10, i * 10 + 5, 1))
            .collect();
        assert!(
            !add_stall(&mut full, stall(1, 1_000_000, 1_000_010, 4)),
            "widens"
        );
        assert_eq!(full.len(), MAX_STALLS);
        let last = full.iter().rfind(|s| s.phase == 1).unwrap();
        assert_eq!((last.to, last.rows), (1_000_010, 5));
        assert_eq!(totals(&full).rows, MAX_STALLS as u64 + 4);
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
