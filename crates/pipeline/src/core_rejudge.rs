//! The core re-judge (§20.5): once the core changes (its
//! [`CORE_VERSION`] goes up), every stored torrent judged by an older one
//! is judged again by today's, in bounded batches that resume where they
//! stopped. A torrent the earlier core read whole ([`read_whole_by_v1`])
//! keeps its judgment: only torrents it may have judged in part are
//! judged again. What the core now holds is held as ingest would hold it; a
//! torrent that can't be judged (its file list won't read, or judging it
//! fails) is held as too large to judge, never left shown. Only stored
//! torrents are read: a quarantine row is never judged again from its
//! sealed name here.

use lodestar_classify::{ContentType, file_infos};
use lodestar_filter::reclassify::{Refiling, rejudge_core};
use lodestar_filter::{CORE_VERSION, Handle, Item, read_whole_by_v1};
use lodestar_store::{CorePass, Error, Store};

use crate::enrich::Backfill;

/// One batch of the core re-judge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoreRejudged {
    /// Torrents judged again.
    pub judged: usize,
    /// Torrents the earlier core read whole, so its judgment stands.
    pub stood: usize,
    /// Swarm hashes of the torrents it held back (dropped or quarantined):
    /// intake must refuse them now.
    pub held: Vec<[u8; 20]>,
    /// The classification rules changed during the batch, so nothing was
    /// written: run it again from the same place.
    pub rejected: bool,
    /// The pass's totals before this batch, and after it.
    pub before: CorePass,
    pub after: CorePass,
    /// The pass reached the end: the store is now judged by today's core.
    pub finished: bool,
}

/// Whether a store error means this torrent's own rows won't read, so it
/// can't be judged (held), rather than that the store failed (the batch
/// is tried again).
fn unreadable(e: &Error) -> bool {
    matches!(
        e,
        Error::Codec(_) | Error::Compression(_) | Error::Corrupt(_)
    )
}

/// Judges again, by today's core, up to `limit` stored torrents after
/// where the re-judge to [`CORE_VERSION`] stopped, and records the place
/// and totals. When none are left, records the store as judged by it
/// (`finished`). `None` when there's nothing to do (judged already, or by
/// a newer core). `mark` is
/// called with the hashes the batch will hold just before its write, as
/// the backfill does.
pub fn rejudge_stored(
    store: &mut Store,
    filter: &Handle,
    limit: usize,
    ctx: &Backfill<'_>,
    mark: &dyn Fn(&Store, &[[u8; 20]]),
) -> Result<Option<CoreRejudged>, Error> {
    let Some(before) = store.core_rejudge_due(CORE_VERSION)? else {
        return Ok(None);
    };
    // The generation first, then the rules, as the backfill reads them.
    let generation = store.class_gen()?;
    let engine = filter.current();
    let ids = store.ids_after(before.after, limit)?;
    let Some(&last) = ids.last() else {
        store.finish_core_rejudge(CORE_VERSION)?;
        return Ok(Some(CoreRejudged {
            before,
            after: before,
            finished: true,
            ..CoreRejudged::default()
        }));
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let mut refilings = Vec::new();
    let (mut judged, mut stood, mut unjudged) = (0, 0, 0);
    for &id in &ids {
        let Some(row) = store.torrent(id)? else {
            continue;
        };
        let files = match store.files(id) {
            Ok(Some(tree)) => Some(file_infos(&tree.files, tree.encoding)),
            Ok(None) => None,
            Err(e) if unreadable(&e) => None,
            Err(e) => return Err(e),
        };
        // The earlier core read it whole: its judgment stands, and only
        // the marker moves on. A check that panics reads as not whole.
        if let Some(files) = &files {
            let item = Item {
                hashes: &[],
                name: &row.name,
                files,
                content_type: ContentType::Other,
            };
            let whole =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read_whole_by_v1(&item)));
            if whole.unwrap_or(false) {
                stood += 1;
                continue;
            }
        }
        judged += 1;
        let own = store
            .correction(id)?
            .map(crate::enrich::correction)
            .unwrap_or_default();
        // Your Is adult correction, or an always-adult source: read as
        // Adult too. A Not adult one changes nothing the core sees.
        let own = crate::source_adult::correction(own, row.sources, ctx.adult);
        let also_adult = own.adult == Some(true);
        let decision = match files {
            // A panic judging one torrent holds it, as one that can't be
            // judged: the batch goes on, and nothing is left shown.
            Some(files) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                rejudge_core(&row, &files, also_adult, &engine, now)
            }))
            .unwrap_or_else(|_| {
                unjudged += 1;
                Some((engine.unjudgeable(), false))
            }),
            None => {
                unjudged += 1;
                Some((engine.unjudgeable(), false))
            }
        };
        if let Some((d, adult_hold)) = decision {
            refilings.push(Refiling {
                adult_hold,
                ..Refiling::new(&row, &d, ctx.sealer)
            });
        }
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
    };
    store.set_core_rejudge(CORE_VERSION, &after)?;
    Ok(Some(CoreRejudged {
        judged,
        stood,
        held,
        rejected: false,
        before,
        after,
        finished: false,
    }))
}
