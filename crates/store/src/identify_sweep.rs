//! What search indexes from identification answers (ENR-11), and the
//! backfill that brings the torrents already holding them to the index.
//!
//! **Served identifications** ([`Store::served_identifications`]): the
//! claims rows of one torrent that search, Torznab and export may serve.
//! Each must be all of these, the same rule as the effective title's
//! (`lodestar_pipeline::identify::stored_title`, ENR-10 and ENR-terms):
//! - an answer: a version 3 row whose details section holds a match
//!   confidence ([`Claims::identification`], the test
//!   `Claims::shapes_the_record` makes too, so a purge that removes it
//!   reindexes the torrent), from a source whose bit isn't in the
//!   torrent's `sources` (a finder can't enrich what it found);
//! - not through a private source (ENR-9b: the section follows the
//!   row's `private_source` flag as the claimed name does);
//! - on a torrent that isn't [`flags::PRIVACY`](crate::flags::PRIVACY);
//! - from a source whose terms record (`enr10/terms`) allows passing its
//!   answers on. A missing or unreadable record doesn't.
//!
//! The torrent check is stricter than `stored_title`, which doesn't read
//! the torrent's flags: a private torrent's answers are never served.
//!
//! **The sweep** ([`Store::identify_sweep_step`]): torrents enriched
//! after ENR-11 reach the index through the backfill's `reindex_log`
//! entries (ENR-10). Those enriched before it hold version 3 claims the
//! index never read, so this walks `torrent_claims` once, in bounded
//! steps, and logs every torrent holding a version 3 row for the index
//! (plan §3, SB-6's contract: no generation, no corpus walk). Its cursor
//! and end live in `settings` under `identify_sweep.`, written in the
//! same transaction as the step's log entries, so a restart goes on
//! where it stopped and never loses a step.
//!
//! [`Claims::identification`]: crate::claims::Claims::identification

use rusqlite::{OptionalExtension, params};

use crate::claims::{self, AltTitle, ExternalId, VERSION_DETAILS};
use crate::{Result, SourceId, Store, begin_write, flags};

/// Where a plugin's terms record is kept in its `source_state`
/// (`lodestar_pipeline::identify::TERMS_KEY`).
pub const TERMS_KEY: &str = "enr10/terms";

/// Torrent claims rows one sweep step reads, at most.
pub const SWEEP_BATCH: usize = 1024;

const CURSOR_TORRENT: &str = "identify_sweep.cursor_torrent";
const CURSOR_SOURCE: &str = "identify_sweep.cursor_source";
const HIGH: &str = "identify_sweep.high";
const DONE: &str = "identify_sweep.done";
/// The last torrent logged, so a torrent whose rows span two steps is
/// logged once.
const LAST_LOGGED: &str = "identify_sweep.last_logged";

/// One identification answer search may serve
/// ([`Store::served_identifications`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identified {
    pub source: SourceId,
    /// The answer's match confidence, 0–100.
    pub confidence: u8,
    /// Its external ids, each in [`claims::ENRICHMENT_SCHEMES`] and
    /// [`claims::id_ok`]'s form.
    pub ids: Vec<ExternalId>,
    pub canonical_title: Option<String>,
    pub alt_titles: Vec<AltTitle>,
}

/// Where the sweep stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepState {
    /// No step has run.
    NotStarted,
    /// Rows up to `(torrent, source)` are done, of those up to torrent
    /// `high` that existed when the sweep started.
    Running {
        torrent: i64,
        source: i64,
        high: i64,
    },
    Done,
}

impl SweepState {
    pub fn is_done(self) -> bool {
        self == SweepState::Done
    }
}

/// What one sweep step did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SweepStep {
    /// Claims rows read.
    pub read: usize,
    /// Torrents logged for the index.
    pub logged: usize,
    /// The sweep is complete.
    pub done: bool,
}

/// Whether a terms record allows redistribution: `lodestar_sources`'s
/// `Terms::from_record` read for that one flag (version 1, a flags byte
/// with no unknown bits, a big-endian `u64`). Anything else doesn't.
pub fn record_redistributes(v: &[u8]) -> bool {
    matches!(v, [1, flags, rest @ ..] if rest.len() == 8 && flags & !0b111 == 0 && flags & 0b10 != 0)
}

impl Store {
    /// Whether `source`'s recorded terms let its answers be passed on
    /// (ENR-terms). A missing or unreadable record doesn't.
    pub fn redistributes(&self, source: SourceId) -> Result<bool> {
        Ok(self
            .plugin_state(source, TERMS_KEY)?
            .is_some_and(|v| record_redistributes(&v)))
    }

