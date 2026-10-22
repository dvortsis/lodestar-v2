//! What config files and floor-marked lists added to the locked floor,
//! and the entries they later dropped (owner's decision §2.7, §20.7).
//!
//! Config files (`blocklist.files`) and lists marked as part of the floor
//! may only **add** to it. An entry a file no longer lists, or that a
//! floor-marked list's publisher removed, stays blocked and waits in a
//! *pending floor removals* list until it's removed (or kept) in the app,
//! with the effect warning and the password.
//!
//! Both are kept in `settings` as packed 20-byte hashes:
//! - `floor.file:<path>`: every entry the file added that's still in the
//!   floor, so a later start can tell what the file dropped;
//! - `floor.pending:file:<path>` and `floor.pending:list:<id>`: the
//!   entries waiting for a decision.
//!
//! A file's entries are stored in `cs_hashes` as imported (CS-1b). A
//! list's pending entries are added to its floor layer when it's applied.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, params};

use crate::{Result, Store};

const MANIFEST: &str = "floor.file:";
const PENDING: &str = "floor.pending:";
/// `cs_hashes.origin` of an imported entry.
const IMPORTED: u8 = 2;

type Set = BTreeSet<[u8; 20]>;

/// Where a floor entry came from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FloorSource {
    /// A `blocklist.files` path, as configured.
    File(String),
    /// A subscription marked as part of the floor.
    List(i64),
}

impl FloorSource {
    /// The stable text form (`file:<path>`, `list:<id>`), also used by the
    /// filters API.
    pub fn key(&self) -> String {
        match self {
            FloorSource::File(p) => format!("file:{p}"),
            FloorSource::List(id) => format!("list:{id}"),
        }
    }

    pub fn from_key(key: &str) -> Option<FloorSource> {
        if let Some(p) = key.strip_prefix("file:") {
            return (!p.is_empty()).then(|| FloorSource::File(p.to_owned()));
        }
        let id = key.strip_prefix("list:")?.parse().ok()?;
        Some(FloorSource::List(id))
    }
}

/// What settling pending floor removals did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settled {
    /// The entries that left the source's pending list.
    pub settled: Vec<[u8; 20]>,
    /// Of those, the ones that left the floor: a removal leaves an entry
    /// another config file lists, or that the floor holds otherwise.
    pub unblocked: Vec<[u8; 20]>,
}

/// Floor entries a source dropped, still blocked until you decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingFloor {
    pub source: FloorSource,
    pub hashes: Vec<[u8; 20]>,
}

fn unpack(data: &[u8]) -> Set {
    data.chunks_exact(20)
        .map(|c| <[u8; 20]>::try_from(c).expect("20 bytes"))
        .collect()
}

fn read(c: &Connection, key: &str) -> Result<Set> {
    let data: Option<Vec<u8>> = c
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(data.as_deref().map(unpack).unwrap_or_default())
}

fn write(c: &Connection, key: &str, set: &Set) -> Result<()> {
    if set.is_empty() {
        c.execute("DELETE FROM settings WHERE key = ?1", [key])?;
    } else {
        let data: Vec<u8> = set.iter().flatten().copied().collect();
        c.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, data],
        )?;
    }
    Ok(())
}

/// Whether `hash` is in `cs_hashes` as an import. Your own confirmations
/// and reports aren't any file's to drop.
fn imported(c: &Connection, hash: &[u8; 20]) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS (SELECT 1 FROM cs_hashes WHERE hash = ?1 AND origin = ?2)",
        params![&hash[..], IMPORTED],
        |r| r.get(0),
    )?)
}

/// Every entry the config files other than `path` still add.
fn listed_by_other_files(c: &Connection, path: &str) -> Result<Set> {
    let mut stmt =
        c.prepare_cached("SELECT value FROM settings WHERE key LIKE 'floor.file:%' AND key != ?1")?;
    let rows = stmt.query_map([format!("{MANIFEST}{path}")], |r| r.get::<_, Vec<u8>>(0))?;
    let mut out = Set::new();
    for data in rows {
        out.extend(unpack(&data?));
    }
    Ok(out)
}

fn pending_key(source: &FloorSource) -> String {
    format!("{PENDING}{}", source.key())
}

