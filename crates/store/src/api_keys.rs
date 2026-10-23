//! API keys and feed tokens (architecture §12).
//!
//! The store never sees a secret. The auth code (B12) makes each one from
//! at least 256 random bits, apart from the public prefix (the prefix is
//! shown, so it can't count towards them), shows it once, and hands the
//! store only:
//! - a short public **prefix**, unique, shown in the list to tell keys
//!   apart and used to find the row;
//! - the **SHA-256** of the whole secret.
//!
//! SHA-256 rather than Argon2id: a password needs a slow hash because
//! people pick guessable ones; a 256-bit random secret can't be guessed, so
//! a slow hash adds nothing and would cost every Torznab or RSS request an
//! Argon2 run (19 MiB, tens of milliseconds). A copy of the database
//! leaks no usable key either way.
//!
//! Lookup is by prefix (an index), then a constant-time comparison of the
//! hash, so how long a check takes says nothing about how much of the hash
//! matched. A revoked or expired key fails only after its hash matched:
//! someone who knows only the public prefix learns nothing new.

use rusqlite::{OptionalExtension, params};

use crate::user_data::check_name;
use crate::{Error, Result, Store};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyKind {
    /// Torznab and integrations; its scopes say what it may do.
    ApiKey,
    /// Read-only RSS: one saved search's feed, or every feed.
    FeedToken,
}

impl KeyKind {
    fn code(self) -> i64 {
        match self {
            KeyKind::ApiKey => 0,
            KeyKind::FeedToken => 1,
        }
    }

    fn from_code(code: i64) -> rusqlite::Result<KeyKind> {
        match code {
            0 => Ok(KeyKind::ApiKey),
            1 => Ok(KeyKind::FeedToken),
            _ => Err(rusqlite::Error::IntegralValueOutOfRange(1, code)),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct NewApiKey<'a> {
    pub kind: KeyKind,
    pub name: &'a str,
    /// 8–32 of `A-Z a-z 0-9 _ -`.
    pub prefix: &'a str,
    /// SHA-256 of the whole secret.
    pub hash: [u8; 32],
    /// Bits defined by the auth code; stored as given.
    pub scopes: u32,
    /// A feed token for one saved search; must be `None` for an API key.
    pub saved_search_id: Option<i64>,
    pub expires_at: Option<i64>,
}

/// The hash isn't secret, but it's never printed either: a log line
/// shouldn't carry anything derived from a key (§12.1).
impl std::fmt::Debug for NewApiKey<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewApiKey")
            .field("kind", &self.kind)
            .field("name", &self.name)
            .field("prefix", &self.prefix)
            .field("hash", &"[redacted]")
            .field("scopes", &self.scopes)
            .field("saved_search_id", &self.saved_search_id)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// A key as listed: everything but its hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKey {
    pub id: i64,
    pub kind: KeyKind,
    pub name: String,
    pub prefix: String,
    pub scopes: u32,
    pub saved_search_id: Option<i64>,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub expires_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

/// The outcome of checking a presented secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCheck {
    Valid(ApiKey),
    /// No key has this prefix, or the hash doesn't match.
    Unknown,
    Revoked,
    Expired,
}

/// How often, at most, `last_used_at` is written for one key.
const TOUCH_EVERY: i64 = 60;

const COLUMNS: &str = "id, kind, name, prefix, scopes, saved_search_id, created_at,
    last_used_at, expires_at, revoked_at";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ApiKey> {
    Ok(ApiKey {
        id: r.get(0)?,
        kind: KeyKind::from_code(r.get(1)?)?,
        name: r.get(2)?,
        prefix: r.get(3)?,
        scopes: r.get(4)?,
        saved_search_id: r.get(5)?,
        created_at: r.get(6)?,
        last_used_at: r.get(7)?,
        expires_at: r.get(8)?,
        revoked_at: r.get(9)?,
    })
}

