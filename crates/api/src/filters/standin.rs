//! Undoing what stand-ins did once their rows read again
//! (docs/decisions/2026-11-02-F-filter-standin-undo.md).
//!
//! A stored filter row that can't be read fails closed with a stand-in
//! (`lodestar_filter::load`). The rows that did are recorded across
//! restarts ([`StandIns`]). Once a rule's row no longer fails closed, the
//! quarantine its stand-in made is re-judged by the engine as it is now,
//! and only what it wouldn't hold is released; a released hash is judged
//! in full again when intake fetches it. What a rule that lost its
//! `unless` dropped or hid is counted for review and left as it is. Adult holds made while an Is
//! adult classification rule couldn't be read stay held: a hold your own
//! correction made looks the same, and nothing could make it again. They
//! are counted for the review queue, and stored torrents are classified
//! again. Nothing is released past what the rules you wrote decide, so it
//! doesn't ask for the password (§20.11); it's surfaced instead, as an
//! Insights notice with the counts and a log event.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use lodestar_filter::load::{STAND_INS_KEY, StandIn, StandIns};
use lodestar_search::parse;
use lodestar_store::{HoldRow, Store};

use super::{Action, FiltersApi, Pending, set_pending_requeues, stopping};
use crate::error::{ApiResult, internal};

/// The `settings` key of the last re-judge, for Insights:
/// `{at} {released} {kept} {adult} {dropped} {hidden} {partial}`.
pub(crate) const STAND_IN_UNDO_KEY: &str = "filter.stand_in_undo";
/// How long Insights shows the last re-judge.
pub(crate) const NOTICE_SECS: i64 = 7 * 86_400;
/// Quarantine rows re-judged per store lock.
const CHUNK: usize = 512;

/// One re-judge at a time: the start's and a change's could overlap.
static ONE: Mutex<()> = Mutex::new(());

/// What the last re-judge did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rejudged {
    pub at: i64,
    /// Hashes released: nothing holds them now. A run that follows one a
    /// stop cut short adds to its count.
    pub released: usize,
    /// Holds the rules as they are still make, or that couldn't be
    /// judged (a sealed name that won't open).
    pub kept: usize,
    /// Adult holds made while an Is adult rule couldn't be read, left for
    /// you to review: some may be other rules' or your corrections'.
    pub adult: usize,
    /// Torrents a rule whose `unless` couldn't be read dropped (it keeps
    /// its own action), left for you to review.
    pub dropped: usize,
    /// Torrents such a rule hid, as `filter_hits` still records them.
    pub hidden: usize,
    /// A stop or an error cut it short; the rest runs next time.
    pub partial: bool,
}

impl Rejudged {
    fn any(&self) -> bool {
        self.released + self.kept + self.adult + self.dropped + self.hidden > 0
    }

    fn encode(&self) -> String {
        format!(
            "{} {} {} {} {} {} {}",
            self.at,
            self.released,
            self.kept,
            self.adult,
            self.dropped,
            self.hidden,
            u8::from(self.partial)
        )
    }

    fn decode(text: &str) -> Option<Rejudged> {
        let n: Vec<i64> = text
            .split(' ')
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        let [at, released, kept, adult, dropped, hidden, partial] = n[..] else {
            return None;
        };
        let count = |v: i64| usize::try_from(v).ok();
        Some(Rejudged {
            at,
            released: count(released)?,
            kept: count(kept)?,
            adult: count(adult)?,
            dropped: count(dropped)?,
            hidden: count(hidden)?,
            partial: partial != 0,
        })
    }
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

/// Whether rule `label`'s stored row reads now, or is gone: checked in
/// the store, so an engine that hasn't loaded the rules never counts as
/// one that reads them.
fn rule_reads(store: &Store, label: &str) -> ApiResult<bool> {
    let id: i64 = label[2..].parse().map_err(internal)?;
    let set = store.rule_set().map_err(internal)?;
    Ok(set.rules.iter().find(|r| r.id == id).is_none_or(|r| {
        parse(&r.query).is_ok()
            && Action::from_code(r.action).is_some()
            && r.unless_query.as_deref().is_none_or(|u| parse(u).is_ok())
    }))
}

impl FiltersApi {
    /// [`FiltersApi::undo_stand_ins`] on a blocking task, after a change's
    /// request: the request doesn't wait for it. `requeued_all` when the
    /// change already queued every stored torrent to be classified again.
    pub(crate) fn undo_in_background(self: &Arc<Self>, requeued_all: bool) {
        let f = self.clone();
        tokio::task::spawn_blocking(move || {
            // A failure is logged where it happens, and leaves the rows
            // recorded for the next change or start.
            let _ = f.undo_stand_ins(crate::unix_now(), requeued_all);
        });
    }