/// A floor-marked list's applied version changes from `old` to `new`:
/// entries the publisher removed wait in the list's pending removals, and
/// entries it lists again leave them. Returns how many newly wait. Lists
/// that aren't part of the floor are left alone.
pub(crate) fn note_list_version(
    c: &Connection,
    id: i64,
    old: Option<&[u8]>,
    new: &[u8],
) -> Result<usize> {
    let floor: bool = c
        .query_row("SELECT floor FROM subscriptions WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?
        .unwrap_or(false);
    if !floor {
        return Ok(0);
    }
    let new = unpack(new);
    let key = pending_key(&FloorSource::List(id));
    let mut pending = read(c, &key)?;
    let mut added = 0;
    for h in old.map(unpack).unwrap_or_default().difference(&new) {
        added += usize::from(pending.insert(*h));
    }
    pending.retain(|h| !new.contains(h));
    write(c, &key, &pending)?;
    Ok(added)
}

/// A subscription is gone: so are its pending removals.
pub(crate) fn forget_list(c: &Connection, id: i64) -> Result<()> {
    write(c, &pending_key(&FloorSource::List(id)), &Set::new())
}

impl Store {
    /// Adds a config file's entries to the floor as imported CS-1b
    /// entries. Entries it added before and no longer lists stay in the
    /// floor and wait in its pending removals. Returns how many wait.
    /// A file that's no longer configured is synced with no entries.
    ///
    /// An entry another file still lists doesn't wait: that file keeps it
    /// in the floor, and it waits in that file's list if that file drops
    /// it too.
    pub fn sync_floor_file(&mut self, path: &str, hashes: &[[u8; 20]], at: i64) -> Result<usize> {
        let tx = crate::begin_write(&mut self.conn)?;
        let listed: Set = hashes.iter().copied().collect();
        let manifest_key = format!("{MANIFEST}{path}");
        let key = pending_key(&FloorSource::File(path.to_owned()));
        let before = read(&tx, &manifest_key)?;
        let mut pending = read(&tx, &key)?;
        {
            let mut add = tx.prepare_cached(
                "INSERT INTO cs_hashes (hash, origin, added_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (hash) DO NOTHING",
            )?;
            for h in &listed {
                add.execute(params![&h[..], IMPORTED, at])?;
            }
        }
        pending.extend(before.difference(&listed));
        let others = listed_by_other_files(&tx, path)?;
        let mut kept = Set::new();
        for h in pending {
            if !listed.contains(&h) && !others.contains(&h) && imported(&tx, &h)? {
                kept.insert(h);
            }
        }
        let manifest: Set = listed.union(&kept).copied().collect();
        write(&tx, &manifest_key, &manifest)?;
        write(&tx, &key, &kept)?;
        tx.commit()?;
        Ok(kept.len())
    }

    /// The config files that have added floor entries, as configured.
    pub fn floor_files(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key FROM settings WHERE key LIKE 'floor.file:%' ORDER BY key")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for k in rows {
            out.push(k?[MANIFEST.len()..].to_owned());
        }
        Ok(out)
    }

    /// Every source's pending floor removals.
    pub fn pending_floor(&self) -> Result<Vec<PendingFloor>> {
        let mut stmt = self.conn.prepare(
            "SELECT key, value FROM settings WHERE key LIKE 'floor.pending:%' ORDER BY key",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (key, data) = row?;
            if let Some(source) = FloorSource::from_key(&key[PENDING.len()..]) {
                out.push(PendingFloor {
                    source,
                    hashes: unpack(&data).into_iter().collect(),
                });
            }
        }
        Ok(out)
    }

    /// One source's pending floor removals.
    pub fn pending_floor_of(&self, source: &FloorSource) -> Result<Vec<[u8; 20]>> {
        Ok(read(&self.conn, &pending_key(source))?
            .into_iter()
            .collect())
    }

    /// Settles pending floor removals: `hashes` (all of them when `None`)
    /// leave the source's pending list. With `remove` they leave the
    /// floor; otherwise they stay in it as ordinary imported entries, no
    /// longer tied to the source. Returns the entries settled, and which
    /// of them left the floor.
    pub fn resolve_pending_floor(
        &mut self,
        source: &FloorSource,
        hashes: Option<&[[u8; 20]]>,
        remove: bool,
        at: i64,
    ) -> Result<Settled> {
        let tx = crate::begin_write(&mut self.conn)?;
        let mut unblocked = Vec::new();
        let key = pending_key(source);
        let mut pending = read(&tx, &key)?;
        let chosen: Set = match hashes {
            Some(hs) => hs
                .iter()
                .filter(|h| pending.contains(*h))
                .copied()
                .collect(),
            None => pending.clone(),
        };
        pending.retain(|h| !chosen.contains(h));
        write(&tx, &key, &pending)?;
        match source {
            FloorSource::File(path) => {
                let manifest_key = format!("{MANIFEST}{path}");
                let mut manifest = read(&tx, &manifest_key)?;
                manifest.retain(|h| !chosen.contains(h));
                write(&tx, &manifest_key, &manifest)?;
                if remove {
                    // An entry another file lists stays: that file adds it,
                    // and the next start would add it back.
                    let others = listed_by_other_files(&tx, path)?;
                    let mut del =
                        tx.prepare_cached("DELETE FROM cs_hashes WHERE hash = ?1 AND origin = ?2")?;
                    for h in chosen.difference(&others) {
                        if del.execute(params![&h[..], IMPORTED])? > 0 {
                            unblocked.push(*h);
                        }
                    }
                }
            }
            // Removed: the list's layer is rebuilt without them. Kept:
            // they become entries of their own.
            FloorSource::List(_) if !remove => {
                let mut add = tx.prepare_cached(
                    "INSERT INTO cs_hashes (hash, origin, added_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT (hash) DO NOTHING",
                )?;
                for h in &chosen {
                    add.execute(params![&h[..], IMPORTED, at])?;
                }
            }
            // Removed: what the floor still holds by hash stays blocked.
            FloorSource::List(_) => {
                for h in &chosen {
                    let held: bool = tx.query_row(
                        "SELECT EXISTS (SELECT 1 FROM cs_hashes WHERE hash = ?1)",
                        [&h[..]],
                        |r| r.get(0),
                    )?;
                    if !held {
                        unblocked.push(*h);
                    }
                }
            }
        }
        tx.commit()?;
        Ok(Settled {
            settled: chosen.into_iter().collect(),
            unblocked,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewSubscription, Refresh, StoreConfig};

    fn store() -> Store {
        Store::open_in_memory(StoreConfig::default()).unwrap()
    }

    fn cs(s: &Store) -> Vec<([u8; 20], u8)> {
        let mut v = s.filter_state().unwrap().cs_hashes;
        v.sort();
        v
    }

    #[test]
    fn sources_round_trip() {
        for s in [FloorSource::File("/a/b:c.txt".into()), FloorSource::List(7)] {
            assert_eq!(FloorSource::from_key(&s.key()), Some(s));
        }
        assert_eq!(FloorSource::from_key("file:"), None);
        assert_eq!(FloorSource::from_key("list:x"), None);
        assert_eq!(FloorSource::from_key("other"), None);
    }

    #[test]
    fn files_only_add() {
        let mut s = store();
        let (a, b, c) = ([1; 20], [2; 20], [3; 20]);
        assert_eq!(s.sync_floor_file("f.txt", &[a, b], 10).unwrap(), 0);
        assert_eq!(cs(&s), [(a, IMPORTED), (b, IMPORTED)]);
        // `b` was deleted from the file: still in the floor, waiting.
        assert_eq!(s.sync_floor_file("f.txt", &[a, c], 20).unwrap(), 1);
        assert_eq!(cs(&s).len(), 3);
        let p = s.pending_floor().unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].source, FloorSource::File("f.txt".into()));
        assert_eq!(p[0].hashes, [b]);
        // Reported again on the next start, until it's decided.
        assert_eq!(s.sync_floor_file("f.txt", &[a, c], 30).unwrap(), 1);
        // Listed again: no longer waiting.
        assert_eq!(s.sync_floor_file("f.txt", &[a, b, c], 40).unwrap(), 0);
        assert!(s.pending_floor().unwrap().is_empty());
        // The file left the config: everything it added waits.
        assert_eq!(s.floor_files().unwrap(), ["f.txt"]);
        assert_eq!(s.sync_floor_file("f.txt", &[], 50).unwrap(), 3);
        assert_eq!(cs(&s).len(), 3);
    }

    #[test]
    fn removals_take_effect_only_when_settled() {
        let mut s = store();
        let (a, b, mine) = ([1; 20], [2; 20], [9; 20]);
        s.add_cs_hashes(&[mine], 0, 1).unwrap();
        s.sync_floor_file("f.txt", &[a, b, mine], 10).unwrap();
        // Your confirmation isn't the file's to drop, so it never waits.
        assert_eq!(s.sync_floor_file("f.txt", &[], 20).unwrap(), 2);
        let src = FloorSource::File("f.txt".into());
        // Keeping `a` makes it an ordinary entry; removing `b` unblocks it.
        assert_eq!(
            s.resolve_pending_floor(&src, Some(&[a, [7; 20]]), false, 30)
                .unwrap(),
            Settled {
                settled: vec![a],
                unblocked: vec![]
            }
        );
        assert_eq!(
            s.resolve_pending_floor(&src, None, true, 30).unwrap(),
            Settled {
                settled: vec![b],
                unblocked: vec![b]
            }
        );
        assert_eq!(cs(&s), [(a, IMPORTED), (mine, 0)]);
        assert!(s.pending_floor().unwrap().is_empty());
        // Settled entries aren't the file's any more.
        assert_eq!(s.sync_floor_file("f.txt", &[], 40).unwrap(), 0);
        assert_eq!(cs(&s).len(), 2);
    }

    /// An entry two files list stays blocked while either lists it, and
    /// never waits in one file's list while the other still adds it.
    #[test]
    fn an_entry_two_files_list_stays_while_either_does() {
        let mut s = store();
        let (a, h) = ([1; 20], [2; 20]);
        s.sync_floor_file("a.txt", &[a, h], 10).unwrap();
        s.sync_floor_file("b.txt", &[h], 10).unwrap();
        // `a.txt` drops `h`; `b.txt` still lists it, so nothing waits.
        assert_eq!(s.sync_floor_file("a.txt", &[a], 20).unwrap(), 0);
        assert_eq!(s.sync_floor_file("b.txt", &[h], 20).unwrap(), 0);
        assert!(s.pending_floor().unwrap().is_empty());
        assert!(cs(&s).contains(&(h, IMPORTED)));
        // Then `b.txt` drops it too: it waits there.
        assert_eq!(s.sync_floor_file("a.txt", &[a], 30).unwrap(), 0);
        assert_eq!(s.sync_floor_file("b.txt", &[], 30).unwrap(), 1);
        let src = FloorSource::File("b.txt".into());
        assert_eq!(s.pending_floor_of(&src).unwrap(), [h]);
        assert_eq!(
            s.resolve_pending_floor(&src, None, true, 40)
                .unwrap()
                .unblocked,
            [h]
        );
        assert!(!cs(&s).contains(&(h, IMPORTED)), "removed");

        // A file that starts listing an entry after another dropped it:
        // removing it through the first file's list leaves it blocked.
        let mut s = store();
        s.sync_floor_file("a.txt", &[h], 10).unwrap();
        assert_eq!(s.sync_floor_file("a.txt", &[], 20).unwrap(), 1);
        s.sync_floor_file("b.txt", &[h], 20).unwrap();
        let src = FloorSource::File("a.txt".into());
        assert_eq!(
            s.resolve_pending_floor(&src, None, true, 30).unwrap(),
            Settled {
                settled: vec![h],
                unblocked: vec![]
            },
            "settled, but b.txt still adds it"
        );
        assert!(cs(&s).contains(&(h, IMPORTED)));
        assert!(s.pending_floor().unwrap().is_empty());
    }

    #[test]
    fn floor_lists_keep_what_upstream_removed() {
        let mut s = store();
        let mut sub = |url: &str, floor: bool| {
            s.add_subscription(&NewSubscription {
                url,
                name: url,
                kind: 0,
                format: Some("hashes"),
                action: 4,
                floor,
                interval_secs: 3600,
            })
            .unwrap()
        };
        let (floor, plain) = (sub("https://a/", true), sub("https://b/", false));
        let v = |hs: &[[u8; 20]]| hs.concat();
        let updated = |s: &mut Store, id, data: &[u8]| {
            s.record_refresh(
                id,
                1,
                &Refresh::Updated {
                    data,
                    entries: (data.len() / 20) as i64,
                    etag: None,
                    last_modified: None,
                },
            )
            .unwrap()
        };
        let (a, b, c) = ([1; 20], [2; 20], [3; 20]);
        for id in [floor, plain] {
            assert_eq!(updated(&mut s, id, &v(&[a, b, c])), 0);
            assert_eq!(updated(&mut s, id, &v(&[a, c])), usize::from(id == floor));
        }
        let src = FloorSource::List(floor);
        assert_eq!(s.pending_floor_of(&src).unwrap(), [b]);
        assert!(
            s.pending_floor_of(&FloorSource::List(plain))
                .unwrap()
                .is_empty()
        );
        // A held version that drops `c`, accepted.
        s.record_refresh(
            floor,
            2,
            &Refresh::Held {
                data: &v(&[a]),
                entries: 1,
                etag: None,
                last_modified: None,
                why: "shrank",
            },
        )
        .unwrap();
        assert!(s.resolve_held(floor, true, 3).unwrap());
        assert_eq!(s.pending_floor_of(&src).unwrap(), [b, c]);
        // Listed again upstream: no longer waiting.
        updated(&mut s, floor, &v(&[a, b]));
        assert_eq!(s.pending_floor_of(&src).unwrap(), [c]);
        // Kept: an ordinary imported entry.
        s.resolve_pending_floor(&src, None, false, 4).unwrap();
        assert_eq!(cs(&s), [(c, IMPORTED)]);
        updated(&mut s, floor, &v(&[a]));
        assert!(s.remove_subscription(floor).unwrap());
        assert!(s.pending_floor().unwrap().is_empty());
    }
}
