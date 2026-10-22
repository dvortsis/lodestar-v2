//! Your marks on torrents (architecture §7 `user_items`): favourite,
//! hidden, labels and a note. One row per marked torrent; clearing the
//! last mark deletes the row, so the table only holds what you set.
//!
//! "Hidden" here is yours, apart from a filter rule's `flags::HIDDEN`.
//!
//! **Labels** are stored as one canonical text: each label normalized
//! (trimmed, inner whitespace runs folded to one space), then sorted by
//! byte order, deduplicated and joined with U+001F (unit separator). A
//! label can't contain control characters, so the separator is never
//! ambiguous, and equal label sets always encode to equal text.

use rusqlite::{OptionalExtension, params};

use crate::user_data::{check_note, sql_limit, write_tx};
use crate::{Error, Result, Store};

/// At most this many labels per torrent.
pub const MAX_LABELS: usize = 32;
/// At most this many characters per label, after normalization.
pub const MAX_LABEL_CHARS: usize = 64;

const SEP: char = '\u{1f}';

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserItem {
    pub torrent_id: i64,
    pub favourite: bool,
    pub hidden: bool,
    /// Normalized, sorted and unique.
    pub labels: Vec<String>,
    pub note: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A change to a torrent's marks; `None` leaves a field as it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UserItemUpdate<'a> {
    pub favourite: Option<bool>,
    pub hidden: Option<bool>,
    /// Replaces the labels; an empty list removes them.
    pub labels: Option<&'a [&'a str]>,
    /// `Some(None)` removes the note.
    pub note: Option<Option<&'a str>>,
}

/// One label in canonical form, or why it's refused.
pub(crate) fn normalize_label(label: &str) -> Result<String> {
    if label.chars().any(char::is_control) {
        return Err(Error::Invalid("labels can't contain control characters"));
    }
    let folded = label.split_whitespace().collect::<Vec<_>>().join(" ");
    let n = folded.chars().count();
    if n == 0 || n > MAX_LABEL_CHARS {
        return Err(Error::Invalid("a label must be 1-64 characters"));
    }
    Ok(folded)
}

/// The canonical encoding of a label set; `None` when it's empty.
pub(crate) fn encode_labels(labels: &[&str]) -> Result<Option<String>> {
    let mut out = labels
        .iter()
        .map(|l| normalize_label(l))
        .collect::<Result<Vec<_>>>()?;
    out.sort_unstable();
    out.dedup();
    if out.len() > MAX_LABELS {
        return Err(Error::Invalid("at most 32 labels"));
    }
    Ok((!out.is_empty()).then(|| out.join(&SEP.to_string())))
}

/// Decodes what [`encode_labels`] wrote. Anything else is corrupt.
pub(crate) fn decode_labels(text: Option<&str>) -> Result<Vec<String>> {
    let Some(text) = text else {
        return Ok(Vec::new());
    };
    let labels: Vec<String> = text.split(SEP).map(str::to_owned).collect();
    let canonical = labels.len() <= MAX_LABELS
        && labels.windows(2).all(|w| w[0] < w[1])
        && labels
            .iter()
            .all(|l| normalize_label(l).is_ok_and(|n| n == *l));
    if canonical {
        Ok(labels)
    } else {
        Err(Error::Corrupt("labels are not canonical"))
    }
}

const COLUMNS: &str = "torrent_id, favourite, hidden, labels, note, created_at, updated_at";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(UserItem, Option<String>)> {
    Ok((
        UserItem {
            torrent_id: r.get(0)?,
            favourite: r.get(1)?,
            hidden: r.get(2)?,
            labels: Vec::new(),
            note: r.get(4)?,
            created_at: r.get(5)?,
            updated_at: r.get(6)?,
        },
        r.get(3)?,
    ))
}

fn finish((mut item, labels): (UserItem, Option<String>)) -> Result<UserItem> {
    item.labels = decode_labels(labels.as_deref())?;
    Ok(item)
}