    /// Records the rows the engine's last load found failing closed, and
    /// re-judges what the stand-in of every recorded row that no longer
    /// does made. Run at start and after each filter change, once the
    /// engine is reloaded (§20.13: the engine first, then releases). Only
    /// the shutdown stops it; what's left stays recorded and runs at the
    /// next start. What it did is surfaced even when it stops part-way.
    /// `requeued_all` skips classifying every stored torrent again when
    /// the caller just queued that. Returns the counts when there was
    /// anything to re-judge.
    pub fn undo_stand_ins(&self, now: i64, requeued_all: bool) -> ApiResult<Option<Rejudged>> {
        let _one = ONE.lock().unwrap_or_else(PoisonError::into_inner);
        let mut done = Rejudged {
            at: now,
            ..Rejudged::default()
        };
        let result = self.undo_recorded(now, requeued_all, &mut done);
        done.partial = result.is_err();
        if done.any() {
            let mut store = self.store();
            let before = store
                .setting_text(STAND_IN_UNDO_KEY)
                .map_err(internal)?
                .as_deref()
                .and_then(Rejudged::decode);
            // What a run cut short released is released for good; the
            // rest it counted is counted again by this one.
            if let Some(b) = before.filter(|b| b.partial && now - b.at < NOTICE_SECS) {
                done.released += b.released;
            }
            store
                .set_settings_text(&[(STAND_IN_UNDO_KEY, &done.encode())])
                .map_err(internal)?;
            drop(store);
            lodestar_log::log_event!(
                crate::log_events::FILTERS_STAND_INS_REJUDGED,
                released = done.released as u64,
                kept = done.kept as u64,
                review = (done.adult + done.dropped + done.hidden) as u64
            );
        }
        result?;
        Ok(done.any().then_some(done))
    }

    fn undo_recorded(&self, now: i64, requeued_all: bool, done: &mut Rejudged) -> ApiResult<()> {
        let recovered = {
            let mut store = self.store();
            let (text, before) = read_record(&store)?;
            let (mut record, mut recovered) =
                before.reconcile(&self.engine.current().unreadable, now);
            // A rule only once its row really reads: otherwise it's kept
            // as it was, still failing closed.
            let mut still = Vec::new();
            for e in &recovered {
                if StandIns::rule(&e.label).is_some() && !rule_reads(&store, &e.label)? {
                    still.push(e.clone());
                }
            }
            for e in &still {
                record = record.without(e);
                record.0.push(StandIn {
                    until: None,
                    ..e.clone()
                });
                recovered.retain(|r| r != e);
            }
            // Before the record lets a classification entry go: a stop
            // after that still classifies everything again next start.
            if !requeued_all && recovered.iter().any(|e| e.label.starts_with("C-")) {
                set_pending_requeues(&mut store, &Pending::All)?;
            }
            write_record(&mut store, text.as_deref(), &record)?;
            recovered
        };
        let mut class = false;
        let mut adult: HashSet<[u8; 20]> = HashSet::new();
        for e in &recovered {
            let window = (e.since, e.until.unwrap_or(now));
            if let Some(rule) = StandIns::rule(&e.label) {
                self.rejudge(rule, window, now, done)?;
                // A rule that lost its `unless` keeps its own action: what
                // it dropped or hid isn't held in quarantine to re-judge.
                let store = self.store();
                done.dropped += store.tombstones_between(rule, window).map_err(internal)?;
                done.hidden += store
                    .hits_between(rule, Action::Hide.code(), window)
                    .map_err(internal)?;
            } else {
                class = true;
                // One count over every window: a hold in two is one.
                adult.extend(self.store().adult_holds_between(window).map_err(internal)?);
                done.adult = adult.len();
            }
            let mut store = self.store();
            let (text, record) = read_record(&store)?;
            write_record(&mut store, text.as_deref(), &record.without(e))?;
        }
        if class && !requeued_all {
            // The Adult labels its stand-in gave stored torrents: every
            // one is classified again, your corrections included. Without
            // a search index it stays pending for the first start with one.
            let _ = self.requeue_all();
        }
        Ok(())
    }

    /// Re-judges, a chunk per store lock, `rule`'s quarantine made in
    /// `window`. The engine can't change while the lock is held (changes
    /// reload it under the same lock), and a row a writer replaced
    /// meanwhile isn't released ([`Store::release_rows`]).
    fn rejudge(
        &self,
        rule: &str,
        window: (i64, i64),
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
                .quarantine_between(rule, window, after, CHUNK)
                .map_err(internal)?;
            let Some(last) = rows.last() else {
                return Ok(());
            };
            after = Some((last.at, last.hash));
            let full = rows.len() == CHUNK;
            let engine = self.engine.current();
            let (free, kept): (Vec<HoldRow>, Vec<HoldRow>) = rows.into_iter().partition(|r| {
                let name = match r.sealed.as_deref() {
                    None => Some(String::new()),
                    Some(sealed) => self.sealer.as_deref().and_then(|s| s.open(sealed).ok()),
                };
                // A sealed name that won't open can't be judged: it stays.
                name.is_some_and(|name| !engine.holds(&r.hash, &name, now))
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
    Rejudged::decode(&text).filter(|r| now - r.at < NOTICE_SECS)
}
