//! Undoing what stand-ins did once their rows read again
//! (docs/decisions/2026-11-02-F-filter-standin-undo.md).
//!
//! A stored filter row that can't be read fails closed with a stand-in
//! (`lodestar_filter::load`). The rows that did are recorded across
//! restarts ([`StandIns`]). Once one no longer fails closed, every hold
//! its stand-in may have made is re-judged by the engine as it is now, and
//! only those it wouldn't hold are released. A released hash is judged in
//! full, files included, when intake fetches it again. Nothing is
//! released on the stand-in's say, and nothing past what the rules you
//! wrote would decide, so it doesn't ask for the password (§20.11). It's
//! surfaced instead: an Insights notice with the counts, and a log event.

use std::sync::{Mutex, PoisonError};

use lodestar_filter::load::{STAND_INS_KEY, StandIns};
use lodestar_store::{Held, HoldRow, Store};

use super::{FiltersApi, Pending, set_pending_requeues, stopping};
use crate::error::{ApiResult, internal};

/// The `settings` key of the last re-judge, for Insights:
/// `{at} {released} {kept}`.
pub(crate) const STAND_IN_UNDO_KEY: &str = "filter.stand_in_undo";
/// How long Insights shows the last re-judge.
pub(crate) const NOTICE_SECS: i64 = 7 * 86_400;
/// Held rows re-judged per store lock.
const CHUNK: usize = 512;

/// One re-judge at a time: the start's and a change's could overlap.
static ONE: Mutex<()> = Mutex::new(());

/// What the last re-judge did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rejudged {
    pub at: i64,
    /// Hashes released: nothing holds them now.
    pub released: usize,
    /// Holds the rules as they are still make.
    pub kept: usize,
}

fn read_record(store: &Store) -> ApiResult<(Option<String>, StandIns)> {
    let text = store.setting_text(STAND_INS_KEY).map_err(internal)?;
    // A record that doesn't read names nothing to release: it's started
    // afresh from this load, which only keeps more held.
    let record = text
        .as_deref()
        .and_then(StandIns::decode)
        .unwrap_or_default();
    Ok((text, record))
}

fn write_record(store: &mut Store, old: Option<&str>, record: &StandIns) -> ApiResult<()> {
    let text = record.encode();
    if old == Some(text.as_str()) || (old.is_none() && record.0.is_empty()) {
        return Ok(());
    }
    if record.0.is_empty() {
        store.update_settings_text(&[], &[STAND_INS_KEY])
    } else {
        store.set_settings_text(&[(STAND_INS_KEY, &text)])
    }
    .map_err(internal)
}

impl FiltersApi {
    /// Records the rows the engine's last load found failing closed, and
    /// re-judges the holds of every recorded row that no longer does. Run
    /// at start and after each filter change, once the engine is reloaded
    /// (§20.13: the engine first, then releases). Only the shutdown stops
    /// it; what's left stays recorded and runs at the next start. Returns
    /// the counts when there was anything to re-judge.
    pub fn undo_stand_ins(&self, now: i64) -> ApiResult<Option<Rejudged>> {
        let _one = ONE.lock().unwrap_or_else(PoisonError::into_inner);
        let recovered = {
            let mut store = self.store();
            let (text, before) = read_record(&store)?;
            let (record, recovered) = before.reconcile(&self.engine.current().unreadable);
            // Before the record lets the entry go: a stop after that
            // still classifies everything again at the next start.
            if recovered
                .iter()
                .any(|(l, _)| StandIns::scope(l) == Some(None))
            {
                set_pending_requeues(&mut store, &Pending::All)?;
            }
            write_record(&mut store, text.as_deref(), &record)?;
            recovered
        };
        if recovered.is_empty() {
            return Ok(None);
        }
        let mut done = Rejudged {
            at: now,
            released: 0,
            kept: 0,
        };
        let mut class = false;
        for (label, since) in &recovered {
            let Some(rule) = StandIns::scope(label) else {
                continue;
            };
            class |= rule.is_none();
            for table in [Held::Quarantine, Held::Tombstone] {
                self.rejudge(table, rule, *since, now, &mut done)?;
            }
            let mut store = self.store();
            let (text, record) = read_record(&store)?;
            write_record(&mut store, text.as_deref(), &record.without(label, *since))?;
        }
        if class {
            // The Adult labels its stand-in gave stored torrents: every
            // one is classified again. Without a search index it stays
            // pending for the first start with one.
            let _ = self.requeue_all();
        }
        let text = format!("{} {} {}", done.at, done.released, done.kept);
        self.store()
            .set_settings_text(&[(STAND_IN_UNDO_KEY, &text)])
            .map_err(internal)?;
        lodestar_log::log_event!(
            crate::log_events::FILTERS_STAND_INS_REJUDGED,
            released = done.released as u64,
            kept = done.kept as u64
        );
        Ok(Some(done))
    }

    /// Re-judges, a chunk per store lock, the holds in `table` a stand-in
    /// may have made: `rule`'s (or every adult hold) from `since` on. The
    /// engine can't change while the lock is held (changes reload it
    /// under the same lock), and a row a writer replaced meanwhile isn't
    /// released ([`Store::release_rows`]).
    fn rejudge(
        &self,
        table: Held,
        rule: Option<&str>,
        since: i64,
        now: i64,
        done: &mut Rejudged,
    ) -> ApiResult<()> {
        let mut after = None;
        loop {
            if self.shutdown.is_stopped() {
                return Err(stopping());
            }
            let mut store = self.store();
            let rows = store
                .holds_since(table, rule, since, after, CHUNK)
                .map_err(internal)?;
            let Some(last) = rows.last() else {
                return Ok(());
            };
            after = Some((last.at, last.hash));
            let full = rows.len() == CHUNK;
            let engine = self.engine.current();
            let (free, kept): (Vec<HoldRow>, Vec<HoldRow>) = rows.into_iter().partition(|r| {
                let name = self
                    .sealer
                    .as_deref()
                    .zip(r.sealed.as_deref())
                    .and_then(|(s, sealed)| s.open(sealed).ok())
                    .unwrap_or_default();
                !engine.holds(&r.hash, &name, now)
            });
            done.kept += kept.len();
            let freed = store.release_rows(&free).map_err(internal)?;
            done.released += freed.len();
            self.unhold(&store, &freed)?;
            if !full {
                return Ok(());
            }
        }
    }
}

/// The last re-judge, while Insights shows it. A value that doesn't read
/// shows nothing.
pub fn stand_in_notice(store: &Store, now: i64) -> Option<Rejudged> {
    let text = store.setting_text(STAND_IN_UNDO_KEY).ok().flatten()?;
    let mut it = text.split(' ').map(str::parse::<i64>);
    let (Some(Ok(at)), Some(Ok(r)), Some(Ok(k))) = (it.next(), it.next(), it.next()) else {
        return None;
    };
    let r = Rejudged {
        at,
        released: usize::try_from(r).ok()?,
        kept: usize::try_from(k).ok()?,
    };
    (now - r.at < NOTICE_SECS).then_some(r)
}