    /// The identification answers on torrent `torrent_id` that search may
    /// index, by source id (see the module's rules). Empty for a torrent
    /// that isn't stored.
    pub fn served_identifications(&self, torrent_id: i64) -> Result<Vec<Identified>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT c.source_id, c.data, t.sources, t.flags
             FROM torrent_claims c JOIN torrents t ON t.id = c.torrent_id
             WHERE c.torrent_id = ?1 AND c.flags = 0 AND (t.sources >> c.source_id) & 1 = 0
               AND substr(c.data, 1, 1) = ?2
             ORDER BY c.source_id",
        )?;
        let rows = stmt.query_map(params![torrent_id, [VERSION_DETAILS].as_slice()], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (source, data, finders, tflags) = row?;
            if tflags & flags::PRIVACY != 0 {
                return Ok(Vec::new());
            }
            let source = SourceId::from_sql(source)?;
            if finders & source.bit() != 0 {
                continue;
            }
            // A row that doesn't decode is dropped, as everywhere else.
            let Ok(c) = claims::decode(&data) else {
                continue;
            };
            // The claims-shape rule's own test (`Claims::identification`):
            // a purge reindexes every torrent losing an answer served here.
            let Some((_, confidence)) = c.identification() else {
                continue;
            };
            let Some(d) = c.details else { continue };
            if !self.redistributes(source)? {
                continue;
            }
            let ids = c
                .ids
                .into_iter()
                .filter(|i| {
                    claims::ENRICHMENT_SCHEMES.contains(&i.scheme.as_str())
                        && claims::id_ok(&i.scheme, &i.value)
                })
                .collect();
            out.push(Identified {
                source,
                confidence,
                ids,
                canonical_title: d.canonical_title,
                alt_titles: d.alt_titles,
            });
        }
        Ok(out)
    }

    /// Where the sweep stands.
    pub fn identify_sweep_state(&self) -> Result<SweepState> {
        if self.setting_i64(DONE)? == Some(1) {
            return Ok(SweepState::Done);
        }
        Ok(match self.setting_i64(HIGH)? {
            None => SweepState::NotStarted,
            Some(high) => SweepState::Running {
                torrent: self.setting_i64(CURSOR_TORRENT)?.unwrap_or(0),
                source: self.setting_i64(CURSOR_SOURCE)?.unwrap_or(-1),
                high,
            },
        })
    }

    /// One step of the sweep: reads at most `limit` claims rows (at least
    /// one, at most [`SWEEP_BATCH`]) past the cursor, logs each torrent
    /// holding a version 3 row that isn't through a private source, and
    /// moves the cursor, in one transaction. The first step fixes the end
    /// at the highest torrent holding claims then; later claims reach the
    /// index through the backfill. A complete sweep does nothing.
    pub fn identify_sweep_step(&mut self, limit: usize) -> Result<SweepStep> {
        let limit = limit.clamp(1, SWEEP_BATCH);
        let tx = begin_write(&mut self.conn)?;
        let get = |key: &str| -> Result<Option<i64>> {
            Ok(tx
                .prepare_cached("SELECT value FROM settings WHERE key = ?1")?
                .query_row([key], |r| r.get(0))
                .optional()?)
        };
        if get(DONE)? == Some(1) {
            return Ok(SweepStep {
                done: true,
                ..SweepStep::default()
            });
        }
        let high = match get(HIGH)? {
            Some(h) => h,
            None => tx
                .prepare_cached("SELECT coalesce(max(torrent_id), 0) FROM torrent_claims")?
                .query_row([], |r| r.get(0))?,
        };
        let (mut torrent, mut source) = (
            get(CURSOR_TORRENT)?.unwrap_or(0),
            get(CURSOR_SOURCE)?.unwrap_or(-1),
        );
        let mut step = SweepStep::default();
        let mut logged: Vec<i64> = Vec::new();
        let mut last = get(LAST_LOGGED)?;
        {
            let mut stmt = tx.prepare_cached(
                "SELECT torrent_id, source_id, flags = 0 AND substr(data, 1, 1) = ?4
                 FROM torrent_claims
                 WHERE (torrent_id, source_id) > (?1, ?2) AND torrent_id <= ?3
                 ORDER BY torrent_id, source_id LIMIT ?5",
            )?;
            let rows = stmt.query_map(
                params![
                    torrent,
                    source,
                    high,
                    [VERSION_DETAILS].as_slice(),
                    limit as i64
                ],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, bool>(2)?,
                    ))
                },
            )?;
            let mut log = tx.prepare_cached("INSERT INTO reindex_log (torrent_id) VALUES (?1)")?;
            for row in rows {
                let (t, s, wanted) = row?;
                step.read += 1;
                (torrent, source) = (t, s);
                if wanted && last != Some(t) {
                    log.execute([t])?;
                    logged.push(t);
                    last = Some(t);
                }
            }
        }
        step.logged = logged.len();
        // The walk is bounded by `high`, so only a short read ends it: a
        // full batch may stop partway through torrent `high`'s rows.
        step.done = step.read < limit;
        let mut set = tx.prepare_cached(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )?;
        set.execute(params![HIGH, high])?;
        set.execute(params![CURSOR_TORRENT, torrent])?;
        set.execute(params![CURSOR_SOURCE, source])?;
        if let Some(t) = last {
            set.execute(params![LAST_LOGGED, t])?;
        }
        if step.done {
            set.execute(params![DONE, 1])?;
        }
        drop(set);
        tx.commit()?;
        Ok(step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terms_record_redistributes_only_with_its_flag() {
        let rec = |flags: u8| {
            let mut v = vec![1, flags];
            v.extend(180u64.to_be_bytes());
            v
        };
        assert!(record_redistributes(&rec(0b010)));
        assert!(record_redistributes(&rec(0b111)));
        assert!(!record_redistributes(&rec(0b101)));
        assert!(!record_redistributes(&rec(0b1010)));
        let mut v2 = rec(0b010);
        v2[0] = 2;
        assert!(!record_redistributes(&v2));
        assert!(!record_redistributes(&rec(0b010)[..9]));
        let mut long = rec(0b010);
        long.push(0);
        assert!(!record_redistributes(&long));
        assert!(!record_redistributes(&[]));
    }
}
