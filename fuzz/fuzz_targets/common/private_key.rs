//! Whether a JSON document uses one of `serde_json`'s private marker keys
//! as an object key (X-insights-source-names, the `fuzz (tpdb)` finding).
//!
//! The fuzz crate builds `serde_json` with `raw_value` (`fuzz/Cargo.toml`).
//! With it, `serde_json::Value` reads an object whose key is the private
//! token `$serde_json::private::RawValue` as a raw value, not as an
//! object, so a differential check against `Value` disagrees with a
//! plugin's parser that reads it, correctly, as an ordinary key. A
//! harness skips only its comparison through `Value` for such a document.
//!
//! Only a key written without escapes counts, because `serde_json`
//! matches the token on the key's raw text: `"\u0024serde_json::…"` is an
//! ordinary key to it, so a harness still compares that document
//! (`fuzz/seeds/tpdb/private_raw_value_key_escaped.json` is the evidence:
//! it never tripped the old harness). The walk reads keys through
//! `deserialize_any`, which `serde_json` never treats specially, and a key
//! counts only when it's handed over borrowed: unchanged from the input.

use serde::de::{Deserializer, MapAccess, SeqAccess, Visitor};

/// The prefix of every `serde_json` private marker key (`RawValue`'s and,
/// under `arbitrary_precision`, `Number`'s).
const PRIVATE: &str = "$serde_json::private::";

/// Whether `root` is JSON with an object key starting [`PRIVATE`]
/// anywhere in it. `false` for anything `serde_json` can't read.
pub fn has_private_key(root: &[u8]) -> bool {
    let mut de = serde_json::Deserializer::from_slice(root);
    de.deserialize_any(Walk).unwrap_or(false)
}

/// Visits one value; `true` if a private key is in it.
struct Walk;

impl<'de> Visitor<'de> for Walk {
    type Value = bool;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<bool, E> {
        Ok(false)
    }
    fn visit_i64<E>(self, _: i64) -> Result<bool, E> {
        Ok(false)
    }
    fn visit_u64<E>(self, _: u64) -> Result<bool, E> {
        Ok(false)
    }
    fn visit_f64<E>(self, _: f64) -> Result<bool, E> {
        Ok(false)
    }
    fn visit_str<E>(self, _: &str) -> Result<bool, E> {
        Ok(false)
    }
    fn visit_unit<E>(self) -> Result<bool, E> {
        Ok(false)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<bool, A::Error> {
        let mut found = false;
        while let Some(f) = seq.next_element_seed(Seed)? {
            found |= f;
        }
        Ok(found)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<bool, A::Error> {
        let mut found = false;
        while let Some(key) = map.next_key_seed(Key)? {
            found |= key;
            found |= map.next_value_seed(Seed)?;
        }
        Ok(found)
    }
}

/// A key: `true` when it's written without escapes and starts [`PRIVATE`].
struct Key;

impl<'de> serde::de::DeserializeSeed<'de> for Key {
    type Value = bool;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<bool, D::Error> {
        d.deserialize_str(Key)
    }
}

impl<'de> Visitor<'de> for Key {
    type Value = bool;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an object key")
    }

    /// Borrowed: the key's own bytes, with no escape in them.
    fn visit_borrowed_str<E>(self, key: &'de str) -> Result<bool, E> {
        Ok(key.starts_with(PRIVATE))
    }

    /// Copied: it had an escape, so it isn't `serde_json`'s token.
    fn visit_str<E>(self, _: &str) -> Result<bool, E> {
        Ok(false)
    }
}

/// [`Walk`] as a seed, for elements and values.
struct Seed;

impl<'de> serde::de::DeserializeSeed<'de> for Seed {
    type Value = bool;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<bool, D::Error> {
        d.deserialize_any(Walk)
    }
}