impl Store {
    pub fn user_item(&self, torrent_id: i64) -> Result<Option<UserItem>> {
        self.conn
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM user_items WHERE torrent_id = ?1"
            ))?
            .query_row([torrent_id], row)
            .optional()?
            .map(finish)
            .transpose()
    }

    /// The marks of those of `ids` that have any, in id order: a search
    /// page's enrichment.
    pub fn user_items(&self, ids: &[i64]) -> Result<Vec<UserItem>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM user_items
             WHERE torrent_id IN (SELECT value FROM json_each(?1)) ORDER BY torrent_id"
        ))?;
        let rows = stmt.query_map([format!("[{list}]")], row)?;
        rows.map(|r| finish(r?)).collect()
    }

    /// Changes a torrent's marks and returns them; `None` when no mark is
    /// left, in which case the row is gone. An unknown torrent is
    /// [`Error::NotFound`].
    pub fn set_user_item(
        &mut self,
        torrent_id: i64,
        u: &UserItemUpdate<'_>,
        now: i64,
    ) -> Result<Option<UserItem>> {
        let labels = u.labels.map(encode_labels).transpose()?;
        if let Some(Some(note)) = u.note {
            check_note(note)?;
        }
        let tx = write_tx(&mut self.conn)?;
        let known: bool = tx
            .prepare_cached("SELECT EXISTS (SELECT 1 FROM torrents WHERE id = ?1)")?
            .query_row([torrent_id], |r| r.get(0))?;
        if !known {
            return Err(Error::NotFound("torrent"));
        }
        let old = tx
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM user_items WHERE torrent_id = ?1"
            ))?
            .query_row([torrent_id], row)
            .optional()?;
        let (old_item, old_labels) = match old {
            Some((item, labels)) => (Some(item), labels),
            None => (None, None),
        };
        let favourite = u
            .favourite
            .unwrap_or(old_item.as_ref().is_some_and(|i| i.favourite));
        let hidden = u
            .hidden
            .unwrap_or(old_item.as_ref().is_some_and(|i| i.hidden));
        let old_note = old_item.as_ref().and_then(|i| i.note.clone());
        let labels = labels.unwrap_or(old_labels.clone());
        let note = match u.note {
            Some(n) => n.map(str::to_owned),
            None => old_note.clone(),
        };
        // Old text left behind (cleared or replaced) stays in WAL frames
        // until a scrub (§2.17).
        let replaced = (old_labels.is_some() && old_labels != labels)
            || (old_note.is_some() && old_note != note);
        let result = if !favourite && !hidden && labels.is_none() && note.is_none() {
            let deleted =
                tx.execute("DELETE FROM user_items WHERE torrent_id = ?1", [torrent_id])?;
            tx.commit()?;
            if deleted > 0 {
                self.after_user_delete();
            }
            None
        } else {
            let created_at = old_item.as_ref().map_or(now, |i| i.created_at);
            tx.prepare_cached(
                "INSERT INTO user_items
                     (torrent_id, favourite, hidden, labels, note, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (torrent_id) DO UPDATE SET favourite = excluded.favourite,
                     hidden = excluded.hidden, labels = excluded.labels, note = excluded.note,
                     updated_at = excluded.updated_at",
            )?
            .execute(params![
                torrent_id, favourite, hidden, labels, note, created_at, now
            ])?;
            tx.commit()?;
            if replaced {
                self.after_user_delete();
            }
            Some(UserItem {
                torrent_id,
                favourite,
                hidden,
                labels: decode_labels(labels.as_deref())?,
                note,
                created_at,
                updated_at: now,
            })
        };
        Ok(result)
    }

    /// Removes every mark on a torrent.
    pub fn delete_user_item(&mut self, torrent_id: i64) -> Result<bool> {
        let deleted = self
            .conn
            .execute("DELETE FROM user_items WHERE torrent_id = ?1", [torrent_id])?;
        if deleted > 0 {
            self.after_user_delete();
        }
        Ok(deleted > 0)
    }

    /// Marked torrents, most recently changed first: favourites only,
    /// hidden only, those carrying `label`, or (all `None`/`false`) every
    /// one. `before` is the previous page's last `(updated_at,
    /// torrent_id)`.
    pub fn list_user_items(
        &self,
        favourite: bool,
        hidden: bool,
        label: Option<&str>,
        before: Option<(i64, i64)>,
        limit: usize,
    ) -> Result<Vec<UserItem>> {
        let label = label.map(normalize_label).transpose()?;
        let (b_at, b_id) = before.unwrap_or((i64::MAX, i64::MAX));
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM user_items
             WHERE (?1 = 0 OR favourite = 1) AND (?2 = 0 OR hidden = 1)
               AND (?3 IS NULL OR instr(char(31) || labels || char(31),
                                        char(31) || ?3 || char(31)) > 0)
               AND (updated_at, torrent_id) < (?4, ?5)
             ORDER BY updated_at DESC, torrent_id DESC LIMIT ?6"
        ))?;
        let rows = stmt.query_map(
            params![favourite, hidden, label, b_at, b_id, sql_limit(limit)],
            row,
        )?;
        rows.map(|r| finish(r?)).collect()
    }

    /// Every label in use with how many torrents carry it, by label.
    pub fn labels_in_use(&self) -> Result<Vec<(String, u64)>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT labels FROM user_items WHERE labels IS NOT NULL")?;
        let mut counts = std::collections::BTreeMap::<String, u64>::new();
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let text: String = r.get(0)?;
            for l in decode_labels(Some(&text))? {
                *counts.entry(l).or_default() += 1;
            }
        }
        Ok(counts.into_iter().collect())
    }

    /// `(torrent_id, favourite, hidden)` for every torrent with either
    /// flag, in id order: what the user-flag hot column is rebuilt from.
    pub fn user_flags(&self) -> Result<Vec<(i64, bool, bool)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT torrent_id, favourite, hidden FROM user_items
             WHERE favourite = 1 OR hidden = 1 ORDER BY torrent_id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn labels_normalize_sort_and_dedupe() {
        let enc = encode_labels(&["  4K  HDR ", "anime", "4K HDR", "Anime"]).unwrap();
        assert_eq!(enc.as_deref(), Some("4K HDR\u{1f}Anime\u{1f}anime"));
        assert_eq!(
            decode_labels(enc.as_deref()).unwrap(),
            ["4K HDR", "Anime", "anime"]
        );
        assert_eq!(encode_labels(&[]).unwrap(), None);
        assert!(decode_labels(None).unwrap().is_empty());
    }

    #[test]
    fn bad_labels_are_refused() {
        for bad in ["", "   ", "a\u{1f}b", "tab\tinside", &"x".repeat(65)] {
            assert!(encode_labels(&[bad]).is_err(), "{bad:?}");
        }
        assert!(encode_labels(&["é".repeat(64).as_str()]).is_ok());
        let many: Vec<String> = (0..33).map(|i| format!("l{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(encode_labels(&many).is_err());
        // Duplicates count once.
        assert!(encode_labels(&["a"; 40]).is_ok());
    }

    #[test]
    fn non_canonical_text_is_corrupt() {
        for bad in ["b\u{1f}a", "a\u{1f}a", "a\u{1f}", " a", "a  b", ""] {
            assert!(decode_labels(Some(bad)).is_err(), "{bad:?}");
        }
    }

    use crate::user_data::{delete_torrent, test_store};

    fn set<'a>(s: &mut Store, id: i64, u: UserItemUpdate<'a>, now: i64) -> Option<UserItem> {
        s.set_user_item(id, &u, now).unwrap()
    }

    #[test]
    fn marks_merge_and_the_last_one_removes_the_row() {
        let mut s = test_store(5);
        let fav = UserItemUpdate {
            favourite: Some(true),
            ..Default::default()
        };
        let item = set(&mut s, 2, fav, 10).unwrap();
        assert_eq!(
            (item.favourite, item.hidden, item.created_at),
            (true, false, 10)
        );

        let labels = ["remux", " 4K "];
        let more = UserItemUpdate {
            labels: Some(&labels),
            note: Some(Some("watch with subs")),
            ..Default::default()
        };
        let item = set(&mut s, 2, more, 20).unwrap();
        assert_eq!(
            item,
            UserItem {
                torrent_id: 2,
                favourite: true,
                hidden: false,
                labels: vec!["4K".into(), "remux".into()],
                note: Some("watch with subs".into()),
                created_at: 10,
                updated_at: 20,
            }
        );
        assert_eq!(s.user_item(2).unwrap().unwrap(), item);

        let clear = UserItemUpdate {
            favourite: Some(false),
            labels: Some(&[]),
            note: Some(None),
            ..Default::default()
        };
        assert_eq!(set(&mut s, 2, clear, 30), None);
        assert_eq!(s.user_item(2).unwrap(), None);
        let rows: i64 = s
            .conn
            .query_row("SELECT count(*) FROM user_items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
        // Clearing a torrent with no marks writes nothing either.
        assert_eq!(set(&mut s, 3, clear, 30), None);
    }

    #[test]
    fn refusals() {
        let mut s = test_store(1);
        let fav = UserItemUpdate {
            favourite: Some(true),
            ..Default::default()
        };
        assert!(matches!(
            s.set_user_item(99, &fav, 0),
            Err(Error::NotFound("torrent"))
        ));
        let bad = UserItemUpdate {
            labels: Some(&["ok", "bad\u{7}"]),
            favourite: Some(true),
            ..Default::default()
        };
        assert!(matches!(
            s.set_user_item(1, &bad, 0),
            Err(Error::Invalid(_))
        ));
        let note = "n".repeat(4001);
        let long = UserItemUpdate {
            note: Some(Some(&note)),
            ..Default::default()
        };
        assert!(s.set_user_item(1, &long, 0).is_err());
        assert_eq!(s.user_item(1).unwrap(), None, "nothing was written");
        // A row with no mark can't exist, whoever writes it.
        assert!(
            s.conn
                .execute(
                    "INSERT INTO user_items (torrent_id, favourite, hidden, created_at,
                         updated_at) VALUES (1, 0, 0, 0, 0)",
                    [],
                )
                .is_err()
        );
    }

    #[test]
    fn lists_labels_flags_and_cascades() {
        let mut s = test_store(6);
        let tags_a = ["anime", "4K"];
        let tags_b = ["anime"];
        let tags_c = ["animated"];
        let ups = [
            (1, Some(true), None, Some(&tags_a[..])),
            (2, None, Some(true), Some(&tags_b[..])),
            (3, Some(true), Some(true), None),
            (4, None, None, Some(&tags_c[..])),
        ];
        for (i, (id, favourite, hidden, labels)) in ups.into_iter().enumerate() {
            let u = UserItemUpdate {
                favourite,
                hidden,
                labels,
                note: None,
            };
            set(&mut s, id, u, 100 + i as i64);
        }
        let ids = |v: Vec<UserItem>| v.into_iter().map(|i| i.torrent_id).collect::<Vec<_>>();
        assert_eq!(
            ids(s.list_user_items(false, false, None, None, 10).unwrap()),
            [4, 3, 2, 1]
        );
        assert_eq!(
            ids(s.list_user_items(true, false, None, None, 10).unwrap()),
            [3, 1]
        );
        assert_eq!(
            ids(s.list_user_items(false, true, None, None, 10).unwrap()),
            [3, 2]
        );
        // A label matches whole, not as a prefix ("anime" isn't "animated").
        assert_eq!(
            ids(s
                .list_user_items(false, false, Some(" anime"), None, 10)
                .unwrap()),
            [2, 1]
        );
        let page = s.list_user_items(false, false, None, None, 2).unwrap();
        let last = page.last().unwrap();
        assert_eq!(
            ids(s
                .list_user_items(
                    false,
                    false,
                    None,
                    Some((last.updated_at, last.torrent_id)),
                    10
                )
                .unwrap()),
            [2, 1]
        );
        assert_eq!(
            s.labels_in_use().unwrap(),
            [
                ("4K".into(), 1),
                ("animated".into(), 1),
                ("anime".into(), 2)
            ]
        );
        assert_eq!(
            s.user_flags().unwrap(),
            [(1, true, false), (2, false, true), (3, true, true)]
        );
        assert_eq!(ids(s.user_items(&[4, 6, 1, 4]).unwrap()), [1, 4]);

        delete_torrent(&s, 1);
        assert_eq!(s.user_item(1).unwrap(), None);
        assert!(s.delete_user_item(2).unwrap());
        assert!(!s.delete_user_item(2).unwrap());
        assert_eq!(
            ids(s.list_user_items(false, false, None, None, 10).unwrap()),
            [4, 3]
        );
    }

    #[test]
    fn corrupt_labels_are_reported() {
        let s = test_store(1);
        s.conn
            .execute(
                "INSERT INTO user_items (torrent_id, favourite, hidden, labels, created_at,
                     updated_at) VALUES (1, 0, 0, 'b' || char(31) || 'a', 0, 0)",
                [],
            )
            .unwrap();
        assert!(matches!(s.user_item(1), Err(Error::Corrupt(_))));
    }

    fn label() -> impl Strategy<Value = String> {
        // Letters, spaces, a separator-adjacent control, and non-ASCII.
        proptest::collection::vec(
            prop_oneof![
                Just('a'),
                Just('B'),
                Just(' '),
                Just('\u{a0}'),
                Just('é'),
                Just('中'),
                Just('\u{1f}'),
                Just('\n'),
                Just('_'),
            ],
            0..70,
        )
        .prop_map(|c| c.into_iter().collect())
    }

    proptest! {
        /// Encoding is canonical: the set, not the order or spelling of
        /// whitespace, decides the text; decoding gives back the
        /// normalized set; and the text never holds an invalid label.
        #[test]
        fn label_encoding_round_trips(labels in proptest::collection::vec(label(), 0..40)) {
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            match encode_labels(&refs) {
                Ok(enc) => {
                    let decoded = decode_labels(enc.as_deref()).unwrap();
                    let mut want: Vec<String> =
                        refs.iter().map(|l| normalize_label(l).unwrap()).collect();
                    want.sort_unstable();
                    want.dedup();
                    prop_assert_eq!(&decoded, &want);
                    let mut rev = refs.clone();
                    rev.reverse();
                    prop_assert_eq!(encode_labels(&rev).unwrap(), enc.clone());
                    let again: Vec<&str> = decoded.iter().map(String::as_str).collect();
                    prop_assert_eq!(encode_labels(&again).unwrap(), enc);
                }
                Err(_) => {
                    let bad_label = refs.iter().any(|l| normalize_label(l).is_err());
                    let too_many = !bad_label && {
                        let mut n: Vec<String> =
                            refs.iter().map(|l| normalize_label(l).unwrap()).collect();
                        n.sort_unstable();
                        n.dedup();
                        n.len() > MAX_LABELS
                    };
                    prop_assert!(bad_label || too_many);
                }
            }
        }
    }
}