fn prefix_ok(p: &str) -> bool {
    (8..=32).contains(&p.len())
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Compares in time that depends only on the length.
fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

impl Store {
    /// Stores a key's prefix and hash and returns it as listed. A prefix
    /// already in use is [`Error::Invalid`] (the caller makes a new
    /// secret); an unknown saved search is [`Error::NotFound`].
    pub fn create_api_key(&mut self, k: &NewApiKey<'_>, now: i64) -> Result<ApiKey> {
        check_name(k.name)?;
        if !prefix_ok(k.prefix) {
            return Err(Error::Invalid("prefix must be 8-32 of A-Z a-z 0-9 _ -"));
        }
        if k.kind == KeyKind::ApiKey && k.saved_search_id.is_some() {
            return Err(Error::Invalid(
                "only a feed token belongs to a saved search",
            ));
        }
        let tx = crate::begin_write(&mut self.conn)?;
        let taken: bool = tx
            .prepare_cached("SELECT EXISTS (SELECT 1 FROM api_keys WHERE prefix = ?1)")?
            .query_row([k.prefix], |r| r.get(0))?;
        if taken {
            return Err(Error::Invalid("prefix in use"));
        }
        if let Some(s) = k.saved_search_id {
            let known: bool = tx
                .prepare_cached("SELECT EXISTS (SELECT 1 FROM saved_searches WHERE id = ?1)")?
                .query_row([s], |r| r.get(0))?;
            if !known {
                return Err(Error::NotFound("saved search"));
            }
        }
        tx.execute(
            "INSERT INTO api_keys
                 (kind, name, prefix, hash, scopes, saved_search_id, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                k.kind.code(),
                k.name,
                k.prefix,
                &k.hash[..],
                k.scopes,
                k.saved_search_id,
                now,
                k.expires_at
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(self.api_key(id)?.expect("just inserted"))
    }

    pub fn api_key(&self, id: i64) -> Result<Option<ApiKey>> {
        Ok(self
            .conn
            .prepare_cached(&format!("SELECT {COLUMNS} FROM api_keys WHERE id = ?1"))?
            .query_row([id], row)
            .optional()?)
    }

    /// Every key and token, revoked ones included, oldest first.
    pub fn api_keys(&self) -> Result<Vec<ApiKey>> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("SELECT {COLUMNS} FROM api_keys ORDER BY id"))?;
        let rows = stmt.query_map([], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Checks a presented secret, given its prefix and SHA-256. Read-only;
    /// record the use with [`Store::touch_api_key`].
    pub fn verify_api_key(&self, prefix: &str, hash: &[u8; 32], now: i64) -> Result<KeyCheck> {
        if !prefix_ok(prefix) {
            return Ok(KeyCheck::Unknown);
        }
        let found = self
            .conn
            .prepare_cached(&format!(
                "SELECT {COLUMNS}, hash FROM api_keys WHERE prefix = ?1"
            ))?
            .query_row([prefix], |r| Ok((row(r)?, r.get::<_, Vec<u8>>(10)?)))
            .optional()?;
        let Some((key, stored)) = found else {
            return Ok(KeyCheck::Unknown);
        };
        let stored: [u8; 32] = stored
            .try_into()
            .map_err(|_| Error::Corrupt("api key hash is not 32 bytes"))?;
        if !ct_eq(&stored, hash) {
            return Ok(KeyCheck::Unknown);
        }
        Ok(if key.revoked_at.is_some() {
            KeyCheck::Revoked
        } else if key.expires_at.is_some_and(|e| e <= now) {
            KeyCheck::Expired
        } else {
            KeyCheck::Valid(key)
        })
    }

    /// Records that a key was used, at most once a minute per key so a
    /// busy Torznab client doesn't write on every request. Returns whether
    /// it wrote.
    pub fn touch_api_key(&mut self, id: i64, now: i64) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE api_keys SET last_used_at = ?2
             WHERE id = ?1 AND (last_used_at IS NULL OR last_used_at <= ?2 - ?3)",
            params![id, now, TOUCH_EVERY],
        )?;
        Ok(changed > 0)
    }

    /// Revokes a key; it stays listed as revoked. `false` if it doesn't
    /// exist or was revoked already.
    pub fn revoke_api_key(&mut self, id: i64, now: i64) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE api_keys SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
            params![id, now],
        )?;
        Ok(changed > 0)
    }

    /// Deletes a key and its hash.
    pub fn delete_api_key(&mut self, id: i64) -> Result<bool> {
        let deleted = self
            .conn
            .execute("DELETE FROM api_keys WHERE id = ?1", [id])?;
        if deleted > 0 {
            self.after_user_delete();
        }
        Ok(deleted > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SavedSearchInput;
    use crate::user_data::test_store;

    const HASH: [u8; 32] = [0xA5; 32];

    fn key(prefix: &str) -> NewApiKey<'_> {
        NewApiKey {
            kind: KeyKind::ApiKey,
            name: "Prowlarr",
            prefix,
            hash: HASH,
            scopes: 0b101,
            saved_search_id: None,
            expires_at: None,
        }
    }

    #[test]
    fn create_list_and_verify() {
        let mut s = test_store(0);
        let k = s.create_api_key(&key("lsk_a1b2c3d4"), 100).unwrap();
        assert_eq!(
            k,
            ApiKey {
                id: k.id,
                kind: KeyKind::ApiKey,
                name: "Prowlarr".into(),
                prefix: "lsk_a1b2c3d4".into(),
                scopes: 0b101,
                saved_search_id: None,
                created_at: 100,
                last_used_at: None,
                expires_at: None,
                revoked_at: None,
            }
        );
        assert_eq!(s.api_keys().unwrap(), std::slice::from_ref(&k));
        assert_eq!(
            s.verify_api_key("lsk_a1b2c3d4", &HASH, 200).unwrap(),
            KeyCheck::Valid(k.clone())
        );
        let mut wrong = HASH;
        wrong[31] ^= 1;
        assert_eq!(
            s.verify_api_key("lsk_a1b2c3d4", &wrong, 200).unwrap(),
            KeyCheck::Unknown
        );
        assert_eq!(
            s.verify_api_key("lsk_zzzzzzzz", &HASH, 200).unwrap(),
            KeyCheck::Unknown
        );
        for odd in ["", "short", "lsk_a1b2c3d4'--", &"x".repeat(33)] {
            assert_eq!(
                s.verify_api_key(odd, &HASH, 200).unwrap(),
                KeyCheck::Unknown
            );
        }
    }

    /// The row holds the prefix and a 32-byte hash, and no column could
    /// hold anything else secret.
    #[test]
    fn only_the_hash_is_stored() {
        let mut s = test_store(0);
        s.create_api_key(&key("lsk_a1b2c3d4"), 0).unwrap();
        let cols: Vec<String> = s
            .conn
            .prepare("SELECT name FROM pragma_table_info('api_keys') ORDER BY cid")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            cols,
            [
                "id",
                "kind",
                "name",
                "prefix",
                "hash",
                "scopes",
                "saved_search_id",
                "created_at",
                "last_used_at",
                "expires_at",
                "revoked_at"
            ]
        );
        let hash: Vec<u8> = s
            .conn
            .query_row("SELECT hash FROM api_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hash, HASH);
        assert!(
            s.conn
                .execute(
                    "INSERT INTO api_keys (kind, name, prefix, hash, scopes, created_at)
                     VALUES (0, 'x', 'lsk_other000', x'00', 0, 0)",
                    [],
                )
                .is_err(),
            "a hash is 32 bytes"
        );
    }

    #[test]
    fn revoked_and_expired_keys_fail() {
        let mut s = test_store(0);
        let mut exp = key("lsk_expiring");
        exp.expires_at = Some(500);
        let e = s.create_api_key(&exp, 0).unwrap();
        assert!(matches!(
            s.verify_api_key("lsk_expiring", &HASH, 499).unwrap(),
            KeyCheck::Valid(_)
        ));
        assert_eq!(
            s.verify_api_key("lsk_expiring", &HASH, 500).unwrap(),
            KeyCheck::Expired
        );

        let r = s.create_api_key(&key("lsk_revoked0"), 0).unwrap();
        assert!(s.revoke_api_key(r.id, 10).unwrap());
        assert!(!s.revoke_api_key(r.id, 11).unwrap());
        assert_eq!(s.api_key(r.id).unwrap().unwrap().revoked_at, Some(10));
        assert_eq!(
            s.verify_api_key("lsk_revoked0", &HASH, 20).unwrap(),
            KeyCheck::Revoked
        );
        // Without the secret, a revoked key looks like any unknown one.
        assert_eq!(
            s.verify_api_key("lsk_revoked0", &[0; 32], 20).unwrap(),
            KeyCheck::Unknown
        );
        assert!(s.delete_api_key(e.id).unwrap());
        assert!(!s.delete_api_key(e.id).unwrap());
        assert_eq!(
            s.verify_api_key("lsk_expiring", &HASH, 0).unwrap(),
            KeyCheck::Unknown
        );
        let again = s.create_api_key(&key("lsk_expiring"), 0).unwrap();
        assert!(again.id > r.id, "ids are never reused");
    }

    #[test]
    fn touch_writes_at_most_once_a_minute() {
        let mut s = test_store(0);
        let k = s.create_api_key(&key("lsk_a1b2c3d4"), 0).unwrap();
        assert!(s.touch_api_key(k.id, 1000).unwrap());
        assert!(!s.touch_api_key(k.id, 1059).unwrap());
        assert!(s.touch_api_key(k.id, 1060).unwrap());
        assert_eq!(s.api_key(k.id).unwrap().unwrap().last_used_at, Some(1060));
        assert!(!s.touch_api_key(999, 2000).unwrap());
    }

    #[test]
    fn refusals_and_feed_tokens() {
        let mut s = test_store(0);
        s.create_api_key(&key("lsk_a1b2c3d4"), 0).unwrap();
        assert!(matches!(
            s.create_api_key(&key("lsk_a1b2c3d4"), 0),
            Err(Error::Invalid("prefix in use"))
        ));
        for bad in ["lsk_a1b", "lsk_a1b2 c3d4", "lsk_a1b2c3d4é", &"x".repeat(33)] {
            assert!(
                matches!(s.create_api_key(&key(bad), 0), Err(Error::Invalid(_))),
                "{bad:?}"
            );
        }
        let mut named = key("lsk_nameless");
        named.name = "";
        assert!(s.create_api_key(&named, 0).is_err());

        let search = s
            .create_saved_search(
                &SavedSearchInput {
                    name: "a",
                    query: "a",
                    ast: "{}",
                    sort: None,
                    alert: false,
                    rss: true,
                    channels: 0,
                    paused: false,
                },
                0,
                0,
            )
            .unwrap();
        let mut feed = key("lsf_feed0001");
        feed.kind = KeyKind::FeedToken;
        feed.saved_search_id = Some(search + 1);
        assert!(matches!(
            s.create_api_key(&feed, 0),
            Err(Error::NotFound("saved search"))
        ));
        feed.saved_search_id = Some(search);
        let token = s.create_api_key(&feed, 0).unwrap();
        assert_eq!(token.kind, KeyKind::FeedToken);
        assert_eq!(token.saved_search_id, Some(search));
        let mut misplaced = key("lsk_withfeed");
        misplaced.saved_search_id = Some(search);
        assert!(matches!(
            s.create_api_key(&misplaced, 0),
            Err(Error::Invalid(_))
        ));
        let mut all = key("lsf_allfeeds");
        all.kind = KeyKind::FeedToken;
        s.create_api_key(&all, 0).unwrap();

        s.delete_saved_search(search).unwrap();
        assert!(
            s.api_key(token.id).unwrap().is_none(),
            "goes with its search"
        );
        assert_eq!(s.api_keys().unwrap().len(), 2);
    }

    #[test]
    fn debug_hides_the_hash() {
        let shown = format!("{:?}", key("lsk_a1b2c3d4"));
        assert!(shown.contains("[redacted]") && shown.contains("lsk_a1b2c3d4"));
        assert!(!shown.contains("165"), "no hash bytes: {shown}");
    }

    #[test]
    fn constant_time_compare() {
        let a = [7u8; 32];
        let mut b = a;
        assert!(ct_eq(&a, &b));
        for i in 0..32 {
            b = a;
            b[i] ^= 0x80;
            assert!(!ct_eq(&a, &b));
        }
    }
}
